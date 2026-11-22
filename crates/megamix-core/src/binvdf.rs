//! Steam's binary VDF (KeyValues) format, as used by `shortcuts.vdf`.
//!
//! Each entry is a type byte, a NUL-terminated key, then the value:
//! `0x00` nested map (until `0x08`), `0x01` NUL-terminated string,
//! `0x02` int32, `0x03` float32, `0x07` uint64, `0x0A` int64 (little-endian).
//! Strings are kept as raw bytes so files with odd encodings round-trip.

use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Map(Map),
    Str(Vec<u8>),
    Int(i32),
    Float(f32),
    U64(u64),
    I64(i64),
}

/// Ordered key/value pairs (order matters to Steam for the numbered list).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Map(pub Vec<(Vec<u8>, Value)>);

impl Map {
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.0
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key.as_bytes()))
            .map(|(_, v)| v)
    }
    pub fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        self.0
            .iter_mut()
            .find(|(k, _)| k.eq_ignore_ascii_case(key.as_bytes()))
            .map(|(_, v)| v)
    }
    pub fn str(&self, key: &str) -> Option<String> {
        match self.get(key)? {
            Value::Str(b) => Some(String::from_utf8_lossy(b).into_owned()),
            _ => None,
        }
    }
    pub fn int(&self, key: &str) -> Option<i32> {
        match self.get(key)? {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }
    pub fn map(&self, key: &str) -> Option<&Map> {
        match self.get(key)? {
            Value::Map(m) => Some(m),
            _ => None,
        }
    }
    pub fn map_mut(&mut self, key: &str) -> Option<&mut Map> {
        match self.get_mut(key)? {
            Value::Map(m) => Some(m),
            _ => None,
        }
    }
    pub fn push(&mut self, key: &str, v: Value) {
        self.0.push((key.as_bytes().to_vec(), v));
    }
    pub fn set_str(&mut self, key: &str, s: &str) {
        self.push(key, Value::Str(s.as_bytes().to_vec()));
    }
    pub fn set_int(&mut self, key: &str, i: i32) {
        self.push(key, Value::Int(i));
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError(pub String);

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "binary VDF: {}", self.0)
    }
}

impl std::error::Error for ParseError {}

struct Reader<'a> {
    b: &'a [u8],
    i: usize,
}

impl Reader<'_> {
    fn byte(&mut self) -> Result<u8, ParseError> {
        let v = *self
            .b
            .get(self.i)
            .ok_or_else(|| ParseError("file ends too early".into()))?;
        self.i += 1;
        Ok(v)
    }
    fn cstr(&mut self) -> Result<Vec<u8>, ParseError> {
        let rest = &self.b[self.i..];
        let end = rest
            .iter()
            .position(|&c| c == 0)
            .ok_or_else(|| ParseError("unterminated string".into()))?;
        let s = rest[..end].to_vec();
        self.i += end + 1;
        Ok(s)
    }
    fn bytes<const N: usize>(&mut self) -> Result<[u8; N], ParseError> {
        let s = self
            .b
            .get(self.i..self.i + N)
            .ok_or_else(|| ParseError("file ends too early".into()))?;
        self.i += N;
        Ok(s.try_into().unwrap())
    }
    fn map(&mut self, depth: usize) -> Result<Map, ParseError> {
        if depth > 64 {
            return Err(ParseError("nested too deeply".into()));
        }
        let mut m = Map::default();
        loop {
            let t = self.byte()?;
            if t == 0x08 {
                return Ok(m);
            }
            let key = self.cstr()?;
            let v = match t {
                0x00 => Value::Map(self.map(depth + 1)?),
                0x01 => Value::Str(self.cstr()?),
                0x02 => Value::Int(i32::from_le_bytes(self.bytes()?)),
                0x03 => Value::Float(f32::from_le_bytes(self.bytes()?)),
                0x07 => Value::U64(u64::from_le_bytes(self.bytes()?)),
                0x0A => Value::I64(i64::from_le_bytes(self.bytes()?)),
                other => {
                    return Err(ParseError(format!(
                        "unknown value type 0x{other:02x} at byte {}",
                        self.i
                    )));
                }
            };
            m.0.push((key, v));
        }
    }
}

/// Parse a whole file (a top-level map; the file itself ends with `0x08`).
pub fn parse(b: &[u8]) -> Result<Map, ParseError> {
    let mut r = Reader { b, i: 0 };
    let m = r.map(0)?;
    Ok(m)
}

pub fn write(m: &Map) -> Vec<u8> {
    let mut out = Vec::new();
    write_map(m, &mut out);
    out
}

fn write_map(m: &Map, out: &mut Vec<u8>) {
    for (k, v) in &m.0 {
        let t = match v {
            Value::Map(_) => 0x00,
            Value::Str(_) => 0x01,
            Value::Int(_) => 0x02,
            Value::Float(_) => 0x03,
            Value::U64(_) => 0x07,
            Value::I64(_) => 0x0A,
        };
        out.push(t);
        out.extend_from_slice(k);
        out.push(0);
        match v {
            Value::Map(inner) => write_map(inner, out),
            Value::Str(s) => {
                out.extend_from_slice(s);
                out.push(0);
            }
            Value::Int(i) => out.extend_from_slice(&i.to_le_bytes()),
            Value::Float(f) => out.extend_from_slice(&f.to_le_bytes()),
            Value::U64(u) => out.extend_from_slice(&u.to_le_bytes()),
            Value::I64(i) => out.extend_from_slice(&i.to_le_bytes()),
        }
    }
    out.push(0x08);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let mut entry = Map::default();
        entry.set_int("appid", -123456);
        entry.set_str("AppName", "Game ✓");
        entry.push("tags", Value::Map(Map::default()));
        entry.push("LastPlayTime", Value::Int(0));
        entry.push("x", Value::U64(7));
        let mut list = Map::default();
        list.push("0", Value::Map(entry));
        let mut top = Map::default();
        top.push("shortcuts", Value::Map(list));
        let mut bytes = write(&top);
        // Steam's files end with two 0x08 (end of list, end of file).
        assert_eq!(&bytes[bytes.len() - 2..], &[0x08, 0x08]);
        let back = parse(&bytes).unwrap();
        assert_eq!(back, top);
        let s = back.map("shortcuts").unwrap().map("0").unwrap();
        assert_eq!(s.str("appname").as_deref(), Some("Game ✓"));
        assert_eq!(s.int("APPID"), Some(-123456));
        bytes.truncate(bytes.len() - 3);
        assert!(parse(&bytes).is_err());
    }
}
