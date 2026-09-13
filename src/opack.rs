//! OPACK — Apple's proprietary serialization format used by Handoff, Universal
//! Clipboard and Wi-Fi Password Sharing to carry structured payloads.
//!
//! Ported from seemoo-lab openwifipass `OPACK.py`. Supported types: bool, int
//! (unsigned, up to 4 bytes as the reference encoder does), string, bytes,
//! array, dict. Float/date/UUID are intentionally omitted (the reference does
//! not implement them either).
//!
//! Reference for the wire format:
//!   * false=0x02 / true=0x01
//!   * small int 0..=0x26 => 0x08+value; else 0x30+(len-1) then big-endian bytes
//!   * string len<=0x20 => 0x40+len + utf8; else 0x61+(lenbytes-1) + len + utf8
//!   * bytes  len<=0x20 => 0x70+len + data; else 0x91+(lenbytes-1) + len + data
//!   * array  n<0x0F    => 0xD0+n + items; else 0xDF + items + 0x03
//!   * dict   n<0x0F    => 0xE0+n + pairs; else 0xEF + pairs + 0x03

#![allow(dead_code)] // M2/M3 scaffolding: exercised by unit tests; wired into the runtime once macOS keys exist.

use anyhow::{bail, Result};

/// An OPACK value. Dicts keep insertion order and allow non-string keys, so we
/// model them as an ordered list of pairs rather than a map.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Bool(bool),
    Int(u64),
    Str(String),
    Bytes(Vec<u8>),
    Array(Vec<Value>),
    Dict(Vec<(Value, Value)>),
}

impl Value {
    /// Convenience: build a dict from string keys.
    pub fn dict<I: IntoIterator<Item = (&'static str, Value)>>(pairs: I) -> Value {
        Value::Dict(
            pairs
                .into_iter()
                .map(|(k, v)| (Value::Str(k.to_string()), v))
                .collect(),
        )
    }

    /// Look up a string key in a dict.
    pub fn get(&self, key: &str) -> Option<&Value> {
        if let Value::Dict(pairs) = self {
            pairs.iter().find(|(k, _)| matches!(k, Value::Str(s) if s == key)).map(|(_, v)| v)
        } else {
            None
        }
    }

    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Value::Bytes(b) => Some(b),
            _ => None,
        }
    }
}

pub fn encode(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    encode_into(value, &mut out);
    out
}

fn encode_into(value: &Value, out: &mut Vec<u8>) {
    match value {
        Value::Bool(true) => out.push(0x01),
        Value::Bool(false) => out.push(0x02),
        Value::Int(v) => encode_int(*v, out),
        Value::Str(s) => encode_str(s, out),
        Value::Bytes(b) => encode_bytes(b, out),
        Value::Array(items) => {
            if items.len() < 0x0F {
                out.push(0xD0 + items.len() as u8);
                for it in items {
                    encode_into(it, out);
                }
            } else {
                out.push(0xDF);
                for it in items {
                    encode_into(it, out);
                }
                out.push(0x03);
            }
        }
        Value::Dict(pairs) => {
            if pairs.len() < 0x0F {
                out.push(0xE0 + pairs.len() as u8);
                for (k, v) in pairs {
                    encode_into(k, out);
                    encode_into(v, out);
                }
            } else {
                out.push(0xEF);
                for (k, v) in pairs {
                    encode_into(k, out);
                    encode_into(v, out);
                }
                out.push(0x03);
            }
        }
    }
}

fn min_be_bytes(value: u64) -> Vec<u8> {
    if value == 0 {
        return vec![0];
    }
    let full = value.to_be_bytes();
    let first = full.iter().position(|&b| b != 0).unwrap();
    full[first..].to_vec()
}

fn encode_int(value: u64, out: &mut Vec<u8>) {
    if value < 0x27 {
        out.push(value as u8 + 0x08);
        return;
    }
    let be = min_be_bytes(value);
    // Reference encoder supports up to 4 bytes.
    debug_assert!(be.len() <= 4, "OPACK int wider than 4 bytes");
    out.push(0x30 + (be.len() as u8 - 1));
    out.extend_from_slice(&be);
}

fn encode_len_prefixed(base_short: u8, base_long: u8, payload: &[u8], out: &mut Vec<u8>) {
    let len = payload.len();
    if len <= 0x20 {
        out.push(base_short + len as u8);
        out.extend_from_slice(payload);
        return;
    }
    let be = min_be_bytes(len as u64);
    out.push(base_long + (be.len() as u8 - 1));
    out.extend_from_slice(&be);
    out.extend_from_slice(payload);
}

fn encode_str(s: &str, out: &mut Vec<u8>) {
    encode_len_prefixed(0x40, 0x61, s.as_bytes(), out);
}

fn encode_bytes(b: &[u8], out: &mut Vec<u8>) {
    encode_len_prefixed(0x70, 0x91, b, out);
}

pub fn decode(data: &[u8]) -> Result<Value> {
    let mut d = Decoder { data, pos: 0 };
    d.parse()
}

struct Decoder<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Decoder<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8]> {
        if self.pos + n > self.data.len() {
            bail!("OPACK: unexpected end of data");
        }
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    fn peek(&self) -> Option<u8> {
        self.data.get(self.pos).copied()
    }

