//! `--record OUT.mp4`: offscreen render of the live game, every Nth frame read back from the GPU and piped as raw
//! RGBA to `ffmpeg` (libx264). No window, no winit. The client runs in real time against a live server (its
//! intent clock must track the server's), so frames carry wall-clock timestamps and ffmpeg resamples to a
//! constant frame rate. A bounded queue between the render thread and the encoder drops frames instead of
//! stalling the game if the encoder falls behind.

use std::io::Write;
use std::net::SocketAddr;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, RecvTimeoutError, SyncSender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use bevy::app::ScheduleRunnerPlugin;
use bevy::prelude::*;
use bevy::render::gpu_readback::{Readback, ReadbackComplete};

use crate::demo::{DemoDrive, DemoPlugin};
use crate::net::ViewRes;
use crate::play::ShotImage;
use crate::shot::{base_app, ShotOpts};

pub struct RecordOpts {
    pub out: String,
    pub secs: f32,
    pub fps: u32,
    pub size: (u32, u32),
    pub demo: bool,
}

#[derive(Resource)]
struct Rec {
    cfg: RecordCfg,
    tx: Option<SyncSender<Vec<u8>>>,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
    child: Option<Child>,
    started_at: Option<f32>,
    frame: u64,
    every: u32,
    finish_frames: u32,
    sent: Arc<AtomicU64>,
    dropped: Arc<AtomicU64>,
}

#[derive(Clone)]
struct RecordCfg {
    secs: f32,
    out: String,
    demo: bool,
}

static OK: AtomicBool = AtomicBool::new(false);

fn spawn_ffmpeg(o: &RecordOpts) -> std::io::Result<Child> {
    let log = std::fs::File::create(format!("{}.ffmpeg.log", o.out))?;
    Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "warning", "-y"])
        // frames are stamped with arrival time; the fps filter then makes the output constant-rate
        .args(["-use_wallclock_as_timestamps", "1", "-f", "rawvideo", "-pix_fmt", "rgba"])
        .args(["-s", &format!("{}x{}", o.size.0, o.size.1), "-i", "-"])
        .args(["-vf", &format!("fps={},format=yuv420p", o.fps)])
        .args(["-c:v", "libx264", "-preset", "veryfast", "-crf", "20", "-movflags", "+faststart"])
        .arg(&o.out)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log))
        .spawn()
}

pub fn run(server: SocketAddr, o: RecordOpts, shot: ShotOpts) {
    let mut child = spawn_ffmpeg(&o).expect("start ffmpeg (is /usr/bin/ffmpeg installed?)");
    let mut stdin = child.stdin.take().expect("ffmpeg stdin");
    let (tx, rx) = sync_channel::<Vec<u8>>(4);
    let stop = Arc::new(AtomicBool::new(false));
    let sent = Arc::new(AtomicU64::new(0));
    let (stop2, sent2) = (stop.clone(), sent.clone());
    let join = std::thread::spawn(move || loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(frame) => {
                if stdin.write_all(&frame).is_err() {
                    break; // ffmpeg died; the summary will show it
                }
                sent2.fetch_add(1, Ordering::Relaxed);
            }
            Err(RecvTimeoutError::Timeout) if stop2.load(Ordering::Relaxed) => break,
            Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {}
        }
    });

    let mut app = base_app(server, false, &shot, o.size);
    app.add_plugins(ScheduleRunnerPlugin::run_loop(Duration::from_secs_f64(1.0 / 60.0)));
    if o.demo {
        app.add_plugins(DemoPlugin);
    }
    app.insert_resource(Rec {
        cfg: RecordCfg { secs: o.secs, out: o.out.clone(), demo: o.demo },
        tx: Some(tx),
        stop,
        join: Some(join),
        child: Some(child),
        started_at: None,
        frame: 0,
        every: (60 / o.fps.max(1)).max(1),
        finish_frames: 0,
        sent,
        dropped: Arc::new(AtomicU64::new(0)),
    })
    .add_systems(Update, capture);
    app.run();
    if !OK.load(Ordering::SeqCst) {
        std::process::exit(1);
    }
}

fn capture(
    time: Res<Time>,
    view: Res<ViewRes>,
    img: Option<Res<ShotImage>>,
    demo: Option<Res<DemoDrive>>,
    mut rec: ResMut<Rec>,
    mut commands: Commands,
    mut exit: MessageWriter<AppExit>,
) {
    let (Some(img), t) = (img, time.elapsed_secs()) else { return };
    let rec = &mut *rec;

    if rec.started_at.is_none() {
        if !view.0.connected {
            if t > 20.0 {
                eprintln!("record: never connected to the server");
                rec.stop.store(true, Ordering::SeqCst);
                exit.write(AppExit::Error(std::num::NonZeroU8::new(2).unwrap()));
            }
            return; // clip starts when the game is live, so no "connecting" frames
        }
        rec.started_at = Some(t);
    }
    let rt = t - rec.started_at.unwrap();
    // a demo clip ends 1.5 s after the script finishes (or at --secs); otherwise exactly --secs
    let tail = demo.as_ref().filter(|_| rec.cfg.demo).and_then(|d| d.since_finished(t));
    let done = rt >= rec.cfg.secs || tail.map_or(false, |x| x >= 1.5);

    if !done {
        rec.frame += 1;
        if rec.frame % rec.every as u64 == 0 {
            if let Some(tx) = rec.tx.clone() {
                let dropped = rec.dropped.clone();
                commands.spawn(Readback::texture(img.0.clone())).observe(
                    move |ev: On<ReadbackComplete>, mut commands: Commands| {
                        if tx.try_send(ev.data.clone()).is_err() {
                            dropped.fetch_add(1, Ordering::Relaxed);
                        }
                        commands.entity(ev.entity).despawn();
                    },
                );
            }
        }
        return;
    }

    // stop spawning readbacks, give the in-flight ones a few frames to land, then shut down cleanly
    rec.finish_frames += 1;
    if rec.finish_frames < 20 {
        return;
    }
    rec.tx = None;
    rec.stop.store(true, Ordering::SeqCst);
    if let Some(j) = rec.join.take() {
        let _ = j.join();
    }
    let status = rec.child.take().map(|mut c| c.wait());
    let ok = matches!(&status, Some(Ok(s)) if s.success());
    println!(
        "record: {} frames_encoded={} dropped={} ffmpeg_ok={} clip_secs~{:.1} -> {}",
        if ok { "ok" } else { "FAILED" },
        rec.sent.load(Ordering::Relaxed),
        rec.dropped.load(Ordering::Relaxed),
        ok,
        rt,
        rec.cfg.out
    );
    OK.store(ok && rec.sent.load(Ordering::Relaxed) > 0, Ordering::SeqCst);
    exit.write(AppExit::Success);
}
