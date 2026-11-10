//! Steam's KeyValues formats: the binary one in shortcuts.vdf, and the text one in loginusers.vdf
//! and localconfig.vdf.
//!
//! Binary: every entry is a type byte, a NUL-terminated key, then the value. A map ends with 0x08.

pub enum Value {
    Map(Map),
    Str(String),
    Int(u32),
}

/// Entries in file order; Steam numbers shortcuts by position, so order is kept.
pub type Map = Vec<(String, Value)>;

const MAP: u8 = 0x00;
const STR: u8 = 0x01;
const INT: u8 = 0x02;
const END: u8 = 0x08;

/// Fails on value types steambro doesn't know, so it never rewrites a file it can't reproduce.
pub fn parse(bytes: &[u8]) -> crate::Result<Map> {
    Reader { bytes, pos: 0 }.map()
}

/// The exact inverse of `parse`.
pub fn write(map: &Map) -> Vec<u8> {
    let mut out = Vec::new();
    write_map(&mut out, map);
    out
}

/// Steam has changed key casing over the years ("AppName" vs "appname"), so lookups ignore it.
pub fn get<'a>(map: &'a Map, key: &str) -> Option<&'a Value> {
    map.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v)
}

pub fn get_mut<'a>(map: &'a mut Map, key: &str) -> Option<&'a mut Value> {
    map.iter_mut().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v)
}

fn write_map(out: &mut Vec<u8>, map: &Map) {
    for (key, value) in map {
        let tag = match value {
            Value::Map(_) => MAP,
            Value::Str(_) => STR,
            Value::Int(_) => INT,
        };
        out.push(tag);
        write_str(out, key);
        match value {
            Value::Map(inner) => write_map(out, inner),
            Value::Str(s) => write_str(out, s),
            Value::Int(i) => out.extend_from_slice(&i.to_le_bytes()),
        }
    }
    out.push(END);
}

fn write_str(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(s.as_bytes());
    out.push(0);
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn map(&mut self) -> crate::Result<Map> {
        let mut map = Vec::new();
        loop {
            let tag = self.take(1)?[0];
            if tag == END {
                return Ok(map);
            }
            let key = self.string()?;
            let value = match tag {
                MAP => Value::Map(self.map()?),
                STR => Value::Str(self.string()?),
                INT => Value::Int(u32::from_le_bytes(self.take(4)?.try_into()?)),
                other => return Err(format!("unsupported value type 0x{other:02x} at byte {}", self.pos - 1).into()),
            };
            map.push((key, value));
        }
    }

    fn string(&mut self) -> crate::Result<String> {
        let rest = &self.bytes[self.pos..];
        let len = rest.iter().position(|&b| b == 0).ok_or("unterminated string")?;
        let s = std::str::from_utf8(&rest[..len])?.to_string();
        self.pos += len + 1;
        Ok(s)
    }

    fn take(&mut self, n: usize) -> crate::Result<&[u8]> {
        let chunk = self.bytes.get(self.pos..self.pos + n).ok_or("file ends unexpectedly")?;
        self.pos += n;
        Ok(chunk)
    }
}

/// Text KeyValues: `"key" "value"` pairs and `"key" { ... }` blocks. Read-only.
pub mod text {
    pub enum Value {
        Str(String),
        Map(Map),
    }

    pub type Map = Vec<(String, Value)>;

    pub fn parse(text: &str) -> crate::Result<Map> {
        let mut tokens = Tokens { chars: text.chars().peekable() };
        let map = block(&mut tokens)?;
        Ok(map)
    }

    pub fn get<'a>(map: &'a Map, key: &str) -> Option<&'a Value> {
        map.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v)
    }

    pub fn get_str<'a>(map: &'a Map, key: &str) -> Option<&'a str> {
        match get(map, key) {
            Some(Value::Str(s)) => Some(s),
            _ => None,
        }
    }

    enum Token {
        Open,
        Close,
        Str(String),
    }

    fn block(tokens: &mut Tokens) -> crate::Result<Map> {
        let mut map = Vec::new();
        while let Some(token) = tokens.next()? {
            let key = match token {
                Token::Close => return Ok(map),
                Token::Open => return Err("unexpected '{'".into()),
                Token::Str(key) => key,
            };
            let value = match tokens.next()?.ok_or("file ends after a key")? {
                Token::Open => Value::Map(block(tokens)?),
                Token::Str(s) => Value::Str(s),
                Token::Close => return Err("unexpected '}'".into()),
            };
            map.push((key, value));
        }
        Ok(map)
    }

    struct Tokens<'a> {
        chars: std::iter::Peekable<std::str::Chars<'a>>,
    }

    impl Tokens<'_> {
        fn next(&mut self) -> crate::Result<Option<Token>> {
            loop {
                match self.chars.next() {
                    None => return Ok(None),
                    Some(c) if c.is_whitespace() => {}
                    Some('/') if self.chars.peek() == Some(&'/') => {
                        self.chars.by_ref().find(|&c| c == '\n');
                    }
                    Some('{') => return Ok(Some(Token::Open)),
                    Some('}') => return Ok(Some(Token::Close)),
                    Some('"') => {
                        let mut s = String::new();
                        loop {
                            match self.chars.next().ok_or("unterminated string")? {
                                '"' => break,
                                '\\' => match self.chars.next().ok_or("unterminated string")? {
                                    'n' => s.push('\n'),
                                    't' => s.push('\t'),
                                    other => s.push(other),
                                },
                                c => s.push(c),
                            }
                        }
                        return Ok(Some(Token::Str(s)));
                    }
                    Some(c) => {
                        let mut s = String::from(c);
                        while let Some(&c) = self.chars.peek() {
                            if c.is_whitespace() || c == '{' || c == '}' || c == '"' {
                                break;
                            }
                            s.push(c);
                            self.chars.next();
                        }
                        return Ok(Some(Token::Str(s)));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_round_trip_is_exact() {
        let map: Map = vec![(
            "shortcuts".into(),
            Value::Map(vec![(
                "0".into(),
                Value::Map(vec![
                    ("appid".into(), Value::Int(0x8123_4567)),
                    ("AppName".into(), Value::Str("Game".into())),
                    ("tags".into(), Value::Map(vec![("0".into(), Value::Str("Xbox".into()))])),
                ]),
            )]),
        )];
        let bytes = write(&map);
        assert_eq!(write(&parse(&bytes).unwrap()), bytes);
    }

    #[test]
    fn text_reads_nested_blocks() {
        let map = text::parse("\"users\"\n{\n\t\"765\"\n\t{\n\t\t\"PersonaName\"\t\t\"a \\\"b\\\"\"\n\t}\n}\n").unwrap();
        let Some(text::Value::Map(users)) = text::get(&map, "users") else { panic!() };
        let Some(text::Value::Map(user)) = text::get(users, "765") else { panic!() };
        assert_eq!(text::get_str(user, "personaname"), Some("a \"b\""));
    }
}
