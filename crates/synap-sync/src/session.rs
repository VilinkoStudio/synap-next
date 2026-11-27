use serde::{Deserialize, Serialize};

use crate::merkle::{MerkleDigest, SyncId};

/// Sans-IO 会话：纯状态机，不碰网络、不碰 store、不碰密钥。
///
/// 调用方（synap-node / 客户端后台任务）负责传输字节与落盘；
/// 这里只回答“缺什么”。集合并集幂等，重复推送无害（008 §三）。
#[derive(Debug)]
pub struct SyncSession {
    local: crate::merkle::SyncIdSet,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WantList {
    pub ids: Vec<SyncId>,
}

impl WantList {
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("bad digest: {0}")]
    BadDigest(String),
    #[error("protocol: {0}")]
    Protocol(String),
    /// 卷拒绝了这次读或写。
    #[error("store: {0}")]
    Store(String),
}

impl SyncSession {
    pub fn new(local: crate::merkle::SyncIdSet) -> Self {
        Self { local }
    }

    /// 本端摘要，发给对端。
    pub fn local_digest(&self) -> MerkleDigest {
        self.local.digest()
    }

    /// 对端发来它的全量 ID 列表（小库直连时），算出本端缺的。
    /// 大库走分桶：先 `diff_buckets` 定桶，再只交换桶内 ID。
    pub fn missing_from_offer(&self, offered: &[SyncId]) -> Result<WantList, SyncError> {
        let mut ids = Vec::new();
        for id in offered {
            if !self.local.contains(id) {
                ids.push(*id);
            }
        }
        Ok(WantList { ids })
    }

    /// 对端按桶索要本端桶内 ID 时，用此前缀过滤。
    pub fn ids_in_buckets(&self, buckets: &[u8]) -> Vec<SyncId> {
        self.local
            .iter()
            .filter(|id| buckets.contains(&id.as_bytes()[0]))
            .copied()
            .collect()
    }

    /// 收到一帧后的自证由 store 做（解密→重算 BlockHash→核对 SyncId，
    /// 见 005 §3）；这里只做会话侧的记账钩子，转发者无法注入有效块。
    pub fn note_received(&mut self, id: SyncId) {
        self.local.insert(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::merkle::SyncIdSet;

    fn id(n: u8) -> SyncId {
        SyncId::from_bytes({
            let mut b = [0u8; 32];
            b[0] = n;
            b
        })
    }

    #[test]
    fn missing_is_set_difference() {
        let mut local = SyncIdSet::new();
        local.insert(id(1));
        let s = SyncSession::new(local);
        let want = s.missing_from_offer(&[id(1), id(2)]).unwrap();
        assert_eq!(want.ids, vec![id(2)]);
    }
}
