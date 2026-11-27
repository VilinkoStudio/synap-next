use std::fmt;

use crate::CoreError;

/// BLAKE3 摘要。对 `Block` 的规范字节计算，与压缩、加密、nonce 无关。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlockHash([u8; 32]);

impl BlockHash {
    /// 全零。仅作创世约定：`schema_version` 为此值且 `schema_id` 为 `sys/plugin` 时，payload 是插件源码。
    pub const ZERO: Self = Self([0; 32]);

    /// 从已有摘要构造。不校验它是否真由某个块算出。
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// 摘要字节。
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// 是否为 [`BlockHash::ZERO`]。
    pub fn is_zero(self) -> bool {
        self.0 == [0; 32]
    }
}

impl fmt::Debug for BlockHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "BlockHash(")?;
        fmt_hex(&self.0, f)?;
        write!(f, ")")
    }
}

impl fmt::Display for BlockHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt_hex(&self.0, f)
    }
}

/// 大二进制内容的 BLAKE3 摘要。快照里按值引用，不内嵌字节。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlobHash([u8; 32]);

impl BlobHash {
    /// 从已有摘要构造。
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// 摘要字节。
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for BlobHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "BlobHash(")?;
        fmt_hex(&self.0, f)?;
        write!(f, ")")
    }
}

impl fmt::Display for BlobHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt_hex(&self.0, f)
    }
}

/// 实体标识。16 字节 UUIDv7：高位是生成时的 Unix 毫秒，只用于局部性，不作为业务时间。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EntityId([u8; 16]);

impl EntityId {
    /// 用本机时钟生成一个 UUIDv7。
    ///
    /// 不实现 `Default`：默认值每次都会变成另一个标识。
    #[expect(
        clippy::new_without_default,
        reason = "默认值会在每次调用时生成新的随机标识"
    )]
    pub fn new() -> Self {
        Self(*uuid::Uuid::now_v7().as_bytes())
    }

    /// 从 16 字节还原。版本位不是 v7，或 variant 不是 RFC 4122，则拒绝。
    pub fn try_from_bytes(bytes: [u8; 16]) -> Result<Self, CoreError> {
        if bytes[6] >> 4 != 0x7 || bytes[8] & 0b1100_0000 != 0b1000_0000 {
            return Err(CoreError::InvalidEntityId);
        }
        Ok(Self(bytes))
    }

    /// 原始 16 字节。
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl fmt::Debug for EntityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "EntityId({})", uuid::Uuid::from_bytes(self.0))
    }
}

impl fmt::Display for EntityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&uuid::Uuid::from_bytes(self.0), f)
    }
}

impl std::str::FromStr for EntityId {
    type Err = CoreError;

    /// 接受标准 UUID 文本。版本位不是 v7 时拒绝。
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let uuid = uuid::Uuid::parse_str(text).map_err(|_| CoreError::InvalidEntityId)?;
        Self::try_from_bytes(*uuid.as_bytes())
    }
}

/// Schema 的稳定名义身份，形如 `namespace/name`。同一实体终身不变。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SchemaId(String);

impl SchemaId {
    /// 解析 `namespace/name`。两侧都非空，名字里不再含 `/`，也不含空白。
    pub fn parse(name: &str) -> Result<Self, CoreError> {
        if !is_qualified_name(name) {
            return Err(CoreError::InvalidSchemaId(name.to_owned()));
        }
        Ok(Self(name.to_owned()))
    }

    /// 名义身份原文。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SchemaId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Trait 身份。只有大版本参与身份，小版本须向后兼容。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TraitId {
    name: String,
    major: u16,
}

impl TraitId {
    /// `name` 的形状与 [`SchemaId`] 相同，例如 `std/Checkable` 配 `major = 1`。
    pub fn parse(name: &str, major: u16) -> Result<Self, CoreError> {
        if !is_qualified_name(name) {
            return Err(CoreError::InvalidTraitId(name.to_owned()));
        }
        Ok(Self {
            name: name.to_owned(),
            major,
        })
    }

