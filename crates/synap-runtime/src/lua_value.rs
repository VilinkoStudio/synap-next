//! `CanonicalValue` 与 Lua 值之间的转换。
//!
//! 转换是**类型导向**的：字段声明了什么类型，就按那个类型收。没有类型提示
//! （例如动作参数、自由 map）时才按结构猜：连续整数键是 list，字符串键是 map。
//!
//! 引用、Blob 与 Hlc 在 Lua 里仍是普通 table，带保留键：
//! `{ __synap = "ref", id = "<uuid>" }`

use std::collections::BTreeMap;

use mlua::{Table, Value};
use synap_core::{BlobHash, CanonicalValue, EntityId, Hlc};

use crate::error::PluginError;
use crate::types::TypeSpec;

pub(crate) const REF_TAG: &str = "ref";
pub(crate) const BLOB_TAG: &str = "blob";
const TAG_KEY: &str = "__synap";

/// 值的最大嵌套深度。自引用的 table 也靠它截住，不会让宿主栈溢出。
pub(crate) const MAX_DEPTH: usize = 64;

/// LuaJIT 的数字是 double，超出这个范围的整数进出 Lua 会悄悄变值。
const MAX_SAFE_INT: i64 = 1 << 53;

fn too_deep(path: &str) -> PluginError {
    PluginError::manifest(format!(
        "{path}: value is nested deeper than {MAX_DEPTH} levels (or contains a cycle)"
    ))
}

/// 把规范化值推成 Lua 值。
pub(crate) fn push(lua: &mlua::Lua, value: &CanonicalValue) -> mlua::Result<Value> {
    push_at(lua, value, 0)
}

fn push_at(lua: &mlua::Lua, value: &CanonicalValue, depth: usize) -> mlua::Result<Value> {
    if depth > MAX_DEPTH {
        return Err(mlua::Error::RuntimeError(format!(
            "value is nested deeper than {MAX_DEPTH} levels"
        )));
    }
    Ok(match value {
        CanonicalValue::Null => Value::Nil,
        CanonicalValue::Bool(value) => Value::Boolean(*value),
        CanonicalValue::Int(value) => {
            if !(-MAX_SAFE_INT..=MAX_SAFE_INT).contains(value) {
                return Err(mlua::Error::RuntimeError(format!(
                    "integer {value} cannot pass through Lua without losing precision (limit is ±2^53)"
                )));
            }
            Value::Integer(*value)
        }
        CanonicalValue::Float(value) => Value::Number(*value),
        CanonicalValue::String(text) => Value::String(lua.create_string(text)?),
        CanonicalValue::Bytes(bytes) => Value::String(lua.create_string(bytes)?),
        CanonicalValue::List(items) => {
            let table = lua.create_table_with_capacity(items.len(), 0)?;
            for (index, item) in items.iter().enumerate() {
                table.raw_set(index + 1, push_at(lua, item, depth + 1)?)?;
            }
            Value::Table(table)
        }
        CanonicalValue::Map(map) => {
            let table = lua.create_table_with_capacity(0, map.len())?;
            for (key, item) in map {
                table.raw_set(key.as_str(), push_at(lua, item, depth + 1)?)?;
            }
            Value::Table(table)
        }
        CanonicalValue::Ref(entity) => {
            let table = lua.create_table_with_capacity(0, 2)?;
            table.raw_set(TAG_KEY, REF_TAG)?;
            table.raw_set("id", entity.to_string())?;
            Value::Table(table)
        }
        CanonicalValue::Blob(hash) => {
            let table = lua.create_table_with_capacity(0, 2)?;
            table.raw_set(TAG_KEY, BLOB_TAG)?;
            table.raw_set("hash", hex(hash.as_bytes()))?;
            Value::Table(table)
        }
        CanonicalValue::Hlc(hlc) => {
            let table = lua.create_table_with_capacity(0, 3)?;
            table.raw_set("physical_ms", hlc.physical_ms as i64)?;
            table.raw_set("logical", i64::from(hlc.logical))?;
            table.raw_set("node_id", i64::from(hlc.node_id))?;
            Value::Table(table)
        }
    })
}

