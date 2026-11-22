use crate::{CoreError, Record};

/// 扫描结果：键值字节对列表。
pub type KvPairs = Vec<(Vec<u8>, Vec<u8>)>;

/// KV：腰部对下层的唯一读写口。
/// 写入路径短，按键追加或覆盖，不维护关系表。
pub trait Kv: Send + Sync {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, CoreError>;
    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), CoreError>;
    fn delete(&self, key: &[u8]) -> Result<(), CoreError>;

    /// 前缀扫描，索引和投影重算都走这里，不做全表以外的花活。
    fn scan_prefix(&self, prefix: &[u8]) -> Result<KvPairs, CoreError>;
    // 同步占位：搬运的是已提交版本，不理解业务。
    // 具体 SyncCursor / relay 等以后再加，先只留版本号语义。
}

/// 记录级便捷键：`rec/<uuid>`，版本信息放在 value 里（Record 自带 version）。
pub fn record_key(id: &crate::RecordId) -> Vec<u8> {
    format!("rec/{}", id.0).into_bytes()
}

pub fn encode_record(record: &Record) -> Result<Vec<u8>, CoreError> {
    serde_json::to_vec(record).map_err(|e| CoreError::Codec(e.to_string()))
}

pub fn decode_record(bytes: &[u8]) -> Result<Record, CoreError> {
    serde_json::from_slice(bytes).map_err(|e| CoreError::Codec(e.to_string()))
}