    /// 名字，不含 `@major`。
    pub fn name(&self) -> &str {
        &self.name
    }

    /// 大版本。
    pub const fn major(&self) -> u16 {
        self.major
    }
}

impl fmt::Display for TraitId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.name, self.major)
    }
}

/// 本机单调到达序号。只在本机增长，不参与同步。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LocalSeq(u64);

impl LocalSeq {
    /// 序号 0。库里还没有块时的水位。
    pub const ZERO: Self = Self(0);

    /// 由已落盘的序号构造。
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// 数值。
    pub const fn get(self) -> u64 {
        self.0
    }

    /// 下一条。`u64::MAX` 时返回 `None`。
    pub const fn checked_next(self) -> Option<Self> {
        match self.0.checked_add(1) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }
}

impl fmt::Display for LocalSeq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

/// 混合逻辑时钟。排序先看 `physical_ms`，再看 `logical`，最后用 `node_id` 做平局。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Hlc {
    /// Unix 毫秒。
    pub physical_ms: u64,
    /// 同一毫秒内的逻辑计数。
    pub logical: u16,
    /// 产生此戳的节点。只参与平局，不是业务时钟。
    pub node_id: u32,
}

/// 投影的物理身份：名字加上计算定义的摘要。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProjNamespace {
    /// 投影名字。
    pub proj_id: String,
    /// 定义哈希，涵盖 Trait 版本、Impl 所在插件、算子与 Host API。
    pub def_hash: [u8; 32],
}

fn is_qualified_name(name: &str) -> bool {
    let Some((namespace, rest)) = name.split_once('/') else {
        return false;
    };
    !namespace.is_empty()
        && !rest.is_empty()
        && !rest.contains('/')
        && !namespace.chars().any(char::is_whitespace)
        && !rest.chars().any(char::is_whitespace)
}

fn fmt_hex(bytes: &[u8], f: &mut fmt::Formatter<'_>) -> fmt::Result {
    for byte in bytes {
        write!(f, "{byte:02x}")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entity_id_new_is_uuid_v7() {
        let id = EntityId::new();
        assert_eq!(EntityId::try_from_bytes(*id.as_bytes()).unwrap(), id);
    }

    #[test]
    fn entity_id_rejects_random_bytes() {
        assert_eq!(
            EntityId::try_from_bytes([0; 16]),
            Err(CoreError::InvalidEntityId)
        );
    }

    #[test]
    fn schema_id_shape() {
        assert!(SchemaId::parse("sys/plugin").is_ok());
        assert!(SchemaId::parse("vilinko.todo/item").is_ok());
        assert!(SchemaId::parse("plugin").is_err());
        assert!(SchemaId::parse("/plugin").is_err());
        assert!(SchemaId::parse("sys/").is_err());
        assert!(SchemaId::parse("sys/a/b").is_err());
        assert!(SchemaId::parse("sys/plu gin").is_err());
    }

    #[test]
    fn hlc_orders_by_physical_then_logical_then_node() {
        let base = Hlc {
            physical_ms: 10,
            logical: 1,
            node_id: 1,
        };
        let later_ms = Hlc {
            physical_ms: 11,
            logical: 0,
            node_id: 0,
        };
        let later_logical = Hlc {
            physical_ms: 10,
            logical: 2,
            node_id: 0,
        };
        let later_node = Hlc {
            physical_ms: 10,
            logical: 1,
            node_id: 2,
        };
        assert!(base < later_ms);
        assert!(base < later_logical);
        assert!(base < later_node);
    }

    #[test]
    fn local_seq_stops_at_max() {
        assert_eq!(LocalSeq::new(u64::MAX).checked_next(), None);
        assert_eq!(LocalSeq::ZERO.checked_next(), Some(LocalSeq::new(1)));
    }
}
