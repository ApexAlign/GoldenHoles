//! Server configuration: CLI flags override environment variables override defaults.
//! `shinobi-server [BIND] [--bind ADDR | --port N] [--max-players N] [--stats-secs S] [--seed N]
//!                 [--level PATH] [--idle-ms N] [--cookie-bucket-ms N] [--bots N] [--teams] [--team-size N] [--telemetry FILE] [--bot-skill easy|normal|hard] [--balance PATH] [--obstacles]`
//! Env: SHINOBI_BIND, SHINOBI_PORT, SHINOBI_MAX_PLAYERS, SHINOBI_STATS_SECS, SHINOBI_SEED, SHINOBI_LEVEL,
//! SHINOBI_IDLE_MS, SHINOBI_COOKIE_BUCKET_MS, SHINOBI_BOTS, SHINOBI_BOT_SKILL, SHINOBI_BALANCE, SHINOBI_OBSTACLES.
//! The effective config is printed at boot.

use std::path::PathBuf;
use std::time::Duration;

pub const USAGE: &str = "usage: shinobi-server [BIND] [--bind ADDR | --port N] [--max-players N] [--stats-secs S] \
[--seed N] [--level PATH] [--idle-ms N] [--cookie-bucket-ms N] [--bots N] [--teams] [--team-size N] [--telemetry FILE] [--bot-skill easy|normal|hard] [--balance PATH] [--obstacles]   \
(env: SHINOBI_BIND, SHINOBI_PORT, SHINOBI_MAX_PLAYERS, SHINOBI_STATS_SECS, SHINOBI_SEED, SHINOBI_LEVEL, SHINOBI_IDLE_MS, \
SHINOBI_COOKIE_BUCKET_MS, SHINOBI_BOTS, SHINOBI_BOT_SKILL, SHINOBI_BALANCE, SHINOBI_OBSTACLES)";

/// Human ids are 1..=200, bot ids 201..=255 (wall owner on the wire is a u8; NPCs start at 0x8000).
pub const MAX_PLAYERS_LIMIT: usize = 200;
/// First bot id.
pub const BOT_ID_BASE: u16 = 201;

/// Server-side AI opponent difficulty (`--bot-skill`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BotSkill {
    Easy,
    Normal,
    Hard,
}

impl BotSkill {
    pub fn parse(v: &str) -> Result<Self, String> {
        match v.trim().to_ascii_lowercase().as_str() {
            "easy" => Ok(Self::Easy),
            "normal" => Ok(Self::Normal),
            "hard" => Ok(Self::Hard),
            other => Err(format!("bot skill: `{other}` is not easy | normal | hard")),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Easy => "easy",
            Self::Normal => "normal",
            Self::Hard => "hard",
        }
    }
}

/// Bots take ids `BOT_ID_BASE..=255`, so they never collide with humans (1..=200); trail `owner` is a u8.
pub const MAX_BOTS: usize = 55;
/// Team deathmatch: first team to this many team kills wins (docs/8v8.md).
pub const TEAM_FRAG_LIMIT: u16 = 50;

