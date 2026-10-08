//! Tiny strict JSON reader for untrusted input (character files). std-only. Hard limits: nesting depth, node count,
//! string length. Numbers that overflow f64 are errors. Never panics, never recurses past `MAX_DEPTH`.

pub const MAX_DEPTH: usize = 12;
pub const MAX_NODES: usize = 2_000;
pub const MAX_STR: usize = 512;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Value>),
    Obj(Vec<(String, Value)>),
}

impl Value {
    /// Member of an object (first match), `None` for anything else.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Obj(m) => m.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Num(n) if n.is_finite() => Some(*n),
            _ => None,
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JsonError {
    Syntax,
    TooDeep,
    TooBig,
}

pub fn parse(s: &str) -> Result<Value, JsonError> {
    let mut p = P { b: s.as_bytes(), i: 0, nodes: 0 };
    p.ws();
    let v = p.value(0)?;
    p.ws();
    if p.i != p.b.len() {
        return Err(JsonError::Syntax);
    }
    Ok(v)
}

struct P<'a> {
    b: &'a [u8],
    i: usize,
    nodes: usize,
}

impl P<'_> {
    fn ws(&mut self) {
        while matches!(self.b.get(self.i), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }
    fn eat(&mut self, c: u8) -> Result<(), JsonError> {
        if self.b.get(self.i) == Some(&c) {
            self.i += 1;
            Ok(())
        } else {
            Err(JsonError::Syntax)
        }
    }
    fn lit(&mut self, word: &str, v: Value) -> Result<Value, JsonError> {
        if self.b[self.i..].starts_with(word.as_bytes()) {
            self.i += word.len();
            Ok(v)
        } else {
            Err(JsonError::Syntax)
        }
    }
    fn value(&mut self, depth: usize) -> Result<Value, JsonError> {
        if depth > MAX_DEPTH {
            return Err(JsonError::TooDeep);
        }
        self.nodes += 1;
        if self.nodes > MAX_NODES {
            return Err(JsonError::TooBig);
        }
        match self.b.get(self.i).copied() {
            Some(b'{') => {
                self.i += 1;
                let mut m = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b'}') {
                    self.i += 1;
                    return Ok(Value::Obj(m));
                }
                loop {
                    self.ws();
                    let k = self.string()?;
                    self.ws();
                    self.eat(b':')?;
                    self.ws();
                    let v = self.value(depth + 1)?;
                    m.push((k, v));
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            return Ok(Value::Obj(m));
                        }
                        _ => return Err(JsonError::Syntax),
                    }
                }
            }
            Some(b'[') => {
                self.i += 1;
                let mut a = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b']') {
                    self.i += 1;
                    return Ok(Value::Arr(a));
                }
                loop {
                    self.ws();
                    a.push(self.value(depth + 1)?);
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            return Ok(Value::Arr(a));
                        }
                        _ => return Err(JsonError::Syntax),
                    }
                }
            }
            Some(b'"') => Ok(Value::Str(self.string()?)),
            Some(b't') => self.lit("true", Value::Bool(true)),
            Some(b'f') => self.lit("false", Value::Bool(false)),
            Some(b'n') => self.lit("null", Value::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ => Err(JsonError::Syntax),
        }
    }
    fn number(&mut self) -> Result<Value, JsonError> {
        let start = self.i;
        if self.b.get(self.i) == Some(&b'-') {
            self.i += 1;
        }
        let digits = |p: &mut Self| {
            let s = p.i;
            while matches!(p.b.get(p.i), Some(b'0'..=b'9')) {
                p.i += 1;
            }
            p.i > s
        };
        if !digits(self) {
            return Err(JsonError::Syntax);
        }
        if self.b.get(self.i) == Some(&b'.') {
            self.i += 1;
            if !digits(self) {
                return Err(JsonError::Syntax);
            }
        }
        if matches!(self.b.get(self.i), Some(b'e' | b'E')) {
            self.i += 1;
            if matches!(self.b.get(self.i), Some(b'+' | b'-')) {
                self.i += 1;
            }
            if !digits(self) {
                return Err(JsonError::Syntax);
            }
        }
        let txt = std::str::from_utf8(&self.b[start..self.i]).map_err(|_| JsonError::Syntax)?;
        let n: f64 = txt.parse().map_err(|_| JsonError::Syntax)?;
        if !n.is_finite() {
            return Err(JsonError::Syntax);
        }
        Ok(Value::Num(n))
    }
    fn hex4(&mut self) -> Result<u32, JsonError> {
        let h = self.b.get(self.i..self.i + 4).ok_or(JsonError::Syntax)?;
        let s = std::str::from_utf8(h).map_err(|_| JsonError::Syntax)?;
        self.i += 4;
        u32::from_str_radix(s, 16).map_err(|_| JsonError::Syntax)
    }
    fn string(&mut self) -> Result<String, JsonError> {
        self.eat(b'"')?;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let c = *self.b.get(self.i).ok_or(JsonError::Syntax)?;
            self.i += 1;
            match c {
                b'"' => break,
                b'\\' => {
                    let e = *self.b.get(self.i).ok_or(JsonError::Syntax)?;
                    self.i += 1;
                    let ch = match e {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let mut cp = self.hex4()?;
                            if (0xD800..0xDC00).contains(&cp) && self.b[self.i..].starts_with(b"\\u") {
                                self.i += 2;
                                let lo = self.hex4()?;
                                cp = if (0xDC00..0xE000).contains(&lo) { 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00) } else { 0xFFFD };
                            }
                            char::from_u32(cp).unwrap_or('\u{FFFD}')
                        }
                        _ => return Err(JsonError::Syntax),
                    };
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                0..=0x1f => return Err(JsonError::Syntax),
                _ => out.push(c),
            }
            if out.len() > MAX_STR {
                return Err(JsonError::TooBig);
            }
        }
        String::from_utf8(out).map_err(|_| JsonError::Syntax)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basics_and_rejects_garbage() {
        let v = parse(r#" {"a": [1, 2.5e1, -3], "b": {"c": "x\u00e9\n"}, "t": true, "n": null} "#).unwrap();
        assert_eq!(v.get("a"), Some(&Value::Arr(vec![Value::Num(1.0), Value::Num(25.0), Value::Num(-3.0)])));
        assert_eq!(v.get("b").and_then(|b| b.get("c")).and_then(Value::as_str), Some("x\u{e9}\n"));
        for bad in ["", "{", "[1,]", "{\"a\":}", "01x", "\"\u{1}\"", "1e999", "{} x", "nul", "\"\\x\"", "[1 2]", "--1"] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn deep_and_wide_inputs_are_bounded() {
        let deep = "[".repeat(100_000) + &"]".repeat(100_000);
        assert_eq!(parse(&deep), Err(JsonError::TooDeep));
        let wide = format!("[{}]", "1,".repeat(MAX_NODES + 5) + "1");
        assert_eq!(parse(&wide), Err(JsonError::TooBig));
        let long = format!("\"{}\"", "a".repeat(MAX_STR + 10));
        assert_eq!(parse(&long), Err(JsonError::TooBig));
    }
}
