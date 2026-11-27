//! 投影索引的保序键。`BTreeMap` 和 `redb` 都比较这些字节。
//!
//! 一个索引键是：投影身份、零个或多个排序分量、最后的实体标识。
//! 实体标识收尾，所以同一排序键下每个实体只有一行，前缀扫描也不会把实体标识当成下一个分量。
//! 降序分量是升序编码的按位反码，字节序和逻辑序因此相反。
//!
//! 字符串和字节里的 `0x00` 写成 `0x00 0xFF`，再用 `0x00 0x00` 结束，这样拼接之后字典序不变。
//! 有符号整数先把符号位翻转，再按大端写出。

use crate::{CoreError, EntityId, ProjNamespace};

/// 排序方向。同一个投影里，同一位置的方向对所有行都一样。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// 小的在前。
    Ascending,
    /// 大的在前。编码是升序字节的反码。
    Descending,
}

/// 排序分量里的标量。列表和映射是多个分量，不塞进一个标量。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyScalar {
    /// 空。升序时排在其它标量前面。
    Null,
    /// 布尔。`false` 在 `true` 前。
    Bool(bool),
    /// 有符号整数。
    Int(i64),
    /// 文本。按 Unicode 码点顺序，也就是 UTF-8 字节序。
    Text(String),
    /// 不透明字节。
    Bytes(Vec<u8>),
    /// 实体引用。内核在键里认识的唯一语义类型。
    Ref(EntityId),
}

/// 一个排序分量。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyComponent {
    /// 这一位的方向。
    pub direction: Direction,
    /// 这一位的值。
    pub scalar: KeyScalar,
}

impl KeyComponent {
    /// 升序分量。
    pub fn asc(scalar: KeyScalar) -> Self {
        Self {
            direction: Direction::Ascending,
            scalar,
        }
    }

    /// 降序分量。
    pub fn desc(scalar: KeyScalar) -> Self {
        Self {
            direction: Direction::Descending,
            scalar,
        }
    }
}

/// 一行投影索引的逻辑键。不含投影身份；身份在编码时分开传入。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexKey {
    /// 管道产出的排序键。
    pub parts: Vec<KeyComponent>,
    /// 这一行属于哪个实体。
    pub entity: EntityId,
}

impl IndexKey {
    /// 组装一行的逻辑键。
    pub fn new(parts: Vec<KeyComponent>, entity: EntityId) -> Self {
        Self { parts, entity }
    }

    /// 编码成可直接放进有序树的字节。投影名字为空时失败。
    pub fn encode(&self, namespace: &ProjNamespace) -> Result<Vec<u8>, CoreError> {
        let mut out = encode_prefix(namespace, &self.parts)?;
        out.extend_from_slice(self.entity.as_bytes());
        Ok(out)
    }

    /// 从 [`IndexKey::encode`] 的产物还原。
    pub fn decode(bytes: &[u8]) -> Result<(ProjNamespace, Self), CoreError> {
        let (namespace, rest) = split_namespace(bytes)?;
        if rest.len() < ENTITY_LEN {
            return Err(CoreError::InvalidKey);
        }
        let (body, entity_bytes) = rest.split_at(rest.len() - ENTITY_LEN);
        let mut parts = Vec::new();
        let mut offset = 0;
        while offset < body.len() {
            let (part, used) = decode_component(&body[offset..])?;
            offset += used;
            parts.push(part);
        }
        let entity_bytes: [u8; ENTITY_LEN] = entity_bytes
            .try_into()
            .expect("split leaves 16 entity bytes");
        let entity = EntityId::try_from_bytes(entity_bytes).map_err(|_| CoreError::InvalidKey)?;
        Ok((namespace, Self { parts, entity }))
    }
}

/// 投影身份加上若干排序分量，不含收尾的实体标识。前缀扫描用它当起点。
pub fn encode_prefix(
    namespace: &ProjNamespace,
    parts: &[KeyComponent],
) -> Result<Vec<u8>, CoreError> {
    let mut out = namespace_prefix(namespace)?;
    for part in parts {
        out.extend(encode_component(part));
    }
    Ok(out)
}

/// 一个投影在有序树里的起点。该投影的全部索引键都带这个前缀。
pub fn namespace_prefix(namespace: &ProjNamespace) -> Result<Vec<u8>, CoreError> {
    if namespace.proj_id.is_empty() {
        return Err(CoreError::InvalidKey);
    }
    let mut out = encode_terminated(namespace.proj_id.as_bytes());
    out.extend_from_slice(&namespace.def_hash);
    Ok(out)
}

