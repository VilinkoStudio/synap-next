use crate::{BlockHash, EntityId, Hlc, SchemaId};

/// 参与哈希的逻辑头。压缩、nonce、AEAD 不在这里。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockHeader {
    /// 这个块属于哪个实体。
    pub entity_id: EntityId,
    /// 祖先哈希。规范编码时会排序去重，调用方不必事先排好。
    pub parents: Vec<BlockHash>,
    /// 实体终身不变的名义身份。
    pub schema_id: SchemaId,
    /// 定义该 Schema 的插件源码块哈希。全零表示创世约定，见 [`BlockHash::ZERO`]。
    pub schema_version: BlockHash,
    /// 跨端偏序。
    pub hlc: Hlc,
    /// 逻辑删除。v1 的删除只追加这样一块。
    pub is_tombstone: bool,
}

/// 逻辑块。`payload` 在 v1 是自包含快照，core 不解读它。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    /// 逻辑头。
    pub header: BlockHeader,
    /// 不透明快照字节。
    pub payload: Vec<u8>,
}
