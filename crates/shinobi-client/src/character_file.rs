//! Loading the player's own character file (`shinobi-character/1`): `--character PATH`, `SHINOBI_CHARACTER`,
//! `/character PATH`, and the remembered path next to the settings file. Everything is validated by
//! `shinobi_proto::character`; this module only does the file I/O (size-capped) and the path bookkeeping.

use std::io::Read;
use std::path::{Path, PathBuf};

use shinobi_proto::character::{CharError, CharacterSpec, MAX_JSON_BYTES};

/// Read and validate a character file. Never reads more than `MAX_JSON_BYTES + 1` bytes.
pub fn load(path: &Path) -> Result<CharacterSpec, String> {
    load_noted(path).map(|(s, _)| s)
}

/// Like `load`, plus one line per thing that was ignored or reinterpreted ("ignored head.x: unknown field").
pub fn load_noted(path: &Path) -> Result<(CharacterSpec, Vec<String>), String> {
    let f = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut buf = Vec::new();
    f.take(MAX_JSON_BYTES as u64 + 1).read_to_end(&mut buf).map_err(|e| format!("{}: {e}", path.display()))?;
    let text = String::from_utf8(buf).map_err(|_| format!("{}: not UTF-8 text", path.display()))?;
    CharacterSpec::parse_noted(&text).map_err(|e| {
        format!(
            "{}: {}",
            path.display(),
            match e {
                CharError::TooBig => "file too big or too deeply nested",
                CharError::Syntax => "not valid JSON",
                CharError::WrongSchema => "not a JSON object with a \"schema\" text, use \"schema\": \"shinobi-character/1.1\"",
            }
        )
    })
}

/// Where the last chosen character path is remembered (beside `settings.cfg`).
pub fn saved_path_file() -> Option<PathBuf> {
    crate::settings::default_path().map(|p| p.with_file_name("character.path"))
}

/// Remember (or with `None` forget) the chosen character file. Failures are ignored: it is a convenience.
pub fn remember(path: Option<&Path>) {
    let Some(f) = saved_path_file() else { return };
    match path {
        Some(p) => {
            if let Some(dir) = f.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let abs = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
            let _ = std::fs::write(&f, abs.to_string_lossy().as_bytes());
        }
        None => {
            let _ = std::fs::remove_file(&f);
        }
    }
}

/// The startup choice: `--character` wins, then `SHINOBI_CHARACTER`, then the remembered path. `Ok(None)` = stock
/// ninja. A path given explicitly that fails to load is reported (and the stock look is used); a stale remembered
/// path is silently dropped.
pub fn startup(cli: Option<String>, env: Option<String>, use_saved: bool) -> (Option<(PathBuf, CharacterSpec)>, Vec<String>) {
    let mut notes = Vec::new();
    for (src, explicit) in [(cli, true), (env, true), (if use_saved { saved_path_file().and_then(|f| std::fs::read_to_string(f).ok()) } else { None }, false)] {
        let Some(p) = src.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) else { continue };
        match load_noted(Path::new(&p)) {
            Ok((spec, warn)) => {
                notes.extend(warn.into_iter().map(|w| format!("character: {w}")));
                return (Some((PathBuf::from(p), spec)), notes);
            }
            Err(e) if explicit => {
                notes.push(format!("character: {e} (using the stock ninja)"));
                return (None, notes);
            }
            Err(_) => {}
        }
    }
    (None, notes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str, body: &[u8]) -> PathBuf {
        let d = std::env::temp_dir().join(format!("shinobi_char_{}_{name}", std::process::id()));
        std::fs::write(&d, body).unwrap();
        d
    }

    #[test]
    fn loads_the_example_and_refuses_junk() {
        let ok = load(Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../characters/example_fox.json"))).unwrap();
        assert_eq!(ok.name, "FOX");
        let bad = tmp("bad", b"{ nope");
        assert!(load(&bad).unwrap_err().contains("not valid JSON"));
        let big = tmp("big", &vec![b' '; 5 * 1024 * 1024]);
        assert!(load(&big).is_err(), "5 MB file refused after reading at most 16 KiB");
        let bin = tmp("bin", &[0xff, 0xfe, 0, 1]);
        assert!(load(&bin).is_err());
        assert!(load(Path::new("/nonexistent/x.json")).is_err());
        for p in [bad, big, bin] {
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    fn cli_beats_env_and_a_bad_explicit_path_falls_back_to_stock() {
        let good = tmp("good", br#"{"schema":"shinobi-character/1","name":"cli"}"#);
        let (r, _) = startup(Some(good.to_string_lossy().into()), Some("/nonexistent".into()), false);
        assert_eq!(r.unwrap().1.name, "CLI");
        let (r, notes) = startup(Some("/nonexistent/x.json".into()), None, false);
        assert!(r.is_none() && notes.len() == 1);
        let _ = std::fs::remove_file(good);
    }
}