/// `prefix` 的排他上界。全部为 `0xFF` 时没有上界。
pub fn exclusive_prefix_end(prefix: &[u8]) -> Option<Vec<u8>> {
    let mut end = prefix.to_vec();
    while let Some(last) = end.pop() {
        if last != 0xFF {
            end.push(last + 1);
            return Some(end);
        }
    }
    None
}

const ENTITY_LEN: usize = 16;
const TAG_NULL: u8 = 0x01;
const TAG_BOOL: u8 = 0x02;
const TAG_INT: u8 = 0x03;
const TAG_TEXT: u8 = 0x04;
const TAG_BYTES: u8 = 0x05;
const TAG_REF: u8 = 0x06;

fn encode_component(part: &KeyComponent) -> Vec<u8> {
    let mut raw = vec![tag_of(&part.scalar)];
    match &part.scalar {
        KeyScalar::Null => {}
        KeyScalar::Bool(value) => raw.push(u8::from(*value)),
        KeyScalar::Int(value) => raw.extend(encode_i64(*value)),
        KeyScalar::Text(value) => raw.extend(encode_terminated(value.as_bytes())),
        KeyScalar::Bytes(value) => raw.extend(encode_terminated(value)),
        KeyScalar::Ref(entity) => raw.extend_from_slice(entity.as_bytes()),
    }
    if part.direction == Direction::Descending {
        for byte in &mut raw {
            *byte = !*byte;
        }
    }
    raw
}

fn tag_of(scalar: &KeyScalar) -> u8 {
    match scalar {
        KeyScalar::Null => TAG_NULL,
        KeyScalar::Bool(_) => TAG_BOOL,
        KeyScalar::Int(_) => TAG_INT,
        KeyScalar::Text(_) => TAG_TEXT,
        KeyScalar::Bytes(_) => TAG_BYTES,
        KeyScalar::Ref(_) => TAG_REF,
    }
}

fn encode_i64(value: i64) -> [u8; 8] {
    ((value as u64) ^ (1u64 << 63)).to_be_bytes()
}

fn decode_i64(bytes: [u8; 8]) -> i64 {
    (u64::from_be_bytes(bytes) ^ (1u64 << 63)) as i64
}

fn encode_terminated(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 2);
    for byte in data {
        if *byte == 0 {
            out.extend_from_slice(&[0, 0xFF]);
        } else {
            out.push(*byte);
        }
    }
    out.extend_from_slice(&[0, 0]);
    out
}

fn decode_component(input: &[u8]) -> Result<(KeyComponent, usize), CoreError> {
    let tag = *input.first().ok_or(CoreError::InvalidKey)?;
    let (direction, logical_tag) = if matches_tag(tag) {
        (Direction::Ascending, tag)
    } else if matches_tag(!tag) {
        (Direction::Descending, !tag)
    } else {
        return Err(CoreError::InvalidKey);
    };
    let (scalar, used) = decode_scalar(logical_tag, &input[1..], direction)?;
    Ok((KeyComponent { direction, scalar }, used + 1))
}

fn matches_tag(tag: u8) -> bool {
    (TAG_NULL..=TAG_REF).contains(&tag)
}

fn logical_byte(byte: u8, direction: Direction) -> u8 {
    match direction {
        Direction::Ascending => byte,
        Direction::Descending => !byte,
    }
}