/// 按类型收一个值。
pub(crate) fn take(
    value: Value,
    spec: &TypeSpec,
    path: &str,
) -> Result<CanonicalValue, PluginError> {
    take_at(value, spec, path, 0)
}

fn take_at(
    value: Value,
    spec: &TypeSpec,
    path: &str,
    depth: usize,
) -> Result<CanonicalValue, PluginError> {
    if depth > MAX_DEPTH {
        return Err(too_deep(path));
    }
    match spec {
        TypeSpec::Optional(inner) => {
            if matches!(value, Value::Nil) {
                Ok(CanonicalValue::Null)
            } else {
                take_at(value, inner, path, depth)
            }
        }
        TypeSpec::Any => take_any_at(value, path, depth),
        TypeSpec::Bool => match value {
            Value::Boolean(value) => Ok(CanonicalValue::Bool(value)),
            other => mismatch(path, "bool", &other),
        },
        TypeSpec::Int(kind) => match value {
            Value::Integer(value) => {
                let (low, high) = kind.bounds();
                if value < low || value > high {
                    return Err(PluginError::manifest(format!(
                        "{path}: {value} is out of range for {}",
                        spec.describe()
                    )));
                }
                Ok(CanonicalValue::Int(value))
            }
            other => mismatch(path, &spec.describe(), &other),
        },
        TypeSpec::Float => match value {
            Value::Integer(value) => Ok(CanonicalValue::Float(value as f64)),
            Value::Number(value) => Ok(CanonicalValue::Float(value)),
            other => mismatch(path, "float", &other),
        },
        TypeSpec::Text => match value {
            Value::String(text) => text
                .to_str()
                .map(|text| CanonicalValue::String(text.to_owned()))
                .map_err(|_| PluginError::manifest(format!("{path}: text is not UTF-8"))),
            other => mismatch(path, "text", &other),
        },
        TypeSpec::Bytes => match value {
            Value::String(text) => Ok(CanonicalValue::Bytes(text.as_bytes().to_vec())),
            other => mismatch(path, "bytes", &other),
        },
        TypeSpec::Ref => match value {
            Value::Table(table) => {
                let tag: Option<String> = table.raw_get(TAG_KEY)?;
                if tag.as_deref() != Some(REF_TAG) {
                    return Err(PluginError::manifest(format!(
                        "{path}: expected a ref (synap.ref), got a plain table"
                    )));
                }
                let id: String = table.raw_get("id")?;
                let entity = id.parse::<EntityId>().map_err(|_| {
                    PluginError::manifest(format!("{path}: `{id}` is not an entity id"))
                })?;
                Ok(CanonicalValue::Ref(entity))
            }
            other => mismatch(path, "ref", &other),
        },
        TypeSpec::Blob => match value {
            Value::Table(table) => {
                let tag: Option<String> = table.raw_get(TAG_KEY)?;
                if tag.as_deref() != Some(BLOB_TAG) {
                    return Err(PluginError::manifest(format!(
                        "{path}: expected a blob (synap.blob), got a plain table"
                    )));
                }
                let text: String = table.raw_get("hash")?;
                let hash = unhex(&text).ok_or_else(|| {
                    PluginError::manifest(format!("{path}: `{text}` is not a 32-byte hex hash"))
                })?;
                Ok(CanonicalValue::Blob(BlobHash::from_bytes(hash)))
            }
            other => mismatch(path, "blob", &other),
        },
        TypeSpec::Hlc => match value {
            Value::Table(table) => Ok(CanonicalValue::Hlc(Hlc {
                physical_ms: table.raw_get("physical_ms")?,
                logical: table.raw_get("logical")?,
                node_id: table.raw_get("node_id")?,
            })),
            other => mismatch(path, "hlc", &other),
        },
        TypeSpec::List(inner) => match value {
            Value::Table(table) => {
                let items = take_array(table, path)?;
                let mut out = Vec::with_capacity(items.len());
                for (index, item) in items.into_iter().enumerate() {
                    out.push(take_at(
                        item,
                        inner,
                        &format!("{path}[{index}]"),
                        depth + 1,
                    )?);
                }
                Ok(CanonicalValue::List(out))
            }
            other => mismatch(path, "list", &other),
        },
    }
}

