//! Custom characters (`shinobi-character/1` and `/1.1`, see docs/CHARACTER_SPEC.md): untrusted JSON in, a small validated
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

pub const SCHEMA: &str = "shinobi-character/1.1";
/// Schema strings with this prefix are the real thing; any other string is accepted with a warning (see `parse_noted`).
pub const SCHEMA_PREFIX: &str = "shinobi-character/";
/// Larger files are refused before parsing (a real one is ~700 bytes).
pub const MAX_JSON_BYTES: usize = 16 * 1024;
pub const WIRE_LEN: usize = 41;
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
const VISOR: &[&str] = &["slit", "dual", "band", "cat", "round", "angry"];
const EARS: &[&str] = &["none", "pointy", "big", "long", "round", "cat"];
const EXPR: &[&str] = &["neutral", "smile", "grin", "angry", "surprised", "sleepy"];
const HORNS: &[&str] = &["none", "small", "oni", "antlers"];
const NOSE: &[&str] = &["none", "big", "snout"];
const TAIL: &[&str] = &["none", "fox", "cat", "lizard"];
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
    /// v1.1: indices into `EARS`, `EXPR`, `HORNS`, `NOSE`, `TAIL`.
    pub ears: u8,
    pub expression: u8,
    pub horns: u8,
    pub nose: u8,
    pub tail: u8,
    /// Shoulder width steps: 0.90 + 0.02 * q, q in 0..=15 (1.0 = 5).
    pub shoulders_q: u8,
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
            ears: 0,
            expression: 0,
            horns: 0,
            nose: 0,
            tail: 0,
            shoulders_q: 5,
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

/// Parse-time reader over the root object: every lookup treats `null` as absent and records what it had to ignore.
struct Reader<'a> {
    root: &'a Value,
    notes: Vec<String>,
}

fn short(s: &str) -> String {
    s.chars().take(24).collect()
}

impl Reader<'_> {
    fn f(&self, s: &str, k: &str) -> Option<&Value> {
        self.root.get(s).and_then(|v| v.get(k)).filter(|v| !matches!(v, Value::Null))
    }
    fn note(&mut self, s: String) {
        if self.notes.len() < 40 {
            self.notes.push(s);
        }
    }
    /// Enum field: exact name, else the first alias contained in the text, else the default (with a note).
    fn pick(&mut self, s: &str, k: &str, table: &[&str], aliases: &[(&str, u8)], default: u8) -> u8 {
        let Some(v) = self.f(s, k) else { return default };
        let Some(raw) = v.as_str() else {
            self.note(format!("{s}.{k}: expected text, using default"));
            return default;
        };
        let n = raw.trim().to_ascii_lowercase().replace(['-', ' '], "_");
        if let Some(i) = table.iter().position(|t| *t == n) {
            return i as u8;
        }
        match aliases.iter().find(|(key, _)| n.contains(key)) {
            Some((_, i)) => {
                self.note(format!("{s}.{k}: \"{}\" is not a known value, using \"{}\"", short(raw), table[*i as usize]));
                *i
            }
            None => {
                self.note(format!("{s}.{k}: unknown value \"{}\", using default", short(raw)));
                default
            }
        }
    }
    fn step(&mut self, s: &str, k: &str, lo: f64, step: f64, max_q: u8, default: u8) -> u8 {
        match self.f(s, k) {
            None => default,
            Some(v) => match v.as_f64() {
                Some(n) => ((n - lo) / step).round().clamp(0.0, max_q as f64) as u8,
                None => {
                    self.note(format!("{s}.{k}: expected a number, using default"));
                    default
                }
            },
        }
    }
    fn flag(&mut self, s: &str, k: &str, default: bool) -> bool {
        match self.f(s, k) {
            None => default,
            Some(v) => match v.as_bool() {
                Some(b) => b,
                None => {
                    self.note(format!("{s}.{k}: expected true or false, using default"));
                    default
                }
            },
        }
    }
}