fn decode_scalar(
    tag: u8,
    input: &[u8],
    direction: Direction,
) -> Result<(KeyScalar, usize), CoreError> {
    match tag {
        TAG_NULL => Ok((KeyScalar::Null, 0)),
        TAG_BOOL => {
            let byte = logical_byte(*input.first().ok_or(CoreError::InvalidKey)?, direction);
            let value = match byte {
                0 => false,
                1 => true,
                _ => return Err(CoreError::InvalidKey),
            };
            Ok((KeyScalar::Bool(value), 1))
        }
        TAG_INT => {
            if input.len() < 8 {
                return Err(CoreError::InvalidKey);
            }
            let mut bytes = [0u8; 8];
            for (slot, byte) in bytes.iter_mut().zip(input.iter().take(8)) {
                *slot = logical_byte(*byte, direction);
            }
            Ok((KeyScalar::Int(decode_i64(bytes)), 8))
        }
        TAG_TEXT => {
            let (bytes, used) = decode_terminated(input, direction)?;
            let text = String::from_utf8(bytes).map_err(|_| CoreError::InvalidKey)?;
            Ok((KeyScalar::Text(text), used))
        }
        TAG_BYTES => {
            let (bytes, used) = decode_terminated(input, direction)?;
            Ok((KeyScalar::Bytes(bytes), used))
        }
        TAG_REF => {
            if input.len() < ENTITY_LEN {
                return Err(CoreError::InvalidKey);
            }
            let mut bytes = [0u8; ENTITY_LEN];
            for (slot, byte) in bytes.iter_mut().zip(input.iter().take(ENTITY_LEN)) {
                *slot = logical_byte(*byte, direction);
            }
            let entity = EntityId::try_from_bytes(bytes).map_err(|_| CoreError::InvalidKey)?;
            Ok((KeyScalar::Ref(entity), ENTITY_LEN))
        }
        _ => Err(CoreError::InvalidKey),
    }
}

fn decode_terminated(input: &[u8], direction: Direction) -> Result<(Vec<u8>, usize), CoreError> {
    let mut out = Vec::new();
    let mut index = 0;
    while index + 1 < input.len() {
        let first = logical_byte(input[index], direction);
        let second = logical_byte(input[index + 1], direction);
        if first == 0 && second == 0 {
            return Ok((out, index + 2));
        }
        if first == 0 && second == 0xFF {
            out.push(0);
            index += 2;
            continue;
        }
        if first == 0 {
            return Err(CoreError::InvalidKey);
        }
        out.push(first);
        index += 1;
    }
    Err(CoreError::InvalidKey)
}

