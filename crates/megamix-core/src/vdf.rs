//! Minimal parser for Valve's text KeyValues format (`libraryfolders.vdf`,
//! `appmanifest_*.acf`). Read-only; we never write these files.

/// A KeyValues node: either a string or an ordered list of children.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Vdf {
    Str(String),
    Obj(Vec<(String, Vdf)>),
}

impl Vdf {
    /// First child with this key (case-insensitive, as Steam treats keys).
    pub fn get(&self, key: &str) -> Option<&Vdf> {
        match self {
            Vdf::Obj(items) => items
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(key))
                .map(|(_, v)| v),
            Vdf::Str(_) => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Vdf::Str(s) => Some(s),
            Vdf::Obj(_) => None,
        }
    }

    pub fn entries(&self) -> &[(String, Vdf)] {
        match self {
            Vdf::Obj(items) => items,
            Vdf::Str(_) => &[],
        }
    }

    /// Follow a path of keys.
    pub fn path(&self, keys: &[&str]) -> Option<&Vdf> {
        keys.iter().try_fold(self, |node, k| node.get(k))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("VDF parse error at byte {pos}: {msg}")]
pub struct VdfError {
    pub pos: usize,
    pub msg: &'static str,
}

/// Parse a whole document. The result is an `Obj` holding the top-level
/// key(s), e.g. `"libraryfolders" { ... }`.
pub fn parse(text: &str) -> Result<Vdf, VdfError> {
    let mut p = Parser {
        s: text.strip_prefix('\u{feff}').unwrap_or(text).as_bytes(),
        i: 0,
    };
    let items = p.object_body(false)?;
    Ok(Vdf::Obj(items))
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

enum Tok {
    Str(String),
    Open,
    Close,
    Eof,
}

impl Parser<'_> {
    fn err(&self, msg: &'static str) -> VdfError {
        VdfError { pos: self.i, msg }
    }

    fn skip_ws(&mut self) {
        loop {
            while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
                self.i += 1;
            }
            if self.s[self.i..].starts_with(b"//") {
                while self.i < self.s.len() && self.s[self.i] != b'\n' {
                    self.i += 1;
                }
            } else {
                return;
            }
        }
    }

    fn token(&mut self) -> Result<Tok, VdfError> {
        self.skip_ws();
        let Some(&c) = self.s.get(self.i) else {
            return Ok(Tok::Eof);
        };
        match c {
            b'{' => {
                self.i += 1;
                Ok(Tok::Open)
            }
            b'}' => {
                self.i += 1;
                Ok(Tok::Close)
            }
            b'"' => {
                self.i += 1;
                let mut out = Vec::new();
                loop {
                    let Some(&c) = self.s.get(self.i) else {
                        return Err(self.err("unterminated string"));
                    };
                    self.i += 1;
                    match c {
                        b'"' => break,
                        b'\\' => {
                            let Some(&e) = self.s.get(self.i) else {
                                return Err(self.err("unterminated escape"));
                            };
                            self.i += 1;
                            match e {
                                b'n' => out.push(b'\n'),
                                b't' => out.push(b'\t'),
                                b'\\' => out.push(b'\\'),
                                b'"' => out.push(b'"'),
                                other => {
                                    out.push(b'\\');
                                    out.push(other);
                                }
                            }
                        }
                        _ => out.push(c),
                    }
                }
                Ok(Tok::Str(String::from_utf8_lossy(&out).into_owned()))
            }
            _ => {
                // Unquoted token: runs until whitespace or a brace/quote.
                let start = self.i;
                while self.i < self.s.len()
                    && !self.s[self.i].is_ascii_whitespace()
                    && !matches!(self.s[self.i], b'{' | b'}' | b'"')
                {
                    self.i += 1;
                }
                Ok(Tok::Str(
                    String::from_utf8_lossy(&self.s[start..self.i]).into_owned(),
                ))
            }
        }
    }

    fn object_body(&mut self, nested: bool) -> Result<Vec<(String, Vdf)>, VdfError> {
        let mut items = Vec::new();
        loop {
            let key = match self.token()? {
                Tok::Str(k) => k,
                Tok::Close if nested => return Ok(items),
                Tok::Eof if !nested => return Ok(items),
                Tok::Eof => return Err(self.err("missing '}'")),
                Tok::Close => return Err(self.err("unexpected '}'")),
                Tok::Open => return Err(self.err("expected a key, found '{'")),
            };
            let value = match self.token()? {
                Tok::Str(v) => Vdf::Str(v),
                Tok::Open => Vdf::Obj(self.object_body(true)?),
                Tok::Close | Tok::Eof => return Err(self.err("key without a value")),
            };
            items.push((key, value));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_with_escapes_and_comments() {
        let v = parse(
            "// comment\n\"root\"\n{\n\t\"path\"\t\t\"C:\\\\Games\\\\Steam\"\n\t\"Sub\" { \"a\" \"1\" }\n\tbare value\n}\n",
        )
        .unwrap();
        assert_eq!(
            v.path(&["root", "path"]).and_then(Vdf::as_str),
            Some("C:\\Games\\Steam")
        );
        assert_eq!(
            v.path(&["ROOT", "sub", "A"]).and_then(Vdf::as_str),
            Some("1")
        );
        assert_eq!(
            v.path(&["root", "bare"]).and_then(Vdf::as_str),
            Some("value")
        );
    }

    #[test]
    fn reports_errors() {
        assert!(parse("\"a\" {").is_err());
        assert!(parse("\"a\" \"unterminated").is_err());
        assert!(parse("}").is_err());
        assert!(parse("\"a\"").is_err());
    }

    #[test]
    fn non_ascii_paths() {
        let v = parse("\"p\" \"/run/media/ユーザー/SD\"").unwrap();
        assert_eq!(
            v.get("p").and_then(Vdf::as_str),
            Some("/run/media/ユーザー/SD")
        );
    }
}
