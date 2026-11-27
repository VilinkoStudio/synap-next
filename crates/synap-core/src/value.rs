//! 规范化值。payload 与 Trait 输出都走这一套编码。
//!
//! 值类型集合是封闭的：`null / bool / int / float / string / bytes / list /
//! map（键有序）/ ref / blob / hlc`。map 的键只能是字符串，编码前按字节序排序，
//! 所以 Lua table 的遍历顺序不会进入哈希。
//!
//! 浮点先规范化再编码：`-0.0` 收敛到 `0.0`，所有 NaN 收敛到同一个位型。
//! 同一个语义值在任何设备上编码出的字节必须完全一致。

use std::collections::BTreeMap;

use crate::{BlobHash, CoreError, EntityId, Hlc};

/// 规范化值编码的版本。改布局就加一。
pub const VALUE_ENCODING_VERSION: u8 = 1;

/// payload 与 Trait 输出共用的值。
#[derive(Debug, Clone)]
pub enum CanonicalValue {
    /// 空值。Lua 里是 `nil`。
    Null,
    /// 布尔。
    Bool(bool),
    /// 有符号整数。
    Int(i64),
    /// 浮点。
    Float(f64),
    /// UTF-8 文本。
    String(String),
    /// 不透明字节。
    Bytes(Vec<u8>),
    /// 有序列表。
    List(Vec<CanonicalValue>),
    /// 键有序的映射。键只能是字符串。
    Map(BTreeMap<String, CanonicalValue>),
    /// 实体引用。内核唯一认识的语义值。
    Ref(EntityId),
    /// 大二进制内容的摘要。
    Blob(BlobHash),
    /// 混合逻辑时钟。
    Hlc(Hlc),
}

impl PartialEq for CanonicalValue {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Null, Self::Null) => true,
            (Self::Bool(left), Self::Bool(right)) => left == right,
            (Self::Int(left), Self::Int(right)) => left == right,
            (Self::Float(left), Self::Float(right)) => float_bits(*left) == float_bits(*right),
            (Self::String(left), Self::String(right)) => left == right,
            (Self::Bytes(left), Self::Bytes(right)) => left == right,
            (Self::List(left), Self::List(right)) => left == right,
            (Self::Map(left), Self::Map(right)) => left == right,
            (Self::Ref(left), Self::Ref(right)) => left == right,
            (Self::Blob(left), Self::Blob(right)) => left == right,
            (Self::Hlc(left), Self::Hlc(right)) => left == right,
            _ => false,
        }
    }
}

impl CanonicalValue {
    /// 建一个空映射。
    pub fn map() -> Self {
        Self::Map(BTreeMap::new())
    }

    /// 读一个键。不是映射时返回 `None`。
    pub fn get(&self, key: &str) -> Option<&Self> {
        match self {
            Self::Map(map) => map.get(key),
            _ => None,
        }
    }

    /// 取映射本身。
    pub fn as_map(&self) -> Option<&BTreeMap<String, Self>> {
        match self {
            Self::Map(map) => Some(map),
            _ => None,
        }
    }

    /// 取映射本身，可变。
    pub fn as_map_mut(&mut self) -> Option<&mut BTreeMap<String, Self>> {
        match self {
            Self::Map(map) => Some(map),
            _ => None,
        }
    }

    /// 规范化字节。相同语义值永远得到相同字节。
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.push(VALUE_ENCODING_VERSION);
        encode_value(self, &mut out);
        out
    }

    /// 从 [`CanonicalValue::encode`] 的产物还原。拒绝非规范编码、尾随字节与重复键。
    pub fn decode(bytes: &[u8]) -> Result<Self, CoreError> {
        let (version, rest) = bytes.split_first().ok_or(CoreError::CanonicalDecode)?;
        if *version != VALUE_ENCODING_VERSION {
            return Err(CoreError::CanonicalDecode);
        }
        let mut offset = 0;
        let value = decode_value(rest, &mut offset, 0)?;
        if offset != rest.len() {
            return Err(CoreError::CanonicalDecode);
        }
        Ok(value)
    }
}