#[derive(Clone, PartialEq, Debug)]
pub struct Config {
    pub bind: String,
    pub max_players: usize,
    pub stats_interval: Duration,
    /// Seeds obstacle waves (hex `0x..` or decimal).
    pub seed: u64,
    pub level: Option<PathBuf>,
    pub idle_timeout: Duration,
    /// Handshake cookie lifetime granularity: a cookie is valid for the current and previous bucket.
    pub cookie_bucket: Duration,
    /// AI opponents the server runs itself (they do not occupy human slots).
    pub bots: usize,
    pub bot_skill: BotSkill,
    /// Team deathmatch (Cyan vs Orange, race to `TEAM_FRAG_LIMIT` team kills). Implied by `team_size`.
    pub teams: bool,
    /// Bots keep every team at this many players (humans take bot slots as they join). 0 = no fill.
    pub team_size: usize,
    /// Append playtest telemetry (`TELEMETRY` text + JSON lines) to this file as well as the console.
    pub telemetry: Option<PathBuf>,
    /// Balance tuning file (`key = value` lines); default = demo preset.
    pub balance: Option<PathBuf>,
    /// Moving neon obstacle waves (off by default: the game is a shooter).
    pub obstacles: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bind: "0.0.0.0:7777".into(),
            max_players: 16,
            stats_interval: Duration::from_secs(10),
            seed: 0x5348_494E_4F42_4931,
            level: None,
            idle_timeout: Duration::from_millis(5000),
            cookie_bucket: Duration::from_millis(10_000),
            bots: 0,
            bot_skill: BotSkill::Normal,
            teams: false,
            team_size: 0,
            telemetry: None,
            balance: None,
            obstacles: false,
        }
    }
}

/// `--bind 0.0.0.0` (no port) and bare ports are friendly shorthands: the default port is 7777.
pub const DEFAULT_PORT: u16 = 7777;

fn normalize_bind(b: &str) -> String {
    let b = b.trim();
    if b.chars().all(|c| c.is_ascii_digit()) && !b.is_empty() {
        return format!("0.0.0.0:{b}"); // "9000"
    }
    // a bare IPv4 address / hostname (no ':') or a bracketed IPv6 without port
    if !b.contains(':') || (b.starts_with('[') && b.ends_with(']')) {
        return format!("{b}:{DEFAULT_PORT}");
    }
    b.to_string()
}

fn truthy(v: &str) -> bool {
    matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on")
}

fn num<T: std::str::FromStr>(name: &str, v: &str) -> Result<T, String> {
    v.trim().parse().map_err(|_| format!("{name}: `{v}` is not a valid number"))
}

fn seed(v: &str) -> Result<u64, String> {
    match v.trim().strip_prefix("0x") {
        Some(h) => u64::from_str_radix(h, 16).map_err(|_| format!("seed: `{v}` is not valid hex")),
        None => num("seed", v),
    }
}