    fn read_be(&mut self, n: usize) -> Result<u64> {
        let mut v = 0u64;
        for &b in self.take(n)? {
            v = (v << 8) | b as u64;
        }
        Ok(v)
    }

    fn parse(&mut self) -> Result<Value> {
        let t = self.take(1)?[0];
        match t {
            0x01 => Ok(Value::Bool(true)),
            0x02 => Ok(Value::Bool(false)),
            0x08..=0x2F => Ok(Value::Int((t - 0x08) as u64)),
            0x30..=0x33 => {
                let n = (t - 0x30) as usize + 1;
                Ok(Value::Int(self.read_be(n)?))
            }
            0x40..=0x60 => {
                let len = (t - 0x40) as usize;
                self.parse_str(len)
            }
            0x61..=0x64 => {
                let lenbytes = (t - 0x60) as usize;
                let len = self.read_be(lenbytes)? as usize;
                self.parse_str(len)
            }
            0x70..=0x90 => {
                let len = (t - 0x70) as usize;
                Ok(Value::Bytes(self.take(len)?.to_vec()))
            }
            0x91..=0x94 => {
                let lenbytes = (t - 0x90) as usize;
                let len = self.read_be(lenbytes)? as usize;
                Ok(Value::Bytes(self.take(len)?.to_vec()))
            }
            0xD0..=0xDE => {
                let n = (t - 0xD0) as usize;
                let mut items = Vec::with_capacity(n);
                for _ in 0..n {
                    items.push(self.parse()?);
                }
                Ok(Value::Array(items))
            }
            0xDF => {
                let mut items = Vec::new();
                loop {
                    if self.peek() == Some(0x03) {
                        self.pos += 1;
                        break;
                    }
                    items.push(self.parse()?);
                }
                Ok(Value::Array(items))
            }
            0xE0..=0xEE => {
                let n = (t - 0xE0) as usize;
                let mut pairs = Vec::with_capacity(n);
                for _ in 0..n {
                    let k = self.parse()?;
                    let v = self.parse()?;
                    pairs.push((k, v));
                }
                Ok(Value::Dict(pairs))
            }
            0xEF => {
                let mut pairs = Vec::new();
                loop {
                    if self.peek() == Some(0x03) {
                        self.pos += 1;
                        break;
                    }
                    let k = self.parse()?;
                    let v = self.parse()?;
                    pairs.push((k, v));
                }
                Ok(Value::Dict(pairs))
            }
            other => bail!("OPACK: unknown type byte {other:#04x}"),
        }
    }

    fn parse_str(&mut self, len: usize) -> Result<Value> {
        let bytes = self.take(len)?.to_vec();
        Ok(Value::Str(String::from_utf8(bytes)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(v: Value) {
        let enc = encode(&v);
        let dec = decode(&enc).expect("decode");
        assert_eq!(v, dec, "roundtrip mismatch; encoded = {}", hex::encode(&enc));
    }

    #[test]
    fn openwifipass_vector() {
        // From openwifipass tests/test_opack.py: {"pf": 266256}
        let v = Value::dict([("pf", Value::Int(266256))]);
        let enc = encode(&v);
        // E1 (dict,1) | 42 'p'(70) 'f'(66) (str len2) | 32 (int,3 bytes) 04 10 10
        // (266256 == 0x041010)
        assert_eq!(hex::encode(&enc), "e142706632041010");
        assert_eq!(decode(&enc).unwrap(), v);
    }

    #[test]
    fn ints() {
        for v in [0u64, 1, 0x26, 0x27, 0xFF, 0x100, 266256, 0x00FF_FFFF, 0x0100_0000, u32::MAX as u64] {
            roundtrip(Value::Int(v));
        }
        // small-int encoding boundary
        assert_eq!(encode(&Value::Int(0)), vec![0x08]);
        assert_eq!(encode(&Value::Int(0x26)), vec![0x2E]);
        assert_eq!(encode(&Value::Int(0x27)), vec![0x30, 0x27]);
    }

    #[test]
    fn strings_short_and_long() {
        roundtrip(Value::Str("_pd".into()));
        roundtrip(Value::Str("x".repeat(0x20)));
        roundtrip(Value::Str("y".repeat(0x21))); // triggers long form
        roundtrip(Value::Str("z".repeat(500)));
    }

    #[test]
    fn bytes_and_containers() {
        roundtrip(Value::Bytes(vec![0u8; 10]));
        roundtrip(Value::Bytes(vec![0xABu8; 300])); // long form
        roundtrip(Value::Array((0..20).map(Value::Int).collect())); // endless form
        roundtrip(Value::Array(vec![Value::Bool(true), Value::Str("a".into())]));
        let big: Vec<(Value, Value)> =
            (0..20).map(|i| (Value::Str(format!("k{i}")), Value::Int(i))).collect();
        roundtrip(Value::Dict(big)); // endless dict
    }

    #[test]
    fn nested() {
        let v = Value::dict([
            ("_pd", Value::Bytes(vec![1, 2, 3])),
            ("list", Value::Array(vec![Value::Int(1), Value::dict([("a", Value::Bool(false))])])),
        ]);
        roundtrip(v);
    }
}
