//! Small typed accessors over `weftcodec::wcb::Value`.
//!
//! All store records are canonical WCB maps. This module keeps the
//! encode/decode plumbing in one place so every record parser reports the same
//! classes: `Malformed` for structure/type errors, `NonCanonical` for bytes the
//! codec rejects, and `Corrupt` for identity/length mismatches at call sites.

use std::collections::BTreeMap;

use weftcodec::wcb::{self, Value};

use crate::error::{Error, Result};
use crate::identity::Hash32;

/// Encode a value with default (spec) limits.
pub fn encode(value: &Value) -> Result<Vec<u8>> {
    wcb::encode_default(value).map_err(Error::from)
}

/// Decode canonical WCB bytes with default (spec) limits.
pub fn decode(bytes: &[u8]) -> Result<Value> {
    wcb::decode_default(bytes).map_err(Error::from)
}

/// `bytes32` WCB value.
pub fn hash_value(hash: Hash32) -> Value {
    Value::Bytes(hash.as_bytes().to_vec())
}

/// Nullable `bytes32` WCB value.
pub fn opt_hash_value(hash: Option<Hash32>) -> Value {
    match hash {
        Some(hash) => hash_value(hash),
        None => Value::Null,
    }
}

/// Build a WCB map from pairs; the encoder sorts keys canonically.
pub fn map<I, K>(pairs: I) -> Value
where
    I: IntoIterator<Item = (K, Value)>,
    K: Into<String>,
{
    Value::map(pairs)
}

/// Borrow a WCB map, or `Malformed`.
pub fn as_map(value: &Value) -> Result<&BTreeMap<String, Value>> {
    match value {
        Value::Map(map) => Ok(map),
        _ => Err(Error::malformed("expected a WCB map")),
    }
}

/// Reject maps with keys outside `allowed`.
pub fn check_keys(map: &BTreeMap<String, Value>, allowed: &[&str]) -> Result<()> {
    for key in map.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(Error::malformed(format!("unexpected map key `{key}`")));
        }
    }
    Ok(())
}

/// Required field.
pub fn field<'a>(map: &'a BTreeMap<String, Value>, key: &str) -> Result<&'a Value> {
    map.get(key)
        .ok_or_else(|| Error::malformed(format!("missing map key `{key}`")))
}

/// Text field.
pub fn get_text(map: &BTreeMap<String, Value>, key: &str) -> Result<String> {
    match field(map, key)? {
        Value::Str(text) => Ok(text.clone()),
        _ => Err(Error::malformed(format!("key `{key}` must be text"))),
    }
}

/// Unsigned integer field.
pub fn get_u64(map: &BTreeMap<String, Value>, key: &str) -> Result<u64> {
    match field(map, key)? {
        Value::U64(value) => Ok(*value),
        _ => Err(Error::malformed(format!("key `{key}` must be u64"))),
    }
}

/// Timestamp field (nanoseconds since the Unix epoch).
pub fn get_timestamp(map: &BTreeMap<String, Value>, key: &str) -> Result<i64> {
    match field(map, key)? {
        Value::Timestamp(value) => Ok(*value),
        _ => Err(Error::malformed(format!("key `{key}` must be a timestamp"))),
    }
}

/// Raw bytes field.
pub fn get_bytes(map: &BTreeMap<String, Value>, key: &str) -> Result<Vec<u8>> {
    match field(map, key)? {
        Value::Bytes(bytes) => Ok(bytes.clone()),
        _ => Err(Error::malformed(format!("key `{key}` must be bytes"))),
    }
}

/// Exactly-32-byte field.
pub fn get_hash(map: &BTreeMap<String, Value>, key: &str) -> Result<Hash32> {
    let bytes = get_bytes(map, key)?;
    Hash32::from_slice(&bytes).map_err(Error::from)
}

/// Nullable 32-byte field.
pub fn get_opt_hash(map: &BTreeMap<String, Value>, key: &str) -> Result<Option<Hash32>> {
    match field(map, key)? {
        Value::Null => Ok(None),
        Value::Bytes(bytes) => Ok(Some(Hash32::from_slice(bytes).map_err(Error::from)?)),
        _ => Err(Error::malformed(format!(
            "key `{key}` must be bytes32 or null"
        ))),
    }
}

/// Map field.
pub fn get_map(map: &BTreeMap<String, Value>, key: &str) -> Result<BTreeMap<String, Value>> {
    as_map(field(map, key)?).map(Clone::clone)
}

/// Nullable map field.
pub fn get_opt_map(
    map: &BTreeMap<String, Value>,
    key: &str,
) -> Result<Option<BTreeMap<String, Value>>> {
    match field(map, key)? {
        Value::Null => Ok(None),
        Value::Map(inner) => Ok(Some(inner.clone())),
        _ => Err(Error::malformed(format!(
            "key `{key}` must be a map or null"
        ))),
    }
}

/// Array field.
pub fn get_array<'a>(map: &'a BTreeMap<String, Value>, key: &str) -> Result<&'a Vec<Value>> {
    match field(map, key)? {
        Value::Array(items) => Ok(items),
        _ => Err(Error::malformed(format!("key `{key}` must be an array"))),
    }
}

/// A scalar relation target: text, u64, i64, timestamp, bool, or null.
pub fn validate_scalar(value: &Value) -> Result<()> {
    match value {
        Value::Null
        | Value::Bool(_)
        | Value::U64(_)
        | Value::I64(_)
        | Value::Timestamp(_)
        | Value::Str(_) => Ok(()),
        _ => Err(Error::malformed(
            "relation value target must be a WCB scalar",
        )),
    }
}

/// Render a WCB value as JSON for the CLI. Bytes become lowercase hex.
pub fn value_to_json(value: &Value) -> serde_json::Value {
    match value {
        Value::Null => serde_json::Value::Null,
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::U64(n) => serde_json::json!(n),
        Value::I64(n) => serde_json::json!(n),
        Value::Timestamp(ns) => serde_json::json!(ns),
        Value::Str(s) => serde_json::Value::String(s.clone()),
        Value::Bytes(bytes) => serde_json::Value::String(hex::encode(bytes)),
        Value::Array(items) => serde_json::Value::Array(items.iter().map(value_to_json).collect()),
        Value::Map(map) => {
            let mut out = serde_json::Map::new();
            for (key, value) in map {
                out.insert(key.clone(), value_to_json(value));
            }
            serde_json::Value::Object(out)
        }
    }
}

/// Parse a text value from a JSON scalar (used by CLI relation provenance).
pub fn json_to_value(value: &serde_json::Value) -> Result<Value> {
    match value {
        serde_json::Value::Null => Ok(Value::Null),
        serde_json::Value::Bool(b) => Ok(Value::Bool(*b)),
        serde_json::Value::Number(number) => {
            if let Some(u) = number.as_u64() {
                Ok(Value::U64(u))
            } else if let Some(i) = number.as_i64() {
                Ok(Value::I64(i))
            } else {
                Err(Error::invalid_argument("JSON number is not an integer"))
            }
        }
        serde_json::Value::String(s) => Ok(Value::Str(s.clone())),
        serde_json::Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(json_to_value(item)?);
            }
            Ok(Value::Array(out))
        }
        serde_json::Value::Object(map) => {
            let mut out = BTreeMap::new();
            for (key, value) in map {
                out.insert(key.clone(), json_to_value(value)?);
            }
            Ok(Value::Map(out))
        }
    }
}