const TAG_NULL: u8 = 0;
const TAG_FALSE: u8 = 1;
const TAG_TRUE: u8 = 2;
const TAG_INT: u8 = 3;
const TAG_FLOAT: u8 = 4;
const TAG_STRING: u8 = 5;
const TAG_BYTES: u8 = 6;
const TAG_LIST: u8 = 7;
const TAG_MAP: u8 = 8;
const TAG_REF: u8 = 9;
const TAG_BLOB: u8 = 10;
const TAG_HLC: u8 = 11;

fn encode_value(value: &CanonicalValue, out: &mut Vec<u8>) {
    match value {
        CanonicalValue::Null => out.push(TAG_NULL),
        CanonicalValue::Bool(false) => out.push(TAG_FALSE),
        CanonicalValue::Bool(true) => out.push(TAG_TRUE),
        CanonicalValue::Int(number) => {
            out.push(TAG_INT);
            encode_varint(zigzag(*number), out);
        }
        CanonicalValue::Float(number) => {
            out.push(TAG_FLOAT);
            out.extend_from_slice(&float_bits(*number).to_be_bytes());
        }
        CanonicalValue::String(text) => {
            out.push(TAG_STRING);
            encode_len(text.len(), out);
            out.extend_from_slice(text.as_bytes());
        }
        CanonicalValue::Bytes(bytes) => {
            out.push(TAG_BYTES);
            encode_len(bytes.len(), out);
            out.extend_from_slice(bytes);
        }
        CanonicalValue::List(items) => {
            out.push(TAG_LIST);
            encode_len(items.len(), out);
            for item in items {
                encode_value(item, out);
            }
        }
        CanonicalValue::Map(map) => {
            out.push(TAG_MAP);
            encode_len(map.len(), out);
            for (key, item) in map {
                encode_len(key.len(), out);
                out.extend_from_slice(key.as_bytes());
                encode_value(item, out);
            }
        }
        CanonicalValue::Ref(entity) => {
            out.push(TAG_REF);
            out.extend_from_slice(entity.as_bytes());
        }
        CanonicalValue::Blob(hash) => {
            out.push(TAG_BLOB);
            out.extend_from_slice(hash.as_bytes());
        }
        CanonicalValue::Hlc(hlc) => {
            out.push(TAG_HLC);
            out.extend_from_slice(&hlc.physical_ms.to_be_bytes());
            out.extend_from_slice(&hlc.logical.to_be_bytes());
            out.extend_from_slice(&hlc.node_id.to_be_bytes());
        }
    }
}

/// 解码的最大嵌套深度。值来自同步进来的块，不能让对端用深嵌套把本机栈打爆。
pub const MAX_VALUE_DEPTH: usize = 128;

