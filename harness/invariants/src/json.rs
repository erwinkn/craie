//! A minimal JSON reader for the shared fixtures under
//! `packages/bridge/traces/`: they are ours and well formed.

use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Value>),
    Obj(BTreeMap<String, Value>),
}

impl Value {
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Obj(m) => m.get(key),
            _ => None,
        }
    }

    pub fn num(&self) -> Option<f64> {
        match self {
            Value::Num(n) => Some(*n),
            _ => None,
        }
    }

    pub fn str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn arr(&self) -> &[Value] {
        match self {
            Value::Arr(a) => a,
            _ => &[],
        }
    }

    pub fn bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }
}

pub fn parse(s: &str) -> Result<Value, String> {
    let mut p = Parser {
        b: s.as_bytes(),
        at: 0,
    };
    let v = p.value()?;
    p.ws();
    if p.at != p.b.len() {
        return Err(format!("trailing bytes at {}", p.at));
    }
    Ok(v)
}

struct Parser<'a> {
    b: &'a [u8],
    at: usize,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.b.get(self.at).is_some_and(u8::is_ascii_whitespace) {
            self.at += 1;
        }
    }

    fn eat(&mut self, c: u8) -> Result<(), String> {
        self.ws();
        if self.b.get(self.at) != Some(&c) {
            return Err(format!("expected '{}' at {}", c as char, self.at));
        }
        self.at += 1;
        Ok(())
    }

    fn value(&mut self) -> Result<Value, String> {
        self.ws();
        match self.b.get(self.at) {
            Some(b'{') => {
                self.at += 1;
                let mut m = BTreeMap::new();
                self.ws();
                if self.b.get(self.at) == Some(&b'}') {
                    self.at += 1;
                    return Ok(Value::Obj(m));
                }
                loop {
                    self.ws();
                    let Value::Str(k) = self.value()? else {
                        return Err(format!("a key at {}", self.at));
                    };
                    self.eat(b':')?;
                    m.insert(k, self.value()?);
                    self.ws();
                    match self.b.get(self.at) {
                        Some(b',') => self.at += 1,
                        Some(b'}') => {
                            self.at += 1;
                            return Ok(Value::Obj(m));
                        }
                        _ => return Err(format!("',' or '}}' at {}", self.at)),
                    }
                }
            }
            Some(b'[') => {
                self.at += 1;
                let mut a = Vec::new();
                self.ws();
                if self.b.get(self.at) == Some(&b']') {
                    self.at += 1;
                    return Ok(Value::Arr(a));
                }
                loop {
                    a.push(self.value()?);
                    self.ws();
                    match self.b.get(self.at) {
                        Some(b',') => self.at += 1,
                        Some(b']') => {
                            self.at += 1;
                            return Ok(Value::Arr(a));
                        }
                        _ => return Err(format!("',' or ']' at {}", self.at)),
                    }
                }
            }
            Some(b'"') => {
                self.at += 1;
                let mut s = String::new();
                loop {
                    let start = self.at;
                    while !matches!(self.b.get(self.at), Some(b'"' | b'\\') | None) {
                        self.at += 1;
                    }
                    s.push_str(std::str::from_utf8(&self.b[start..self.at]).unwrap());
                    match self.b.get(self.at) {
                        Some(b'"') => {
                            self.at += 1;
                            return Ok(Value::Str(s));
                        }
                        Some(b'\\') => {
                            let c = *self.b.get(self.at + 1).ok_or("escape")?;
                            self.at += 2;
                            s.push(match c {
                                b'n' => '\n',
                                b't' => '\t',
                                b'u' => {
                                    let hex = std::str::from_utf8(&self.b[self.at..self.at + 4])
                                        .map_err(|e| e.to_string())?;
                                    self.at += 4;
                                    let c =
                                        u32::from_str_radix(hex, 16).map_err(|e| e.to_string())?;
                                    char::from_u32(c).ok_or("a surrogate escape")?
                                }
                                c => c as char,
                            });
                        }
                        _ => return Err("an unterminated string".into()),
                    }
                }
            }
            Some(b't') if self.b[self.at..].starts_with(b"true") => {
                self.at += 4;
                Ok(Value::Bool(true))
            }
            Some(b'f') if self.b[self.at..].starts_with(b"false") => {
                self.at += 5;
                Ok(Value::Bool(false))
            }
            Some(b'n') if self.b[self.at..].starts_with(b"null") => {
                self.at += 4;
                Ok(Value::Null)
            }
            Some(_) => {
                let start = self.at;
                while self
                    .b
                    .get(self.at)
                    .is_some_and(|c| c.is_ascii_digit() || b"+-.eE".contains(c))
                {
                    self.at += 1;
                }
                let s = std::str::from_utf8(&self.b[start..self.at]).unwrap();
                s.parse()
                    .map(Value::Num)
                    .map_err(|_| format!("a value at {start}"))
            }
            None => Err("unexpected end".into()),
        }
    }
}