/// Known `section.key` pairs (v1.1); anything else is reported as ignored.
const KNOWN: &[(&str, &[&str])] = &[
    ("build", &["height", "bulk", "shoulders"]),
    ("colors", &["suit_base", "suit_trim", "glow", "visor", "glove", "scarf", "hair", "skin"]),
    ("head", &["hood", "mask", "hair_style", "headband", "visor_style", "ears", "expression", "horns", "nose"]),
    ("torso", &["plates", "sash", "back_item", "emblem", "tail"]),
    ("scarf", &["style", "length"]),
    ("pattern", &["type", "density"]),
    ("weapons", &["edge", "star"]),
];

fn unknown_fields(root: &Value, notes: &mut Vec<String>) {
    let Value::Obj(top) = root else { return };
    for (k, v) in top {
        if notes.len() >= 40 {
            return;
        }
        match KNOWN.iter().find(|(s, _)| *s == k) {
            Some((_, keys)) => {
                if let Value::Obj(inner) = v {
                    for (ik, _) in inner.iter().filter(|(ik, iv)| !keys.contains(&ik.as_str()) && !matches!(iv, Value::Null)) {
                        notes.push(format!("ignored {k}.{}: unknown field", short(ik)));
                    }
                }
            }
            None if matches!(k.as_str(), "schema" | "name" | "accessories") => {}
            None => notes.push(format!("ignored {}: unknown field", short(k))),
        }
    }
}

