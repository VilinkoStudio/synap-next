use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 稳定标识一条记录。内核只保证唯一，不解释业务含义。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RecordId(pub Uuid);

impl RecordId {
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }
}

impl Default for RecordId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for RecordId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// 版本：单调序号 + 写入设备。同步搬运的就是这个，不理解业务。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Version {
    pub seq: u64,
    pub device: String,
}

/// 中层记录：id + 版本 + 不透明 payload。
/// payload 的结构由上层 schema 解释，core 只做字节搬运。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub id: RecordId,
    pub version: Version,
    pub kind: String,
    pub payload: Vec<u8>,
}