fn split_namespace(bytes: &[u8]) -> Result<(ProjNamespace, &[u8]), CoreError> {
    let (proj_bytes, used) = decode_terminated(bytes, Direction::Ascending)?;
    let proj_id = String::from_utf8(proj_bytes).map_err(|_| CoreError::InvalidKey)?;
    if proj_id.is_empty() {
        return Err(CoreError::InvalidKey);
    }
    let rest = bytes.get(used..).ok_or(CoreError::InvalidKey)?;
    if rest.len() < 32 {
        return Err(CoreError::InvalidKey);
    }
    let (hash, tail) = rest.split_at(32);
    let def_hash = hash.try_into().expect("split leaves 32 hash bytes");
    Ok((ProjNamespace { proj_id, def_hash }, tail))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eid(n: u8) -> EntityId {
        let mut bytes = [0u8; 16];
        bytes[6] = 0x70;
        bytes[8] = 0x80;
        bytes[15] = n;
        EntityId::try_from_bytes(bytes).unwrap()
    }

    fn ns(name: &str, mark: u8) -> ProjNamespace {
        ProjNamespace {
            proj_id: name.to_owned(),
            def_hash: [mark; 32],
        }
    }

    fn roundtrip(namespace: &ProjNamespace, key: &IndexKey) {
        let encoded = key.encode(namespace).unwrap();
        let (decoded_ns, decoded) = IndexKey::decode(&encoded).unwrap();
        assert_eq!(&decoded_ns, namespace);
        assert_eq!(&decoded, key);
        assert!(encoded.starts_with(&namespace_prefix(namespace).unwrap()));
    }

    #[test]
    fn encoded_order_matches_logical_order() {
        let namespace = ns("todo/open", 1);
        let keys = [
            IndexKey::new(vec![KeyComponent::asc(KeyScalar::Null)], eid(1)),
            IndexKey::new(vec![KeyComponent::asc(KeyScalar::Bool(false))], eid(1)),
            IndexKey::new(vec![KeyComponent::asc(KeyScalar::Bool(true))], eid(1)),
            IndexKey::new(vec![KeyComponent::asc(KeyScalar::Int(-2))], eid(1)),
            IndexKey::new(vec![KeyComponent::asc(KeyScalar::Int(-1))], eid(1)),
            IndexKey::new(vec![KeyComponent::asc(KeyScalar::Int(0))], eid(1)),
            IndexKey::new(vec![KeyComponent::asc(KeyScalar::Int(1))], eid(1)),
            IndexKey::new(
                vec![
                    KeyComponent::asc(KeyScalar::Int(1)),
                    KeyComponent::asc(KeyScalar::Text("a".into())),
                ],
                eid(1),
            ),
            IndexKey::new(
                vec![
                    KeyComponent::asc(KeyScalar::Int(1)),
                    KeyComponent::asc(KeyScalar::Text("b".into())),
                ],
                eid(1),
            ),
            IndexKey::new(
                vec![
                    KeyComponent::asc(KeyScalar::Int(1)),
                    KeyComponent::asc(KeyScalar::Text("b".into())),
                ],
                eid(2),
            ),
            IndexKey::new(vec![KeyComponent::asc(KeyScalar::Int(2))], eid(1)),
            IndexKey::new(vec![KeyComponent::asc(KeyScalar::Text("a".into()))], eid(1)),
            IndexKey::new(
                vec![KeyComponent::asc(KeyScalar::Text("a\0".into()))],
                eid(1),
            ),
            IndexKey::new(
                vec![KeyComponent::asc(KeyScalar::Text("aa".into()))],
                eid(1),
            ),
            IndexKey::new(vec![KeyComponent::asc(KeyScalar::Text("b".into()))], eid(1)),
            IndexKey::new(vec![KeyComponent::asc(KeyScalar::Bytes(vec![0]))], eid(1)),
            IndexKey::new(vec![KeyComponent::asc(KeyScalar::Bytes(vec![1]))], eid(1)),
            IndexKey::new(vec![KeyComponent::asc(KeyScalar::Ref(eid(1)))], eid(1)),
            IndexKey::new(vec![KeyComponent::asc(KeyScalar::Ref(eid(2)))], eid(1)),
        ];
        for key in &keys {
            roundtrip(&namespace, key);
        }
        let mut encoded: Vec<_> = keys
            .iter()
            .map(|key| key.encode(&namespace).unwrap())
            .collect();
        let sorted = encoded.clone();
        encoded.sort();
        assert_eq!(encoded, sorted);

        let desc = [
            IndexKey::new(
                vec![KeyComponent::desc(KeyScalar::Text("b".into()))],
                eid(1),
            ),
            IndexKey::new(
                vec![KeyComponent::desc(KeyScalar::Text("a\0".into()))],
                eid(1),
            ),
            IndexKey::new(
                vec![KeyComponent::desc(KeyScalar::Text("a".into()))],
                eid(1),
            ),
            IndexKey::new(vec![KeyComponent::desc(KeyScalar::Int(10))], eid(1)),
            IndexKey::new(vec![KeyComponent::desc(KeyScalar::Int(3))], eid(1)),
            IndexKey::new(vec![KeyComponent::desc(KeyScalar::Int(1))], eid(1)),
        ];
        for key in &desc {
            roundtrip(&namespace, key);
        }
        let mut desc_bytes: Vec<_> = desc
            .iter()
            .map(|key| key.encode(&namespace).unwrap())
            .collect();
        let desc_sorted = desc_bytes.clone();
        desc_bytes.sort();
        assert_eq!(desc_bytes, desc_sorted);
    }

    #[test]
    fn namespaces_do_not_interleave() {
        let left = ns("todo/open", 1);
        let right = ns("todo/open", 2);
        let key = IndexKey::new(vec![KeyComponent::asc(KeyScalar::Int(0))], eid(1));
        assert!(key.encode(&left).unwrap() < key.encode(&right).unwrap());
        let other = ns("wiki/recent", 1);
        assert_ne!(
            namespace_prefix(&left).unwrap(),
            namespace_prefix(&other).unwrap()
        );
        assert_ne!(key.encode(&left).unwrap(), key.encode(&other).unwrap());
    }

    #[test]
    fn prefix_end_stops_at_the_boundary() {
        assert_eq!(exclusive_prefix_end(&[0x01, 0x02]), Some(vec![0x01, 0x03]));
        assert_eq!(exclusive_prefix_end(&[0x01, 0xFF]), Some(vec![0x02]));
        assert_eq!(exclusive_prefix_end(&[0xFF]), None);
    }

    #[test]
    fn garbage_is_rejected() {
        assert_eq!(IndexKey::decode(&[0x01]), Err(CoreError::InvalidKey));
        let empty = ProjNamespace {
            proj_id: String::new(),
            def_hash: [0; 32],
        };
        let key = IndexKey::new(Vec::new(), eid(1));
        assert_eq!(key.encode(&empty), Err(CoreError::InvalidKey));
    }
}
