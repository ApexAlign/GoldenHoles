//! Custom characters (`shinobi-character/1`, see docs/CHARACTER_SPEC.md): untrusted JSON in, a small validated
//! `CharacterSpec` out, and a compact quantised wire form (`WIRE_LEN` bytes) that the server re-validates, stores and
//! replicates. Looks only: nothing here touches hitboxes, speed or weapons.
//!
//! Every path (`parse`, `from_wire`) ends in `sanitize`, so a `CharacterSpec` obtained from either is always within
//! the rules: glow value >= 0.6, suit_base value <= 0.35, name `[A-Z0-9 _-]` (upper-cased) <= 16 chars, enums in
//! range, <= 3 distinct accessories, colours quantised to 5-6-5 bits. `sanitize` is idempotent and
//! `from_wire(to_wire(s)) == s` for any sanitised `s`.
//!
//! `CharacterSpec::default()` is "the standard game ninja": the renderer draws the exact stock rig for it.

use crate::bits::{BitReader, BitWriter};
use crate::json::{self, Value};

pub const SCHEMA: &str = "shinobi-character/1";
/// Larger files are refused before parsing (a real one is ~700 bytes).
pub const MAX_JSON_BYTES: usize = 16 * 1024;
pub const WIRE_LEN: usize = 39;
pub type Wire = [u8; WIRE_LEN];

pub const NAME_MAX: usize = 16;
const NAME_CHARS: &[u8; 39] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 _-";

/// Colour slots (index into `CharacterSpec::col`).
pub const C_BASE: usize = 0;
pub const C_TRIM: usize = 1;
pub const C_GLOW: usize = 2;
pub const C_VISOR: usize = 3;
pub const C_GLOVE: usize = 4;
pub const C_SCARF: usize = 5;
pub const C_HAIR: usize = 6;
pub const C_SKIN: usize = 7;
pub const C_EDGE: usize = 8;
pub const C_STAR: usize = 9;
pub const N_COLS: usize = 10;

pub const GLOW_MIN_V: f32 = 0.6;
pub const BASE_MAX_V: f32 = 0.35;

