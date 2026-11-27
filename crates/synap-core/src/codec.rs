use serde::{Deserialize, Serialize};

use crate::{Block, BlockHash, CoreError};

/// 块格式版本（文档里的 `format`）。放在 `postcard` 字节的第一个字段，参与哈希。
/// 改布局就加一，并且解码要继续认得所有旧版本——块永远不改写。
/// 原型期允许破坏性修改，升级靠明文导出与导入。
pub const BLOCK_ENCODING_VERSION: u8 = 1;

/// 只为哈希存在的布局。字段顺序就是协议，不要重排，也不要改成 `serde_bytes`。
#[derive(Serialize)]
struct CanonicalBlock<'a> {
    encoding_version: u8,
    entity_id: &'a [u8; 16],
    parents: &'a [[u8; 32]],
    schema_id: &'a str,
    schema_version: &'a [u8; 32],
    physical_ms: u64,
    logical: u16,
    node_id: u32,
    is_tombstone: bool,
    payload: &'a [u8],
}

/// 解码用的布局。字段顺序必须和 [`CanonicalBlock`] 一致。
#[derive(Deserialize)]
struct OwnedCanonical {
    encoding_version: u8,
    entity_id: [u8; 16],
    parents: Vec<[u8; 32]>,
    schema_id: String,
    schema_version: [u8; 32],
    physical_ms: u64,
    logical: u16,
    node_id: u32,
    is_tombstone: bool,
    payload: Vec<u8>,
}

impl Block {
    /// 规范编码使用的父块：字典序，且没有重复。
    pub fn sorted_parents(&self) -> Vec<BlockHash> {
        let mut parents = self.header.parents.clone();
        parents.sort_unstable();
        parents.dedup();
        parents
    }

    /// `postcard` 规范字节。`parents` 按字典序排序并去重后再编码。
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, CoreError> {
        let parents = self.sorted_parents();
        let parents: Vec<[u8; 32]> = parents.iter().map(|parent| *parent.as_bytes()).collect();
        let canonical = CanonicalBlock {
            encoding_version: BLOCK_ENCODING_VERSION,
            entity_id: self.header.entity_id.as_bytes(),
            parents: &parents,
            schema_id: self.header.schema_id.as_str(),
            schema_version: self.header.schema_version.as_bytes(),
            physical_ms: self.header.hlc.physical_ms,
            logical: self.header.hlc.logical,
            node_id: self.header.hlc.node_id,
            is_tombstone: self.header.is_tombstone,
            payload: &self.payload,
        };
        postcard::to_allocvec(&canonical).map_err(|_| CoreError::CanonicalEncode)
    }

    /// 从 [`Block::canonical_bytes`] 的产物还原。父块必须已经按字典序排好且没有重复。
    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self, CoreError> {
        let (owned, rest): (OwnedCanonical, &[u8]) =
            postcard::take_from_bytes(bytes).map_err(|_| CoreError::CanonicalDecode)?;
        if !rest.is_empty() {
            return Err(CoreError::CanonicalDecode);
        }
        if owned.encoding_version != BLOCK_ENCODING_VERSION {
            return Err(CoreError::CanonicalDecode);
        }
        if owned.parents.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(CoreError::CanonicalDecode);
        }
        let header = crate::BlockHeader {
            entity_id: crate::EntityId::try_from_bytes(owned.entity_id)
                .map_err(|_| CoreError::CanonicalDecode)?,
            parents: owned
                .parents
                .into_iter()
                .map(BlockHash::from_bytes)
                .collect(),
            schema_id: crate::SchemaId::parse(&owned.schema_id)
                .map_err(|_| CoreError::CanonicalDecode)?,
            schema_version: BlockHash::from_bytes(owned.schema_version),
            hlc: crate::Hlc {
                physical_ms: owned.physical_ms,
                logical: owned.logical,
                node_id: owned.node_id,
            },
            is_tombstone: owned.is_tombstone,
        };
        Ok(Self {
            header,
            payload: owned.payload,
        })
    }

    /// `BLAKE3(canonical_bytes)`。
    pub fn hash(&self) -> Result<BlockHash, CoreError> {
        let bytes = self.canonical_bytes()?;
        Ok(BlockHash::from_bytes(*blake3::hash(&bytes).as_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EntityId, Hlc, SchemaId};

    fn sample(parents: Vec<BlockHash>, payload: &[u8]) -> Block {
        let entity_id = EntityId::try_from_bytes([
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x70, 0x08, 0x80, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
            0x0f, 0x10,
        ])
        .unwrap();
        Block {
            header: crate::BlockHeader {
                entity_id,
                parents,
                schema_id: SchemaId::parse("sys/plugin").unwrap(),
                schema_version: BlockHash::ZERO,
                hlc: Hlc {
                    physical_ms: 0,
                    logical: 0,
                    node_id: 0,
                },
                is_tombstone: false,
            },
            payload: payload.to_vec(),
        }
    }

    #[test]
    fn parent_order_and_duplicates_do_not_change_the_hash() {
        let a = BlockHash::from_bytes([0x11; 32]);
        let b = BlockHash::from_bytes([0x22; 32]);
        let left = sample(vec![b, a, b], b"hi");
        let right = sample(vec![a, b], b"hi");
        assert_eq!(left.hash().unwrap(), right.hash().unwrap());
        assert_eq!(left.canonical_bytes().unwrap()[0], BLOCK_ENCODING_VERSION);
    }

    #[test]
    fn payload_changes_the_hash() {
        let left = sample(Vec::new(), b"hi");
        let right = sample(Vec::new(), b"ho");
        assert_ne!(left.hash().unwrap(), right.hash().unwrap());
    }

    #[test]
    fn canonical_bytes_roundtrip_and_reject_a_bad_version() {
        let block = sample(vec![BlockHash::from_bytes([0x22; 32])], b"hi");
        let bytes = block.canonical_bytes().unwrap();
        let restored = Block::from_canonical_bytes(&bytes).unwrap();
        assert_eq!(restored, block);
        let mut bad = bytes;
        bad[0] = 2;
        assert_eq!(
            Block::from_canonical_bytes(&bad),
            Err(CoreError::CanonicalDecode)
        );
    }
}
