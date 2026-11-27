//! 同步身份和分桶摘要。摘要的字节由这里定死，卷里只保存算好的桶。

use serde::{Deserialize, Serialize};

use crate::BlockHash;

/// 块在同步里的身份。`BLAKE3_keyed(sync_key, BlockHash)`。
///
/// 同一个库的设备算出同一个值。没有 `sync_key` 的一方不能从它还原 `BlockHash`。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SyncId([u8; 32]);

impl SyncId {
    /// 从已有的 32 字节还原。不检查它是不是某把密钥派出来的。
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// 原始 32 字节。
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// 用库的同步密钥从内容哈希派生。
    pub fn derive(block: &BlockHash, key: &[u8; 32]) -> Self {
        Self(*blake3::keyed_hash(key, block.as_bytes()).as_bytes())
    }
}

impl std::fmt::Debug for SyncId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SyncId(")?;
        for byte in &self.0[..4] {
            write!(f, "{byte:02x}")?;
        }
        write!(f, "…)")
    }
}

/// 一套身份的分桶摘要。读的时候直接用，不用再扫全部块。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MerkleDigest {
    /// 集合里的身份个数。
    pub count: u64,
    /// 256 个桶再哈希一次。
    pub root: [u8; 32],
    /// 下标就是身份的首字节。
    pub buckets: Vec<[u8; 32]>,
}

impl MerkleDigest {
    /// 由已经存好的 256 个桶拼出摘要。
    pub fn from_bucket_hashes(count: u64, buckets: [[u8; 32]; 256]) -> Self {
        Self {
            count,
            root: root_hash(&buckets),
            buckets: buckets.to_vec(),
        }
    }

    /// 从完整身份集合现算。卷的增量结果必须和这个一致。
    pub fn from_ids(ids: impl IntoIterator<Item = SyncId>) -> Self {
        let mut grouped = vec![Vec::new(); 256];
        let mut count = 0u64;
        for id in ids {
            count += 1;
            grouped[id.as_bytes()[0] as usize].push(id);
        }
        let mut buckets = [[0u8; 32]; 256];
        for (prefix, ids) in grouped.iter_mut().enumerate() {
            buckets[prefix] = sync_bucket_hash(prefix as u8, ids);
        }
        Self::from_bucket_hashes(count, buckets)
    }

    /// 两边桶摘要不同的首字节。长度不是 256 时视为全部不同。
    pub fn diff_buckets(&self, other: &Self) -> Vec<u8> {
        if self.buckets.len() != 256 || other.buckets.len() != 256 {
            return (0..=255).collect();
        }
        (0..=255u8)
            .filter(|prefix| self.buckets[*prefix as usize] != other.buckets[*prefix as usize])
            .collect()
    }
}

/// 一个桶的摘要。身份会先按字节排序，调用方不必排好。
pub fn sync_bucket_hash(prefix: u8, ids: &[SyncId]) -> [u8; 32] {
    let mut ordered = ids.to_vec();
    ordered.sort();
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"synap-sync-bucket-v1");
    hasher.update(&[prefix]);
    for id in ordered {
        hasher.update(id.as_bytes());
    }
    *hasher.finalize().as_bytes()
}

fn root_hash(buckets: &[[u8; 32]; 256]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"synap-sync-root-v1");
    for bucket in buckets {
        hasher.update(bucket);
    }
    *hasher.finalize().as_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u8) -> SyncId {
        let mut bytes = [0u8; 32];
        bytes[0] = n;
        bytes[31] = n;
        SyncId(bytes)
    }

    #[test]
    fn empty_sets_agree() {
        let left = MerkleDigest::from_ids([]);
        let right = MerkleDigest::from_ids([]);
        assert!(left.diff_buckets(&right).is_empty());
    }

    #[test]
    fn one_extra_id_shows_one_bucket() {
        let left = MerkleDigest::from_ids([id(7)]);
        let right = MerkleDigest::from_ids([id(7), id(200)]);
        assert_eq!(left.diff_buckets(&right), vec![200]);
    }

    #[test]
    fn derive_depends_on_the_key() {
        let hash = BlockHash::from_bytes([9; 32]);
        assert_ne!(
            SyncId::derive(&hash, &[1; 32]),
            SyncId::derive(&hash, &[2; 32])
        );
    }
}