// enum value tables (index = stored value); the first listed name in each is also accepted case-insensitively
const HOOD: &[&str] = &["full", "open", "none"];
const MASK: &[&str] = &["none", "lower", "full", "oni", "fox"];
const HAIR: &[&str] = &["none", "topknot", "ponytail", "spikes", "long"];
const VISOR: &[&str] = &["slit", "dual", "band"];
const PLATES: &[&str] = &["none", "light", "heavy"];
const BACK: &[&str] = &["sheath", "banner", "pack", "none"];
const SCARF: &[&str] = &["none", "short", "long", "tails"];
const PATTERN: &[&str] = &["lines", "hex", "stripes", "none"];
/// Accessory ids are 1-based indices into this list (0 = empty slot).
pub const ACCESSORIES: &[&str] = &["shoulder_fins", "kunai_belt", "horns", "tabi_glow", "earring", "glasses"];
pub const ACC_FINS: u8 = 1;
pub const ACC_KUNAI: u8 = 2;
pub const ACC_HORNS: u8 = 3;
pub const ACC_TABI: u8 = 4;
pub const ACC_EARRING: u8 = 5;
pub const ACC_GLASSES: u8 = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CharError {
    TooBig,
    Syntax,
    WrongSchema,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CharacterSpec {
    /// Upper-case `[A-Z0-9 _-]`, <= 16 chars.
    pub name: String,
    /// Height steps: 0.90 + 0.01 * q, q in 0..=20.
    pub height_q: u8,
    /// Width steps: 0.90 + 0.01 * q, q in 0..=25.
    pub bulk_q: u8,
    /// 5-6-5 packed colours by slot (`C_*`); 0 = unset (stock look / falls back to glow for edge and star).
    pub col: [u16; N_COLS],
    pub hood: u8,
    pub mask: u8,
    pub hair_style: u8,
    pub headband: bool,
    pub visor_style: u8,
    pub plates: u8,
    pub sash: bool,
    pub back_item: u8,
    pub emblem: u8,
    pub scarf_style: u8,
    /// Scarf length steps 0..=7 (length 0.0..1.0).
    pub scarf_len_q: u8,
    pub pattern: u8,
    /// Pattern density steps 0..=7.
    pub density_q: u8,
    pub accessories: [u8; 3],
}

impl Default for CharacterSpec {
    fn default() -> Self {
        Self {
            name: String::new(),
            height_q: 10,
            bulk_q: 10,
            col: [0; N_COLS],
            hood: 0,
            mask: 1,
            hair_style: 0,
            headband: true,
            visor_style: 0,
            plates: 1,
            sash: true,
            back_item: 0,
            emblem: 0,
            scarf_style: 1,
            scarf_len_q: 4,
            pattern: 0,
            density_q: 4,
            accessories: [0; 3],
        }
    }
}

// ---- colours ----

pub fn pack565(rgb: [u8; 3]) -> u16 {
    let q = |v: u8, max: u32| ((v as u32 * max + 127) / 255) as u16;
    (q(rgb[0], 31) << 11) | (q(rgb[1], 63) << 5) | q(rgb[2], 31)
}

pub fn unpack565(v: u16) -> [u8; 3] {
    let (r, g, b) = ((v >> 11) & 31, (v >> 5) & 63, v & 31);
    [((r << 3) | (r >> 2)) as u8, ((g << 2) | (g >> 4)) as u8, ((b << 3) | (b >> 2)) as u8]
}

fn value_of(rgb: [u8; 3]) -> f32 {
    *rgb.iter().max().unwrap() as f32 / 255.0
}

/// Scale `rgb` (keeping its hue) so its brightest channel is `target` (grey if black).
fn scale_to_v(rgb: [u8; 3], target: f32) -> [u8; 3] {
    let m = *rgb.iter().max().unwrap() as f32;
    let t = (target * 255.0).round();
    if m < 1.0 {
        return [t as u8; 3];
    }
    let k = t / m;
    rgb.map(|c| (c as f32 * k).round().clamp(0.0, 255.0) as u8)
}

/// Apply the readability rules to one packed colour (0 stays 0 = unset). Idempotent.
fn sanitize_col(slot: usize, v: u16) -> u16 {
    if v == 0 {
        return 0;
    }
    let ok = |rgb: [u8; 3]| match slot {
        C_BASE => value_of(rgb) <= BASE_MAX_V,
        C_GLOW => value_of(rgb) >= GLOW_MIN_V,
        _ => true,
    };
    let mut rgb = unpack565(v);
    let mut q = v;
    for margin in [0.0f32, 0.02, 0.04, 0.08] {
        if ok(unpack565(q)) {
            break;
        }
        rgb = match slot {
            C_BASE => scale_to_v(rgb, BASE_MAX_V - 0.02 - margin),
            _ => scale_to_v(rgb, GLOW_MIN_V + 0.02 + margin),
        };
        q = pack565(rgb).max(1);
    }
    q.max(1)
}

fn parse_hex_colour(s: &str) -> Option<[u8; 3]> {
    let b = s.as_bytes();
    if b.len() != 7 || b[0] != b'#' || !b[1..].iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    let h = |i: usize| u8::from_str_radix(&s[i..i + 2], 16).ok();
    Some([h(1)?, h(3)?, h(5)?])
}

fn pick(v: Option<&Value>, table: &[&str], default: u8) -> u8 {
    match v.and_then(Value::as_str) {
        Some(s) => table.iter().position(|t| t.eq_ignore_ascii_case(s.trim())).map_or(default, |i| i as u8),
        None => default,
    }
}

fn num_step(v: Option<&Value>, lo: f64, step: f64, max_q: u8, default: u8) -> u8 {
    match v.and_then(Value::as_f64) {
        Some(n) => ((n - lo) / step).round().clamp(0.0, max_q as f64) as u8,
        None => default,
    }
}

fn sanitize_name(raw: &str) -> String {
    raw.chars()
        .map(|c| c.to_ascii_uppercase())
        .filter(|c| c.is_ascii() && NAME_CHARS.contains(&(*c as u8)))
        .take(NAME_MAX)
        .collect::<String>()
        .trim()
        .to_string()
}

impl CharacterSpec {
    /// Parse and validate a character file. Unknown fields are ignored and bad values fall back to the stock look;
    /// only a file that is not JSON, is oversized, or lacks the exact schema string is refused.
    pub fn parse(text: &str) -> Result<Self, CharError> {
        if text.len() > MAX_JSON_BYTES {
            return Err(CharError::TooBig);
        }
        let root = json::parse(text.trim_start_matches('\u{feff}')).map_err(|e| match e {
            json::JsonError::Syntax => CharError::Syntax,
            _ => CharError::TooBig,
        })?;
        if root.get("schema").and_then(Value::as_str) != Some(SCHEMA) {
            return Err(CharError::WrongSchema);
        }
        let d = Self::default();
        let sect = |k: &str| root.get(k);
        let f = |s: &str, k: &str| sect(s).and_then(|v| v.get(k));
        let flag = |s: &str, k: &str, dflt: bool| f(s, k).and_then(Value::as_bool).unwrap_or(dflt);
        let mut me = Self {
            name: root.get("name").and_then(Value::as_str).map(sanitize_name).unwrap_or_default(),
            height_q: num_step(f("build", "height"), 0.90, 0.01, 20, d.height_q),
            bulk_q: num_step(f("build", "bulk"), 0.90, 0.01, 25, d.bulk_q),
            hood: pick(f("head", "hood"), HOOD, d.hood),
            mask: pick(f("head", "mask"), MASK, d.mask),
            hair_style: pick(f("head", "hair_style"), HAIR, d.hair_style),
            headband: flag("head", "headband", d.headband),
            visor_style: pick(f("head", "visor_style"), VISOR, d.visor_style),
            plates: pick(f("torso", "plates"), PLATES, d.plates),
            sash: flag("torso", "sash", d.sash),
            back_item: pick(f("torso", "back_item"), BACK, d.back_item),
            emblem: f("torso", "emblem").and_then(Value::as_f64).map_or(d.emblem, |n| n.round().clamp(0.0, 15.0) as u8),
            scarf_style: pick(f("scarf", "style"), SCARF, d.scarf_style),
            scarf_len_q: num_step(f("scarf", "length"), 0.0, 1.0 / 7.0, 7, d.scarf_len_q),
            pattern: pick(f("pattern", "type"), PATTERN, d.pattern),
            density_q: num_step(f("pattern", "density"), 0.0, 1.0 / 7.0, 7, d.density_q),
            ..d
        };
        const KEYS: [(&str, &str); N_COLS] = [
            ("colors", "suit_base"),
            ("colors", "suit_trim"),
            ("colors", "glow"),
            ("colors", "visor"),
            ("colors", "glove"),
            ("colors", "scarf"),
            ("colors", "hair"),
            ("colors", "skin"),
            ("weapons", "edge"),
            ("weapons", "star"),
        ];
        for (i, (s, k)) in KEYS.iter().enumerate() {
            if let Some(rgb) = f(s, k).and_then(Value::as_str).and_then(parse_hex_colour) {
                me.col[i] = pack565(rgb).max(1);
            }
        }
        if let Some(Value::Arr(a)) = root.get("accessories") {
            let mut n = 0;
            for name in a.iter().filter_map(Value::as_str) {
                if let Some(i) = ACCESSORIES.iter().position(|t| t.eq_ignore_ascii_case(name.trim())) {
                    let id = i as u8 + 1;
                    if !me.accessories[..n].contains(&id) && n < 3 {
                        me.accessories[n] = id;
                        n += 1;
                    }
                }
            }
        }
        me.sanitize();
        Ok(me)
    }

    /// Force every field into range and apply the readability rules. Idempotent.
    pub fn sanitize(&mut self) {
        self.name = sanitize_name(&self.name);
        self.height_q = self.height_q.min(20);
        self.bulk_q = self.bulk_q.min(25);
        for (i, c) in self.col.iter_mut().enumerate() {
            *c = sanitize_col(i, *c);
        }
        let d = Self::default();
        let fix = |v: &mut u8, n: usize, dv: u8| {
            if *v as usize >= n {
                *v = dv
            }
        };
        fix(&mut self.hood, HOOD.len(), d.hood);
        fix(&mut self.mask, MASK.len(), d.mask);
        fix(&mut self.hair_style, HAIR.len(), d.hair_style);
        fix(&mut self.visor_style, VISOR.len(), d.visor_style);
        fix(&mut self.plates, PLATES.len(), d.plates);
        fix(&mut self.back_item, BACK.len(), d.back_item);
        fix(&mut self.scarf_style, SCARF.len(), d.scarf_style);
        fix(&mut self.pattern, PATTERN.len(), d.pattern);
        self.emblem = self.emblem.min(15);
        self.scarf_len_q = self.scarf_len_q.min(7);
        self.density_q = self.density_q.min(7);
        let mut out = [0u8; 3];
        let mut n = 0;
        for a in self.accessories {
            if (1..=ACCESSORIES.len() as u8).contains(&a) && !out[..n].contains(&a) {
                out[n] = a;
                n += 1;
            }
        }
        self.accessories = out;
    }

    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub fn height(&self) -> f32 {
        0.90 + 0.01 * self.height_q as f32
    }
    pub fn bulk(&self) -> f32 {
        0.90 + 0.01 * self.bulk_q as f32
    }
    pub fn scarf_len(&self) -> f32 {
        self.scarf_len_q as f32 / 7.0
    }
    pub fn density(&self) -> f32 {
        self.density_q as f32 / 7.0
    }
    pub fn has_acc(&self, id: u8) -> bool {
        self.accessories.contains(&id)
    }

    /// Colour of a slot as sRGB bytes; `edge` and `star` fall back to glow. `None` = stock look for that slot.
    pub fn rgb(&self, slot: usize) -> Option<[u8; 3]> {
        let mut v = self.col[slot];
        if v == 0 && (slot == C_EDGE || slot == C_STAR) {
            v = self.col[C_GLOW];
        }
        (v != 0).then(|| unpack565(v))
    }

    // ---- wire ----

    pub fn to_wire(&self) -> Wire {
        let mut s = self.clone();
        s.sanitize();
        let mut w = BitWriter::new();
        for c in s.col {
            w.write_bits(c as u32, 16);
        }
        w.write_bits(s.height_q as u32, 5);
        w.write_bits(s.bulk_q as u32, 5);
        w.write_bits(s.hood as u32, 2);
        w.write_bits(s.mask as u32, 3);
        w.write_bits(s.hair_style as u32, 3);
        w.write_bool(s.headband);
        w.write_bits(s.visor_style as u32, 2);
        w.write_bits(s.plates as u32, 2);
        w.write_bool(s.sash);
        w.write_bits(s.back_item as u32, 2);
        w.write_bits(s.emblem as u32, 4);
        w.write_bits(s.scarf_style as u32, 2);
        w.write_bits(s.scarf_len_q as u32, 3);
        w.write_bits(s.pattern as u32, 2);
        w.write_bits(s.density_q as u32, 3);
        for a in s.accessories {
            w.write_bits(a as u32, 3);
        }
        w.write_bits(s.name.len() as u32, 5);
        for b in s.name.bytes() {
            w.write_bits(NAME_CHARS.iter().position(|c| *c == b).unwrap_or(0) as u32, 6);
        }
        let bytes = w.finish();
        let mut out = [0u8; WIRE_LEN];
        out[..bytes.len()].copy_from_slice(&bytes);
        out
    }

    /// Decode and re-validate. Any 39 bytes give a legal spec (garbage fields fall back to the stock look).
    pub fn from_wire(wire: &Wire) -> Self {
        let mut r = BitReader::new(wire);
        let mut me = Self::default();
        let rd = |r: &mut BitReader, n: u32| r.read_bits(n).unwrap_or(0);
        for c in me.col.iter_mut() {
            *c = rd(&mut r, 16) as u16;
        }
        me.height_q = rd(&mut r, 5) as u8;
        me.bulk_q = rd(&mut r, 5) as u8;
        me.hood = rd(&mut r, 2) as u8;
        me.mask = rd(&mut r, 3) as u8;
        me.hair_style = rd(&mut r, 3) as u8;
        me.headband = rd(&mut r, 1) == 1;
        me.visor_style = rd(&mut r, 2) as u8;
        me.plates = rd(&mut r, 2) as u8;
        me.sash = rd(&mut r, 1) == 1;
        me.back_item = rd(&mut r, 2) as u8;
        me.emblem = rd(&mut r, 4) as u8;
        me.scarf_style = rd(&mut r, 2) as u8;
        me.scarf_len_q = rd(&mut r, 3) as u8;
        me.pattern = rd(&mut r, 2) as u8;
        me.density_q = rd(&mut r, 3) as u8;
        for a in me.accessories.iter_mut() {
            *a = rd(&mut r, 3) as u8;
        }
        let n = (rd(&mut r, 5) as usize).min(NAME_MAX);
        let mut name = String::new();
        for _ in 0..n {
            if let Some(c) = NAME_CHARS.get(rd(&mut r, 6) as usize) {
                name.push(*c as char);
            }
        }
        me.name = name;
        me.sanitize();
        me
    }

    /// Re-validate a wire blob from the network: the canonical bytes the server stores and replicates.
    pub fn canonical_wire(wire: &Wire) -> Wire {
        Self::from_wire(wire).to_wire()
    }
}

/// Built-in looks for bots, so the effect shows without files. Cycles through 6 presets by `i`.
pub fn preset(i: usize) -> CharacterSpec {
    let c = |hex: &str| pack565(parse_hex_colour(hex).unwrap());
    let mut s = CharacterSpec::default();
    match i % 6 {
        0 => {
            // fox: open hood, ponytail, magenta glow
            s.col[C_BASE] = c("#16181f");
            s.col[C_GLOW] = c("#ff4fd8");
            s.col[C_VISOR] = c("#7ff0ff");
            s.col[C_SCARF] = c("#c0173a");
            s.col[C_HAIR] = c("#2b1a12");
            s.col[C_SKIN] = c("#e0b494");
            (s.hood, s.mask, s.hair_style, s.scarf_style, s.scarf_len_q, s.emblem) = (1, 1, 2, 3, 6, 5);
            s.accessories = [ACC_KUNAI, ACC_TABI, 0];
        }
        1 => {
            // oni heavy: horns, heavy plates, hex pattern
            s.col[C_BASE] = c("#1a0f12");
            s.col[C_GLOW] = c("#ff5a1f");
            s.col[C_VISOR] = c("#ffd23f");
            (s.mask, s.plates, s.pattern, s.density_q, s.bulk_q, s.height_q) = (3, 2, 1, 6, 20, 14);
            s.back_item = 2;
            s.accessories = [ACC_HORNS, ACC_FINS, 0];
            s.emblem = 9;
        }
        2 => {
            // wisp: slim, long scarf, dual visor, stripes
            s.col[C_BASE] = c("#0d1a24");
            s.col[C_GLOW] = c("#4dffb0");
            s.col[C_SCARF] = c("#1fa37a");
            (s.visor_style, s.scarf_style, s.scarf_len_q, s.pattern, s.bulk_q, s.height_q) = (1, 2, 7, 2, 2, 16);
            s.plates = 0;
            s.back_item = 1;
        }
        3 => {
            // ronin: bare head spikes, glasses
            s.col[C_BASE] = c("#1d1d22");
            s.col[C_GLOW] = c("#ffe14d");
            s.col[C_HAIR] = c("#e8e8f0");
            s.col[C_SKIN] = c("#8a5a3c");
            (s.hood, s.mask, s.hair_style, s.headband) = (2, 0, 3, false);
            s.accessories = [ACC_GLASSES, ACC_EARRING, 0];
            s.sash = false;
            s.emblem = 12;
        }
        4 => {
            // ghost: full mask, band visor, no scarf
            s.col[C_BASE] = c("#101018");
            s.col[C_TRIM] = c("#33336b");
            s.col[C_GLOW] = c("#8a7bff");
            s.col[C_VISOR] = c("#ffffff");
            (s.mask, s.visor_style, s.scarf_style, s.pattern, s.density_q) = (2, 2, 0, 3, 0);
            s.back_item = 3;
        }
        _ => {
            // kabuki: long hair, banner, tails
            s.col[C_BASE] = c("#171117");
            s.col[C_GLOW] = c("#ff2e63");
            s.col[C_HAIR] = c("#101010");
            s.col[C_SKIN] = c("#f1d9c8");
            (s.hood, s.mask, s.hair_style, s.scarf_style, s.back_item) = (1, 0, 4, 3, 1);
            s.density_q = 7;
            s.emblem = 3;
        }
    }
    s.name = ["KITSUNE", "ONI", "WISP", "RONIN", "GHOST", "KABUKI"][i % 6].to_string();
    s.sanitize();
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    const KITSUNE: &str = include_str!("../../../characters/example_kitsune.json");

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
    }

    #[test]
    fn example_parses_as_documented() {
        let s = CharacterSpec::parse(KITSUNE).unwrap();
        assert_eq!(s.name, "KITSUNE");
        assert_eq!((s.height_q, s.bulk_q), (12, 5));
        assert_eq!((s.hood, s.mask, s.hair_style, s.headband, s.visor_style), (1, 1, 2, true, 0));
        assert_eq!((s.plates, s.sash, s.back_item, s.emblem), (1, true, 0, 5));
        assert_eq!((s.scarf_style, s.scarf_len_q, s.pattern, s.density_q), (3, 6, 0, 4));
        assert_eq!(s.accessories, [ACC_KUNAI, ACC_TABI, 0]);
        assert!(value_of(s.rgb(C_GLOW).unwrap()) >= GLOW_MIN_V);
        assert_eq!(s.rgb(C_EDGE), s.rgb(C_GLOW));
        assert!(!s.is_default());
    }

    #[test]
    fn empty_and_unknown_means_stock_look() {
        let s = CharacterSpec::parse(r#"{"schema":"shinobi-character/1","future_field":{"x":1}}"#).unwrap();
        assert!(s.is_default());
        let s = CharacterSpec::parse(r#"{"schema":"shinobi-character/1","head":{"hood":"banana","mask":7,"headband":"yes"},"build":{"height":"tall"}}"#).unwrap();
        assert!(s.is_default(), "wrong types and unknown enum names fall back");
    }

    #[test]
    fn schema_is_required_and_exact() {
        for bad in [r#"{}"#, r#"{"schema":"shinobi-character/2"}"#, r#"{"schema":1}"#, r#"[]"#, "", "not json"] {
            assert!(CharacterSpec::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn clamps_numbers_names_and_colours() {
        let s = CharacterSpec::parse(
            r##"{"schema":"shinobi-character/1","name":"  <script>alert(1)</script> and a very long name ","build":{"height":1e300,"bulk":-5},
            "colors":{"suit_base":"#ffffff","glow":"#101010","visor":"red","skin":"#12345"},"torso":{"emblem":99.7},
            "scarf":{"length":-3},"pattern":{"density":1e9},
            "accessories":["horns","horns","glasses","earring","kunai_belt","nope",5]}"##,
        )
        .unwrap();
        assert!(s.name.len() <= NAME_MAX && s.name.chars().all(|c| NAME_CHARS.contains(&(c as u8))), "{:?}", s.name);
        assert_eq!((s.height_q, s.bulk_q, s.emblem, s.scarf_len_q, s.density_q), (20, 0, 15, 0, 7));
        assert!(value_of(s.rgb(C_BASE).unwrap()) <= BASE_MAX_V, "white suit_base is darkened");
        assert!(value_of(s.rgb(C_GLOW).unwrap()) >= GLOW_MIN_V, "dark glow is lifted");
        assert_eq!((s.col[C_VISOR], s.col[C_SKIN]), (0, 0), "bad colour strings are unset");
        assert_eq!(s.accessories, [ACC_HORNS, ACC_GLASSES, ACC_EARRING], "deduped, max 3");
    }

    #[test]
    fn hostile_inputs_never_panic() {
        let huge = format!(r#"{{"schema":"shinobi-character/1","name":"{}"}}"#, "A".repeat(10 * 1024 * 1024));
        assert_eq!(CharacterSpec::parse(&huge), Err(CharError::TooBig));
        let deep = format!(r#"{{"schema":"shinobi-character/1","a":{}{}}}"#, "[".repeat(50_000), "]".repeat(50_000));
        assert!(CharacterSpec::parse(&deep).is_err());
        for s in [
            r#"{"schema":"shinobi-character/1","build":{"height":NaN}}"#,
            r#"{"schema":"shinobi-character/1","build":{"height":1e999}}"#,
            r#"{"schema":"shinobi-character/1","build":"x","colors":[1],"head":null,"accessories":"horns"}"#,
            "{\"schema\":\"shinobi-character/1\",\"name\":\"\\ud800\\u0000\u{1F600}\"}",
        ] {
            let _ = CharacterSpec::parse(s); // Ok or Err, never a panic
        }
        let s = CharacterSpec::parse(r#"{"schema":"shinobi-character/1","build":{"height":1e300,"bulk":1e308},"torso":{"emblem":-1e300}}"#).unwrap();
        assert_eq!((s.height_q, s.bulk_q, s.emblem), (20, 25, 0));
        // random mutations of the example never panic and always yield a rule-abiding spec
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        for _ in 0..2000 {
            let mut b = KITSUNE.as_bytes().to_vec();
            for _ in 0..1 + rng.next() % 6 {
                let i = rng.next() as usize % b.len();
                b[i] = (rng.next() % 128) as u8;
            }
            if let Ok(t) = String::from_utf8(b) {
                if let Ok(s) = CharacterSpec::parse(&t) {
                    assert_rules(&s);
                }
            }
        }
    }

    fn assert_rules(s: &CharacterSpec) {
        let mut t = s.clone();
        t.sanitize();
        assert_eq!(&t, s, "sanitize idempotent");
        assert!(s.name.len() <= NAME_MAX);
        if let Some(g) = s.rgb(C_GLOW) {
            assert!(value_of(g) >= GLOW_MIN_V, "{g:?}");
        }
        if let Some(b) = s.rgb(C_BASE) {
            assert!(value_of(b) <= BASE_MAX_V, "{b:?}");
        }
        assert_eq!(&CharacterSpec::from_wire(&s.to_wire()), s, "wire round trip");
    }

    #[test]
    fn any_wire_bytes_decode_to_a_legal_canonical_spec() {
        let mut rng = Rng(12345);
        for _ in 0..20_000 {
            let mut w = [0u8; WIRE_LEN];
            for b in w.iter_mut() {
                *b = rng.next() as u8;
            }
            let s = CharacterSpec::from_wire(&w);
            assert_rules(&s);
            let canon = CharacterSpec::canonical_wire(&w);
            assert_eq!(CharacterSpec::canonical_wire(&canon), canon, "canonical form is a fixed point");
        }
    }

    #[test]
    fn colour_quantisation_is_stable_for_all_values() {
        for v in 0u16..=u16::MAX {
            assert_eq!(pack565(unpack565(v)), v, "{v:#x}");
        }
        for slot in [C_BASE, C_GLOW, C_TRIM] {
            let mut rng = Rng(77 + slot as u64);
            for _ in 0..20_000 {
                let v = (rng.next() as u16).max(1);
                let a = sanitize_col(slot, v);
                assert_eq!(sanitize_col(slot, a), a);
                assert!(a != 0);
            }
        }
    }

    #[test]
    fn wire_size_and_round_trip() {
        assert!(WIRE_LEN <= 40);
        for s in (0..6).map(preset).chain([CharacterSpec::parse(KITSUNE).unwrap(), CharacterSpec::default()]) {
            assert_rules(&s);
        }
        let k = CharacterSpec::parse(KITSUNE).unwrap();
        assert_eq!(CharacterSpec::from_wire(&k.to_wire()), k);
        assert!(CharacterSpec::from_wire(&CharacterSpec::default().to_wire()).is_default());
        let mut long = CharacterSpec { name: "ABCDEFGHIJKLMNOP".into(), ..Default::default() };
        long.accessories = [1, 2, 3];
        assert_eq!(CharacterSpec::from_wire(&long.to_wire()), long, "longest name still fits");
    }

    #[test]
    fn presets_differ_and_stay_readable() {
        let all: Vec<_> = (0..6).map(preset).collect();
        for i in 0..6 {
            for j in i + 1..6 {
                assert_ne!(all[i], all[j]);
            }
            assert!(all[i].rgb(C_GLOW).is_some());
        }
        assert_eq!(preset(0), preset(6));
    }
}