/// 没有类型提示时按结构收。
pub(crate) fn take_any(value: Value, path: &str) -> Result<CanonicalValue, PluginError> {
    take_any_at(value, path, 0)
}

fn take_any_at(value: Value, path: &str, depth: usize) -> Result<CanonicalValue, PluginError> {
    if depth > MAX_DEPTH {
        return Err(too_deep(path));
    }
    Ok(match value {
        Value::Nil => CanonicalValue::Null,
        Value::Boolean(value) => CanonicalValue::Bool(value),
        Value::Integer(value) => CanonicalValue::Int(value),
        Value::Number(value) => CanonicalValue::Float(value),
        Value::String(text) => match text.to_str() {
            Ok(text) => CanonicalValue::String(text.to_owned()),
            Err(_) => CanonicalValue::Bytes(text.as_bytes().to_vec()),
        },
        Value::Table(table) => take_table(table, path, depth)?,
        other => {
            return Err(PluginError::manifest(format!(
                "{path}: unsupported Lua value `{}`",
                other.type_name()
            )));
        }
    })
}

fn take_table(table: Table, path: &str, depth: usize) -> Result<CanonicalValue, PluginError> {
    let tag: Option<String> = table.raw_get(TAG_KEY)?;
    if tag.as_deref() == Some(REF_TAG) {
        let id: String = table.raw_get("id")?;
        return id
            .parse::<EntityId>()
            .map(CanonicalValue::Ref)
            .map_err(|_| PluginError::manifest(format!("{path}: `{id}` is not an entity id")));
    }
    if tag.as_deref() == Some(BLOB_TAG) {
        let text: String = table.raw_get("hash")?;
        let hash = unhex(&text).ok_or_else(|| {
            PluginError::manifest(format!("{path}: `{text}` is not a 32-byte hex hash"))
        })?;
        return Ok(CanonicalValue::Blob(BlobHash::from_bytes(hash)));
    }

    let entries = table
        .pairs::<Value, Value>()
        .collect::<mlua::Result<Vec<_>>>()?;
    if entries.is_empty() {
        return Ok(CanonicalValue::Map(BTreeMap::new()));
    }
    if is_array(&entries) {
        let mut items = Vec::with_capacity(entries.len());
        for (_, value) in entries {
            items.push(take_any_at(value, path, depth + 1)?);
        }
        return Ok(CanonicalValue::List(items));
    }
    let mut map = BTreeMap::new();
    for (key, value) in entries {
        let Value::String(key) = key else {
            return Err(PluginError::manifest(format!(
                "{path}: map keys must be strings"
            )));
        };
        let key = key
            .to_str()
            .map_err(|_| PluginError::manifest(format!("{path}: map key is not UTF-8")))?
            .to_owned();
        map.insert(
            key.clone(),
            take_any_at(value, &format!("{path}.{key}"), depth + 1)?,
        );
    }
    Ok(CanonicalValue::Map(map))
}

fn take_array(table: Table, path: &str) -> Result<Vec<Value>, PluginError> {
    let entries = table
        .pairs::<Value, Value>()
        .collect::<mlua::Result<Vec<_>>>()?;
    if entries.is_empty() {
        return Ok(Vec::new());
    }
    if !is_array(&entries) {
        return Err(PluginError::manifest(format!(
            "{path}: expected an array, got a map"
        )));
    }
    Ok(entries.into_iter().map(|(_, value)| value).collect())
}