fn decode_value(
    input: &[u8],
    offset: &mut usize,
    depth: usize,
) -> Result<CanonicalValue, CoreError> {
    if depth > MAX_VALUE_DEPTH {
        return Err(CoreError::CanonicalDecode);
    }
    let tag = *input.get(*offset).ok_or(CoreError::CanonicalDecode)?;
    *offset += 1;
    match tag {
        TAG_NULL => Ok(CanonicalValue::Null),
        TAG_FALSE => Ok(CanonicalValue::Bool(false)),
        TAG_TRUE => Ok(CanonicalValue::Bool(true)),
        TAG_INT => {
            let raw = decode_varint(input, offset)?;
            Ok(CanonicalValue::Int(unzigzag(raw)))
        }
        TAG_FLOAT => {
            let mut bytes = [0u8; 8];
            bytes.copy_from_slice(take(input, offset, 8)?);
            Ok(CanonicalValue::Float(f64::from_bits(u64::from_be_bytes(
                bytes,
            ))))
        }
        TAG_STRING => {
            let len = decode_len(input, offset)?;
            let bytes = take(input, offset, len)?;
            let text = std::str::from_utf8(bytes).map_err(|_| CoreError::CanonicalDecode)?;
            Ok(CanonicalValue::String(text.to_owned()))
        }
        TAG_BYTES => {
            let len = decode_len(input, offset)?;
            Ok(CanonicalValue::Bytes(take(input, offset, len)?.to_vec()))
        }
        TAG_LIST => {
            let len = decode_len(input, offset)?;
            let mut items = Vec::with_capacity(len.min(1024));
            for _ in 0..len {
                items.push(decode_value(input, offset, depth + 1)?);
            }
            Ok(CanonicalValue::List(items))
        }
        TAG_MAP => {
            let len = decode_len(input, offset)?;
            let mut map = BTreeMap::new();
            let mut previous: Option<String> = None;
            for _ in 0..len {
                let key_len = decode_len(input, offset)?;
                let key_bytes = take(input, offset, key_len)?;
                let key = std::str::from_utf8(key_bytes).map_err(|_| CoreError::CanonicalDecode)?;
                if previous.as_deref().is_some_and(|prev| prev >= key) {
                    return Err(CoreError::CanonicalDecode);
                }
                previous = Some(key.to_owned());
                let value = decode_value(input, offset, depth + 1)?;
                map.insert(key.to_owned(), value);
            }
            Ok(CanonicalValue::Map(map))
        }
        TAG_REF => {
            let mut bytes = [0u8; 16];
            bytes.copy_from_slice(take(input, offset, 16)?);
            let entity = EntityId::try_from_bytes(bytes).map_err(|_| CoreError::CanonicalDecode)?;
            Ok(CanonicalValue::Ref(entity))
        }
        TAG_BLOB => {
            let mut bytes = [0u8; 32];
            bytes.copy_from_slice(take(input, offset, 32)?);
            Ok(CanonicalValue::Blob(BlobHash::from_bytes(bytes)))
        }
        TAG_HLC => {
            let mut physical = [0u8; 8];
            physical.copy_from_slice(take(input, offset, 8)?);
            let mut logical = [0u8; 2];
            logical.copy_from_slice(take(input, offset, 2)?);
            let mut node = [0u8; 4];
            node.copy_from_slice(take(input, offset, 4)?);
            Ok(CanonicalValue::Hlc(Hlc {
                physical_ms: u64::from_be_bytes(physical),
                logical: u16::from_be_bytes(logical),
                node_id: u32::from_be_bytes(node),
            }))
        }
        _ => Err(CoreError::CanonicalDecode),
    }
}

fn take<'a>(input: &'a [u8], offset: &mut usize, len: usize) -> Result<&'a [u8], CoreError> {
    let end = offset.checked_add(len).ok_or(CoreError::CanonicalDecode)?;
    let slice = input.get(*offset..end).ok_or(CoreError::CanonicalDecode)?;
    *offset = end;
    Ok(slice)
}

fn encode_len(len: usize, out: &mut Vec<u8>) {
    encode_varint(len as u64, out);
}

fn decode_len(input: &[u8], offset: &mut usize) -> Result<usize, CoreError> {
    let len =
        usize::try_from(decode_varint(input, offset)?).map_err(|_| CoreError::CanonicalDecode)?;
    Ok(len)
}

fn zigzag(value: i64) -> u64 {
    ((value << 1) ^ (value >> 63)) as u64
}

fn unzigzag(value: u64) -> i64 {
    ((value >> 1) as i64) ^ -((value & 1) as i64)
}