impl Config {
    /// `args` excludes argv[0]. `env` looks a variable up (injectable for tests).
    pub fn parse(args: &[String], env: &dyn Fn(&str) -> Option<String>) -> Result<Self, String> {
        let mut c = Self::default();
        // 1. environment
        if let Some(v) = env("SHINOBI_BIND") {
            c.bind = v;
        }
        if let Some(v) = env("SHINOBI_PORT") {
            c.bind = format!("0.0.0.0:{}", num::<u16>("SHINOBI_PORT", &v)?);
        }
        if let Some(v) = env("SHINOBI_MAX_PLAYERS") {
            c.max_players = num("SHINOBI_MAX_PLAYERS", &v)?;
        }
        if let Some(v) = env("SHINOBI_STATS_SECS") {
            c.stats_interval = Duration::from_secs_f64(num::<f64>("SHINOBI_STATS_SECS", &v)?.max(0.001));
        }
        if let Some(v) = env("SHINOBI_SEED") {
            c.seed = seed(&v)?;
        }
        if let Some(v) = env("SHINOBI_LEVEL") {
            c.level = Some(v.into());
        }
        if let Some(v) = env("SHINOBI_IDLE_MS") {
            c.idle_timeout = Duration::from_millis(num("SHINOBI_IDLE_MS", &v)?);
        }
        if let Some(v) = env("SHINOBI_COOKIE_BUCKET_MS") {
            c.cookie_bucket = Duration::from_millis(num("SHINOBI_COOKIE_BUCKET_MS", &v)?);
        }
        if let Some(v) = env("SHINOBI_BOTS") {
            c.bots = num("SHINOBI_BOTS", &v)?;
        }
        if let Some(v) = env("SHINOBI_TELEMETRY") {
            c.telemetry = Some(v.into());
        }
        if let Some(v) = env("SHINOBI_TEAM_SIZE") {
            c.team_size = num("SHINOBI_TEAM_SIZE", &v)?;
        }
        if let Some(v) = env("SHINOBI_TEAMS") {
            c.teams = truthy(&v);
        }
        if let Some(v) = env("SHINOBI_BOT_SKILL") {
            c.bot_skill = BotSkill::parse(&v)?;
        }
        if let Some(v) = env("SHINOBI_BALANCE") {
            c.balance = Some(v.into());
        }
        if let Some(v) = env("SHINOBI_OBSTACLES") {
            c.obstacles = truthy(&v);
        }
        // 2. CLI overrides
        let mut it = args.iter();
        let mut positional = 0;
        while let Some(a) = it.next() {
            let mut val = |flag: &str| it.next().cloned().ok_or_else(|| format!("{flag} needs a value"));
            match a.as_str() {
                "--bind" => c.bind = val("--bind")?,
                "--port" => c.bind = format!("0.0.0.0:{}", num::<u16>("--port", &val("--port")?)?),
                "--max-players" => c.max_players = num("--max-players", &val("--max-players")?)?,
                "--stats-secs" => {
                    c.stats_interval = Duration::from_secs_f64(num::<f64>("--stats-secs", &val("--stats-secs")?)?.max(0.001))
                }
                "--seed" => c.seed = seed(&val("--seed")?)?,
                "--level" => c.level = Some(val("--level")?.into()),
                "--idle-ms" => c.idle_timeout = Duration::from_millis(num("--idle-ms", &val("--idle-ms")?)?),
                "--cookie-bucket-ms" => {
                    c.cookie_bucket = Duration::from_millis(num("--cookie-bucket-ms", &val("--cookie-bucket-ms")?)?)
                }
                "--bots" => c.bots = num("--bots", &val("--bots")?)?,
                "--telemetry" => c.telemetry = Some(val("--telemetry")?.into()),
                "--teams" => c.teams = true,
                "--team-size" => c.team_size = num("--team-size", &val("--team-size")?)?,
                "--obstacles" => c.obstacles = true,
                "--balance" => c.balance = Some(val("--balance")?.into()),
                "--bot-skill" => c.bot_skill = BotSkill::parse(&val("--bot-skill")?)?,
                "-h" | "--help" => return Err(String::new()),
                s if !s.starts_with('-') && positional == 0 => {
                    c.bind = s.to_string(); // back-compat: first positional = bind address
                    positional += 1;
                }
                s => return Err(format!("unknown argument `{s}`")),
            }
        }
        c.bind = normalize_bind(&c.bind);
        c.validate()?;
        Ok(c)
    }

    fn validate(&self) -> Result<(), String> {
        if !(1..=MAX_PLAYERS_LIMIT).contains(&self.max_players) {
            return Err(format!("max players must be 1..={MAX_PLAYERS_LIMIT}, got {}", self.max_players));
        }
        if self.bots > MAX_BOTS {
            return Err(format!("bots must be 0..={MAX_BOTS}, got {}", self.bots));
        }
        if self.team_size > MAX_BOTS / 2 {
            return Err(format!("team size must be 0..={}, got {}", MAX_BOTS / 2, self.team_size));
        }
        if self.team_size > 0 && self.bots > 0 {
            return Err("--team-size fills both teams with bots; use it instead of --bots".into());
        }
        if self.cookie_bucket < Duration::from_millis(50) {
            return Err("cookie bucket must be >= 50 ms".into());
        }
        if self.idle_timeout < Duration::from_millis(100) {
            return Err("idle timeout must be >= 100 ms".into());
        }
        Ok(())
    }