fn is_array(entries: &[(Value, Value)]) -> bool {
    entries
        .iter()
        .enumerate()
        .all(|(index, (key, _))| matches!(key, Value::Integer(value) if *value == index as i64 + 1))
}

fn mismatch<T>(path: &str, expected: &str, value: &Value) -> Result<T, PluginError> {
    Err(PluginError::manifest(format!(
        "{path}: expected {expected}, got `{}`",
        value.type_name()
    )))
}

/// 校验一个已经是规范形状的值。`Null` 一律放行，表示字段缺席。
pub(crate) fn check(
    value: &CanonicalValue,
    spec: &TypeSpec,
    path: &str,
) -> Result<(), PluginError> {
    if matches!(value, CanonicalValue::Null) {
        return Ok(());
    }
    match spec {
        TypeSpec::Optional(inner) => check(value, inner, path),
        TypeSpec::Any => Ok(()),
        TypeSpec::Bool => matches!(value, CanonicalValue::Bool(_))
            .then_some(())
            .ok_or_else(|| type_error(path, "bool", value)),
        TypeSpec::Int(kind) => match value {
            CanonicalValue::Int(number) => {
                let (low, high) = kind.bounds();
                if *number < low || *number > high {
                    Err(PluginError::manifest(format!(
                        "{path}: {number} is out of range for {}",
                        spec.describe()
                    )))
                } else {
                    Ok(())
                }
            }
            _ => Err(type_error(path, &spec.describe(), value)),
        },
        TypeSpec::Float => match value {
            CanonicalValue::Int(_) | CanonicalValue::Float(_) => Ok(()),
            _ => Err(type_error(path, "float", value)),
        },
        TypeSpec::Text => matches!(value, CanonicalValue::String(_))
            .then_some(())
            .ok_or_else(|| type_error(path, "text", value)),
        TypeSpec::Bytes => matches!(value, CanonicalValue::Bytes(_))
            .then_some(())
            .ok_or_else(|| type_error(path, "bytes", value)),
        TypeSpec::Ref => matches!(value, CanonicalValue::Ref(_))
            .then_some(())
            .ok_or_else(|| type_error(path, "ref", value)),
        TypeSpec::Blob => matches!(value, CanonicalValue::Blob(_))
            .then_some(())
            .ok_or_else(|| type_error(path, "blob", value)),
        TypeSpec::Hlc => matches!(value, CanonicalValue::Hlc(_))
            .then_some(())
            .ok_or_else(|| type_error(path, "hlc", value)),
        TypeSpec::List(inner) => match value {
            CanonicalValue::List(items) => {
                for (index, item) in items.iter().enumerate() {
                    check(item, inner, &format!("{path}[{index}]"))?;
                }
                Ok(())
            }
            _ => Err(type_error(path, "list", value)),
        },
    }
}

fn type_error(path: &str, expected: &str, value: &CanonicalValue) -> PluginError {
    PluginError::manifest(format!("{path}: expected {expected}, got {}", kind(value)))
}

fn kind(value: &CanonicalValue) -> &'static str {
    match value {
        CanonicalValue::Null => "null",
        CanonicalValue::Bool(_) => "bool",
        CanonicalValue::Int(_) => "int",
        CanonicalValue::Float(_) => "float",
        CanonicalValue::String(_) => "text",
        CanonicalValue::Bytes(_) => "bytes",
        CanonicalValue::List(_) => "list",
        CanonicalValue::Map(_) => "map",
        CanonicalValue::Ref(_) => "ref",
        CanonicalValue::Blob(_) => "blob",
        CanonicalValue::Hlc(_) => "hlc",
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

fn unhex(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (index, slot) in out.iter_mut().enumerate() {
        let high = text.as_bytes()[index * 2];
        let low = text.as_bytes()[index * 2 + 1];
        *slot = (hex_digit(high)? << 4) | hex_digit(low)?;
    }
    Some(out)
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