fn encode_varint(mut value: u64, out: &mut Vec<u8>) {
    loop {
        let byte = (value & 0x7F) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// 拒绝超长编码（例如 `0x80 0x00`），否则同一个值会有两种字节表示。
fn decode_varint(input: &[u8], offset: &mut usize) -> Result<u64, CoreError> {
    let mut value = 0u64;
    let mut shift = 0u32;
    let mut count = 0usize;
    loop {
        let byte = *input.get(*offset).ok_or(CoreError::CanonicalDecode)?;
        *offset += 1;
        count += 1;
        if count > 10 {
            return Err(CoreError::CanonicalDecode);
        }
        let bits = u64::from(byte & 0x7F);
        if count == 10 && bits > 1 {
            return Err(CoreError::CanonicalDecode);
        }
        if shift >= 64 {
            return Err(CoreError::CanonicalDecode);
        }
        value |= bits << shift;
        if byte & 0x80 == 0 {
            if count > 1 && byte == 0 {
                return Err(CoreError::CanonicalDecode);
            }
            return Ok(value);
        }
        shift += 7;
    }
}

fn float_bits(value: f64) -> u64 {
    if value.is_nan() {
        0x7ff8_0000_0000_0000
    } else if value == 0.0 {
        0
    } else {
        value.to_bits()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(value: CanonicalValue) -> Vec<u8> {
        let bytes = value.encode();
        assert_eq!(CanonicalValue::decode(&bytes).unwrap(), value);
        bytes
    }

    #[test]
    fn map_key_order_does_not_change_the_bytes() {
        let mut left = BTreeMap::new();
        left.insert("b".to_owned(), CanonicalValue::Int(2));
        left.insert("a".to_owned(), CanonicalValue::Int(1));
        let mut right = BTreeMap::new();
        right.insert("a".to_owned(), CanonicalValue::Int(1));
        right.insert("b".to_owned(), CanonicalValue::Int(2));
        assert_eq!(
            CanonicalValue::Map(left).encode(),
            CanonicalValue::Map(right).encode()
        );
    }

    #[test]
    fn floats_normalise_zero_and_nan() {
        assert_eq!(float_bits(-0.0), float_bits(0.0));
        assert_eq!(float_bits(f64::NAN), float_bits(-f64::NAN));
        assert_eq!(CanonicalValue::Float(-0.0), CanonicalValue::Float(0.0));
    }

    #[test]
    fn all_value_kinds_round_trip() {
        let entity = EntityId::new();
        let value = CanonicalValue::Map(BTreeMap::from([
            ("null".to_owned(), CanonicalValue::Null),
            ("bool".to_owned(), CanonicalValue::Bool(true)),
            ("int".to_owned(), CanonicalValue::Int(i64::MIN)),
            ("float".to_owned(), CanonicalValue::Float(1.5)),
            ("text".to_owned(), CanonicalValue::String("你好".to_owned())),
            ("bytes".to_owned(), CanonicalValue::Bytes(vec![0, 255])),
            (
                "list".to_owned(),
                CanonicalValue::List(vec![
                    CanonicalValue::Int(1),
                    CanonicalValue::String("x".to_owned()),
                ]),
            ),
            ("ref".to_owned(), CanonicalValue::Ref(entity)),
            (
                "blob".to_owned(),
                CanonicalValue::Blob(BlobHash::from_bytes([7; 32])),
            ),
            (
                "hlc".to_owned(),
                CanonicalValue::Hlc(Hlc {
                    physical_ms: 1,
                    logical: 2,
                    node_id: 3,
                }),
            ),
        ]));
        let bytes = encode(value);
        assert_eq!(bytes[0], VALUE_ENCODING_VERSION);
    }

    #[test]
    fn malformed_bytes_are_rejected() {
        assert!(CanonicalValue::decode(&[]).is_err());
        assert!(CanonicalValue::decode(&[VALUE_ENCODING_VERSION]).is_err());
        assert!(CanonicalValue::decode(&[VALUE_ENCODING_VERSION, TAG_NULL, TAG_NULL]).is_err());
        // 超长 varint。
        assert!(CanonicalValue::decode(&[VALUE_ENCODING_VERSION, TAG_INT, 0x80, 0x00]).is_err());
        // 未知 tag。
        assert!(CanonicalValue::decode(&[VALUE_ENCODING_VERSION, 0x7F]).is_err());
    }

    #[test]
    fn duplicate_or_unsorted_map_keys_are_rejected() {
        let mut bytes = vec![VALUE_ENCODING_VERSION, TAG_MAP, 2];
        for _ in 0..2 {
            bytes.push(1);
            bytes.push(b'a');
            bytes.push(TAG_NULL);
        }
        assert!(CanonicalValue::decode(&bytes).is_err());
    }
}

#[cfg(test)]
mod depth_tests {
    use super::*;

    #[test]
    fn deep_nesting_is_rejected_without_overflowing_the_stack() {
        let mut value = CanonicalValue::Null;
        for _ in 0..MAX_VALUE_DEPTH + 10 {
            value = CanonicalValue::List(vec![value]);
        }
        let bytes = value.encode();
        assert!(CanonicalValue::decode(&bytes).is_err());

        // 手工拼出远超限制的嵌套，确认不会栈溢出。
        let list = CanonicalValue::List(vec![CanonicalValue::Null]).encode();
        let mut evil = vec![list[0]];
        let prefix = &list[1..list.len() - 1];
        for _ in 0..200_000 {
            evil.extend_from_slice(prefix);
        }
        evil.push(list[list.len() - 1]);
        assert!(CanonicalValue::decode(&evil).is_err());
    }
}