// Words that map a free-form value to the nearest known one (checked in order, substring match).
const A_VISOR: &[(&str, u8)] = &[("cat", 3), ("feline", 3), ("round", 4), ("circle", 4), ("big", 4), ("angry", 5), ("mean", 5), ("evil", 5), ("dual", 1), ("two", 1), ("band", 2), ("visor", 2), ("slit", 0)];
const A_EARS: &[(&str, u8)] = &[("cat", 5), ("kitten", 5), ("feline", 5), ("long", 3), ("rabbit", 3), ("bunny", 3), ("hare", 3), ("point", 1), ("elf", 1), ("fox", 1), ("shrimp", 2), ("big", 2), ("large", 2), ("huge", 2), ("flap", 2), ("ear", 2), ("round", 4), ("bear", 4), ("mouse", 4), ("small", 4)];
const A_EXPR: &[(&str, u8)] = &[("grin", 2), ("smile", 1), ("goof", 2), ("tooth", 2), ("happy", 1), ("friendly", 1), ("angry", 3), ("mad", 3), ("scowl", 3), ("frown", 3), ("fierce", 3), ("surpris", 4), ("shock", 4), ("sleep", 5), ("tired", 5), ("bored", 5), ("calm", 0), ("neutral", 0)];
const A_HORNS: &[(&str, u8)] = &[("antler", 3), ("deer", 3), ("moose", 3), ("small", 1), ("nub", 1), ("tiny", 1), ("oni", 2), ("big", 2), ("large", 2), ("horn", 2), ("demon", 2)];
const A_NOSE: &[(&str, u8)] = &[("snout", 2), ("muzzle", 2), ("pig", 2), ("dog", 2), ("fox", 2), ("wolf", 2), ("big", 1), ("large", 1), ("bulb", 1), ("round", 1), ("nose", 1)];
const A_TAIL: &[(&str, u8)] = &[("fox", 1), ("fluff", 1), ("cat", 2), ("feline", 2), ("lizard", 3), ("dragon", 3), ("reptile", 3), ("croc", 3)];

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
    /// Parse and validate a character file (warnings are dropped, see `parse_noted`).
    pub fn parse(text: &str) -> Result<Self, CharError> {
        Self::parse_noted(text).map(|(s, _)| s)
    }

    /// Parse and validate, also returning one human-readable line for everything that was ignored or reinterpreted.
    /// Unknown fields are ignored, `null` counts as absent, unknown enum names fall back to the nearest known value
    /// or the default. Only a file that is not JSON, is oversized, is not an object or has no `schema` string is
    /// refused. A `schema` other than `shinobi-character/...` is accepted with a warning: the string is a courtesy,
    /// every field is validated anyway.
    pub fn parse_noted(text: &str) -> Result<(Self, Vec<String>), CharError> {
        if text.len() > MAX_JSON_BYTES {
            return Err(CharError::TooBig);
        }
        let root = json::parse(text.trim_start_matches('\u{feff}')).map_err(|e| match e {
            json::JsonError::Syntax => CharError::Syntax,
            _ => CharError::TooBig,
        })?;
        if !matches!(root, Value::Obj(_)) {
            return Err(CharError::WrongSchema);
        }
        let mut notes = Vec::new();
        match root.get("schema").and_then(Value::as_str) {
            Some(s) if s == "shinobi-character/1" || s == SCHEMA => {}
            Some(s) if s.starts_with(SCHEMA_PREFIX) => notes.push(format!("schema \"{}\": unknown version, reading it as {SCHEMA}", short(s))),
            Some(s) => notes.push(format!("schema \"{}\" is not \"{SCHEMA}\": read it anyway, fields are checked one by one", short(s))),
            None => return Err(CharError::WrongSchema),
        }
        unknown_fields(&root, &mut notes);
        let d = Self::default();
        let mut r = Reader { root: &root, notes };
        let mut me = Self {
            name: root.get("name").and_then(Value::as_str).map(sanitize_name).unwrap_or_default(),
            height_q: r.step("build", "height", 0.90, 0.01, 20, d.height_q),
            bulk_q: r.step("build", "bulk", 0.90, 0.01, 25, d.bulk_q),
            shoulders_q: r.step("build", "shoulders", 0.90, 0.02, 15, d.shoulders_q),
            hood: r.pick("head", "hood", HOOD, &[("full", 0), ("open", 1), ("none", 2), ("bare", 2)], d.hood),
            mask: r.pick("head", "mask", MASK, &[("oni", 3), ("fox", 4), ("full", 2), ("lower", 1), ("none", 0)], d.mask),
            hair_style: r.pick("head", "hair_style", HAIR, &[("topknot", 1), ("bun", 1), ("pony", 2), ("spike", 3), ("long", 4), ("bald", 0)], d.hair_style),
            headband: r.flag("head", "headband", d.headband),
            visor_style: r.pick("head", "visor_style", VISOR, A_VISOR, d.visor_style),
            ears: r.pick("head", "ears", EARS, A_EARS, d.ears),
            expression: r.pick("head", "expression", EXPR, A_EXPR, d.expression),
            horns: r.pick("head", "horns", HORNS, A_HORNS, d.horns),
            nose: r.pick("head", "nose", NOSE, A_NOSE, d.nose),
            plates: r.pick("torso", "plates", PLATES, &[("heavy", 2), ("light", 1)], d.plates),
            sash: r.flag("torso", "sash", d.sash),
            back_item: r.pick("torso", "back_item", BACK, &[("sheath", 0), ("sword", 0), ("banner", 1), ("pack", 2)], d.back_item),
            tail: r.pick("torso", "tail", TAIL, A_TAIL, d.tail),
            emblem: match r.f("torso", "emblem") {
                None => d.emblem,
                Some(v) => v.as_f64().map_or(d.emblem, |n| n.round().clamp(0.0, 15.0) as u8),
            },
            scarf_style: r.pick("scarf", "style", SCARF, &[("tail", 3), ("long", 2), ("short", 1)], d.scarf_style),
            scarf_len_q: r.step("scarf", "length", 0.0, 1.0 / 7.0, 7, d.scarf_len_q),
            pattern: r.pick("pattern", "type", PATTERN, &[("hex", 1), ("stripe", 2), ("line", 0)], d.pattern),
            density_q: r.step("pattern", "density", 0.0, 1.0 / 7.0, 7, d.density_q),
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
            let Some(v) = r.f(s, k) else { continue };
            match v.as_str().and_then(parse_hex_colour) {
                Some(rgb) => me.col[i] = pack565(rgb).max(1),
                None => r.note(format!("{s}.{k}: not a \"#RRGGBB\" colour, left unset")),
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
                } else {
                    r.note(format!("accessories: unknown \"{}\", ignored", short(name)));
                }
            }
        }
        let notes = r.notes;
        me.sanitize();
        Ok((me, notes))
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
        fix(&mut self.ears, EARS.len(), d.ears);
        fix(&mut self.expression, EXPR.len(), d.expression);
        fix(&mut self.horns, HORNS.len(), d.horns);
        fix(&mut self.nose, NOSE.len(), d.nose);
        fix(&mut self.tail, TAIL.len(), d.tail);
        self.shoulders_q = self.shoulders_q.min(15);
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
    pub fn shoulders(&self) -> f32 {
        0.90 + 0.02 * self.shoulders_q as f32
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
        w.write_bits(s.visor_style as u32, 3);
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
        w.write_bits(s.ears as u32, 3);
        w.write_bits(s.expression as u32, 3);
        w.write_bits(s.horns as u32, 2);
        w.write_bits(s.nose as u32, 2);
        w.write_bits(s.tail as u32, 2);
        w.write_bits(s.shoulders_q as u32, 4);
        w.write_bits(s.name.len() as u32, 5);
        for b in s.name.bytes() {
            w.write_bits(NAME_CHARS.iter().position(|c| *c == b).unwrap_or(0) as u32, 6);
        }
        let bytes = w.finish();
        let mut out = [0u8; WIRE_LEN];
        out[..bytes.len()].copy_from_slice(&bytes);
        out
    }

    /// Decode and re-validate. Any `WIRE_LEN` bytes give a legal spec (garbage fields fall back to the stock look).
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
        me.visor_style = rd(&mut r, 3) as u8;
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
        me.ears = rd(&mut r, 3) as u8;
        me.expression = rd(&mut r, 3) as u8;
        me.horns = rd(&mut r, 2) as u8;
        me.nose = rd(&mut r, 2) as u8;
        me.tail = rd(&mut r, 2) as u8;
        me.shoulders_q = rd(&mut r, 4) as u8;
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
    fn schema_must_be_a_string_in_an_object() {
        for bad in [r#"{}"#, r#"{"schema":1}"#, r#"{"schema":null}"#, r#"[]"#, "", "not json"] {
            assert!(CharacterSpec::parse(bad).is_err(), "{bad}");
        }
        // other strings are only a warning: the schema is a courtesy, fields are validated anyway
        let (_, n) = CharacterSpec::parse_noted(r#"{"schema":"shinobi-character/2"}"#).unwrap();
        assert!(n.iter().any(|l| l.contains("unknown version")), "{n:?}");
        for ok in ["shinobi-character/1", "shinobi-character/1.1"] {
            let (s, n) = CharacterSpec::parse_noted(&format!(r#"{{"schema":"{ok}"}}"#)).unwrap();
            assert!(s.is_default() && n.is_empty(), "{ok}: {n:?}");
        }
    }

    /// A chatbot answered with its own schema name and fields v1 did not have (reconstructed from the report:
    /// `head.expression`, `head.ears`, `head.visor_style: "big_cat_eyes"`, `colors.hair: null`).
    const OGRE_CHATBOT: &str = r##"{
  "schema": "ogre-character/1",
  "name": "Ogre",
  "build": { "height": 1.05, "bulk": 1.15 },
  "colors": {
    "suit_base": "#2b2b1f", "suit_trim": "#4a5a2a", "glow": "#7bff3a", "visor": "#ffe14d",
    "glove": "#5a8a3a", "scarf": "#8a3a1a", "hair": null, "skin": "#6fbf3a"
  },
  "head": { "hood": "none", "mask": "none", "hair_style": "none", "headband": false,
            "visor_style": "big_cat_eyes", "ears": "big_shrimp", "expression": "goofy_smile" },
  "torso": { "plates": "heavy", "sash": true, "back_item": "pack", "emblem": 0 },
  "scarf": { "style": "short", "length": 0.4 },
  "pattern": { "type": "hex", "density": 0.5 },
  "weapons": { "edge": "#7bff3a", "star": "#ffe14d" },
  "accessories": ["shoulder_fins"]
}"##;

    #[test]
    fn chatbot_file_with_wrong_schema_and_odd_values_loads_with_warnings() {
        let (s, notes) = CharacterSpec::parse_noted(OGRE_CHATBOT).unwrap();
        let has = |w: &str| notes.iter().any(|n| n.contains(w));
        assert!(has("ogre-character/1") && has("head.visor_style") && has("head.ears"), "{notes:?}");
        assert!(has("head.expression") && has("goofy_smile"), "{notes:?}");
        assert!(!has("colors.hair"), "null is silently absent: {notes:?}");
        assert_eq!(s.rgb(C_SKIN).map(|c| c[1] > c[0]), Some(true), "green skin applied");
        assert_eq!(s.rgb(C_VISOR).map(|c| c[0] > 200), Some(true), "yellow visor applied");
        assert!(s.rgb(C_SCARF).is_some() && s.col[C_HAIR] == 0);
        assert_eq!(s.plates, 2);
        assert_eq!((s.visor_style, s.ears, s.expression), (3, 2, 1), "cat visor, big ears, smile");
        assert_eq!((s.hood, s.mask, s.scarf_style), (2, 0, 1));
        assert_rules(&s);
    }

    #[test]
    fn unknown_fields_are_named_and_enums_fall_back() {
        let (s, n) = CharacterSpec::parse_noted(
            r#"{"schema":"shinobi-character/1.1","wings":{"x":1},"head":{"gills":"yes","nose":"zzz","horns":"giant_antlers","ears":null}}"#,
        )
        .unwrap();
        assert!(n.contains(&"ignored wings: unknown field".to_string()), "{n:?}");
        assert!(n.contains(&"ignored head.gills: unknown field".to_string()), "{n:?}");
        assert_eq!((s.nose, s.horns, s.ears), (0, 3, 0));
        assert!(n.iter().any(|l| l.starts_with("head.nose:")), "{n:?}");
    }

    #[test]
    fn v1_files_read_identically_under_1_1() {
        let s = CharacterSpec::parse(KITSUNE).unwrap();
        assert_eq!((s.ears, s.expression, s.horns, s.nose, s.tail, s.shoulders_q), (0, 0, 0, 0, 0, 5));
        let (_, n) = CharacterSpec::parse_noted(KITSUNE).unwrap();
        assert!(n.is_empty(), "{n:?}");
    }

    #[test]
    fn v1_1_examples_parse_clean_and_roundtrip() {
        for (t, name) in [(include_str!("../../../characters/example_ogre.json"), "OGRE"), (include_str!("../../../characters/example_fox.json"), "FOX")] {
            let (s, n) = CharacterSpec::parse_noted(t).unwrap();
            assert!(n.is_empty() && s.name == name, "{n:?}");
            assert_rules(&s);
        }
        let o = CharacterSpec::parse(include_str!("../../../characters/example_ogre.json")).unwrap();
        assert_eq!((o.ears, o.expression, o.visor_style, o.horns, o.nose, o.shoulders_q), (2, 2, 3, 1, 1, 15));
        let f = CharacterSpec::parse(include_str!("../../../characters/example_fox.json")).unwrap();
        assert_eq!((f.tail, f.ears, f.nose, f.visor_style), (1, 1, 2, 5));
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
        assert!(WIRE_LEN <= 41);
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