    /// One-line-per-field dump printed at boot.
    pub fn describe(&self) -> String {
        format!(
            "config: bind={} max_players={} stats_every={:.1}s seed={:#x} level={} idle_timeout={}ms cookie_bucket={}ms bots={} ({}) balance={} obstacles={}",
            self.bind,
            self.max_players,
            self.stats_interval.as_secs_f64(),
            self.seed,
            self.level.as_ref().map_or("built-in".to_string(), |p| p.display().to_string()),
            self.idle_timeout.as_millis(),
            self.cookie_bucket.as_millis(),
            self.bots,
            self.bot_skill.as_str(),
            self.balance.as_ref().map_or("demo preset".to_string(), |p| p.display().to_string()),
            if self.obstacles { "on" } else { "off" },
        )
    }
}

/// LAN address of this machine, found by "connecting" a UDP socket (sends nothing).
pub fn lan_ip() -> Option<std::net::IpAddr> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.0.2.1:9").ok()?; // TEST-NET-1: never routed, just picks the outbound interface
    Some(s.local_addr().ok()?.ip())
}

impl Config {
    /// Friendly boot banner: where to connect, who is in, how to stop.
    pub fn banner(&self, level_summary: &str, lan: Option<std::net::IpAddr>) -> String {
        let port = self.bind.rsplit(':').next().unwrap_or("7777");
        let host = self.bind.rsplit_once(':').map_or(self.bind.as_str(), |(h, _)| h);
        let connect = if host == "0.0.0.0" || host == "[::]" {
            match lan {
                Some(ip) => format!("127.0.0.1:{port}  (this machine)   {ip}:{port}  (LAN)"),
                None => format!("127.0.0.1:{port}  (this machine)"),
            }
        } else {
            self.bind.clone()
        };
        let bots = if self.bots == 0 { "none".to_string() } else { format!("{} ({})", self.bots, self.bot_skill.as_str()) };
        let line = "=".repeat(64);
        let game = match (host, lan) {
            ("0.0.0.0" | "[::]", Some(ip)) => format!("shinobi-game --server {ip}:{port}   (other machines on this network)"),
            _ => format!("shinobi-game --server 127.0.0.1:{port}"),
        };
        format!(
            "{line}\n  SHINOBI SERVER   protocol v{}, {} Hz\n  connect : {connect}\n  game    : {game}\n  humans  : up to {}     bots: {bots}\n  level   : {level_summary}\n  stop    : Ctrl-C\n{line}",
            shinobi_proto::datagram::VERSION,
            shinobi_proto::TICK_HZ,
            self.max_players,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str], env: &[(&str, &str)]) -> Result<Config, String> {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        Config::parse(&args, &|k| env.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string()))
    }

    #[test]
    fn defaults_env_cli_precedence() {
        let d = parse(&[], &[]).unwrap();
        assert_eq!((d.bind.as_str(), d.max_players), ("0.0.0.0:7777", 16));
        let e = parse(&[], &[("SHINOBI_PORT", "9000"), ("SHINOBI_MAX_PLAYERS", "8"), ("SHINOBI_SEED", "0xff")]).unwrap();
        assert_eq!((e.bind.as_str(), e.max_players, e.seed), ("0.0.0.0:9000", 8, 255));
        let c = parse(&["--port", "9100", "--seed", "42"], &[("SHINOBI_PORT", "9000"), ("SHINOBI_SEED", "7")]).unwrap();
        assert_eq!((c.bind.as_str(), c.seed), ("0.0.0.0:9100", 42), "CLI beats env");
        // back-compat positional bind (what the integration tests pass)
        assert_eq!(parse(&["127.0.0.1:5555"], &[]).unwrap().bind, "127.0.0.1:5555");
    }

    #[test]
    fn bad_values_are_errors_not_panics() {
        for (args, needle) in [
            (vec!["--max-players", "0"], "max players"),
            (vec!["--max-players", "201"], "max players"),
            (vec!["--port", "70000"], "not a valid number"),
            (vec!["--seed", "0xzz"], "hex"),
            (vec!["--idle-ms"], "needs a value"),
            (vec!["--frobnicate"], "unknown argument"),
            (vec!["--cookie-bucket-ms", "1"], "cookie bucket"),
            (vec!["--bots", "56"], "bots must be"),
            (vec!["--team-size", "28"], "team size must be"),
            (vec!["--team-size", "8", "--bots", "2"], "instead of --bots"),
            (vec!["--bot-skill", "godlike"], "not easy | normal | hard"),
        ] {
            let e = parse(&args, &[]).unwrap_err();
            assert!(e.contains(needle), "{args:?} -> {e}");
        }
        assert_eq!(parse(&["--help"], &[]).unwrap_err(), "");
    }

    #[test]
    fn world_hazards_are_opt_in() {
        let d = parse(&[], &[]).unwrap();
        assert!(!d.obstacles, "shooter: no obstacle waves by default");
        assert!(parse(&["--obstacles"], &[]).unwrap().obstacles);
        assert!(parse(&[], &[("SHINOBI_OBSTACLES", "1")]).unwrap().obstacles);
        assert!(d.describe().contains("obstacles=off"));
    }

    #[test]
    fn bind_shorthands() {
        assert_eq!(parse(&["--bind", "0.0.0.0"], &[]).unwrap().bind, "0.0.0.0:7777");
        assert_eq!(parse(&["--bind", "9000"], &[]).unwrap().bind, "0.0.0.0:9000");
        assert_eq!(parse(&["--bind", "192.168.1.5:8000"], &[]).unwrap().bind, "192.168.1.5:8000");
        assert_eq!(parse(&["--bind", "localhost"], &[]).unwrap().bind, "localhost:7777");
        assert_eq!(parse(&[], &[("SHINOBI_BIND", "0.0.0.0")]).unwrap().bind, "0.0.0.0:7777");
    }

    #[test]
    fn team_flags_and_env() {
        let c = parse(&["--team-size", "8"], &[]).unwrap();
        assert_eq!(c.team_size, 8);
        assert!(parse(&["--teams"], &[]).unwrap().teams);
        assert_eq!(parse(&[], &[("SHINOBI_TEAM_SIZE", "5")]).unwrap().team_size, 5);
    }

    #[test]
    fn bots_flags_and_env() {
        let c = parse(&["127.0.0.1:7777", "--bots", "3", "--bot-skill", "HARD"], &[]).unwrap();
        assert_eq!((c.bind.as_str(), c.bots, c.bot_skill), ("127.0.0.1:7777", 3, BotSkill::Hard), "demo.sh argument order");
        let e = parse(&[], &[("SHINOBI_BOTS", "5"), ("SHINOBI_BOT_SKILL", "easy")]).unwrap();
        assert_eq!((e.bots, e.bot_skill), (5, BotSkill::Easy));
        assert!(c.describe().contains("bots=3 (hard)"));
    }

    #[test]
    fn banner_shows_connect_address_and_bots() {
        let c = parse(&["--bots", "3", "--bot-skill", "easy"], &[]).unwrap();
        let b = c.banner("built-in arena", Some("192.168.1.20".parse().unwrap()));
        assert!(b.contains("127.0.0.1:7777") && b.contains("192.168.1.20:7777"), "{b}");
        assert!(b.contains("bots: 3 (easy)") && b.contains("protocol v24"), "{b}");
        assert!(b.contains("shinobi-game --server 192.168.1.20:7777"), "{b}");
        let local = parse(&["127.0.0.1:9999"], &[]).unwrap().banner("x", None);
        assert!(local.contains("connect : 127.0.0.1:9999") && local.contains("bots: none"), "{local}");
    }

    #[test]
    fn describe_lists_effective_values() {
        let c = parse(&["--level", "maps/a.txt", "--stats-secs", "2.5"], &[]).unwrap();
        let d = c.describe();
        assert!(d.contains("level=maps/a.txt") && d.contains("stats_every=2.5s") && d.contains("max_players=16"), "{d}");
    }
}
