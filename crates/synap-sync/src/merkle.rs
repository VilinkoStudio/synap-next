//! 本端已持有的同步身份。桶摘要的算法在 `synap-core`，这里只保留集合。

use std::collections::BTreeSet;

pub use synap_core::{MerkleDigest, SyncId};

/// 本端已持有的 SyncId 集合。有序，Merkle 摘要直接在其上算。
#[derive(Debug, Default, Clone)]
pub struct SyncIdSet {
    inner: BTreeSet<SyncId>,
}

impl SyncIdSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, id: SyncId) -> bool {
        self.inner.insert(id)
    }

    pub fn contains(&self, id: &SyncId) -> bool {
        self.inner.contains(id)
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &SyncId> {
        self.inner.iter()
    }

    /// 按首字节分 256 桶。和卷里增量维护的摘要是同一套字节。
    pub fn digest(&self) -> MerkleDigest {
        MerkleDigest::from_ids(self.inner.iter().copied())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u8) -> SyncId {
        let mut bytes = [0u8; 32];
        bytes[0] = n;
        bytes[31] = n;
        SyncId::from_bytes(bytes)
    }

    #[test]
    fn empty_sets_agree() {
        let left = SyncIdSet::new();
        let right = SyncIdSet::new();
        assert!(left.digest().diff_buckets(&right.digest()).is_empty());
    }

    #[test]
    fn one_extra_id_shows_one_bucket() {
        let mut left = SyncIdSet::new();
        let mut right = SyncIdSet::new();
        left.insert(id(7));
        right.insert(id(7));
        right.insert(id(200));
        assert_eq!(left.digest().diff_buckets(&right.digest()), vec![200]);
    }
}
