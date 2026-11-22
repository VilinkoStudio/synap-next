use serde::{Deserialize, Serialize};
use synap_core::{
    decode_record, encode_record, record_key, CoreError, Kv, Record, RecordId, Version,
};

use crate::schema::{PluginSchema, SchemaError};

/// Flutter 发来的动作：指明插件 + 载荷，具体校验由 schema 定。
/// 第一版只做最小写入通路：put_record / delete_record。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Action {
    pub plugin: String,
    pub name: String,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionResult {
    pub record_id: Option<RecordId>,
    pub version: Version,
}

/// 运行时把 schema 接到原语上：校验写入、维护索引和投影、管权限。
/// 插件之间只通过约定的引用和查询交互，不读对方内部表。
pub struct Runtime<K: Kv> {
    kv: K,
    device: String,
    seq: std::sync::atomic::AtomicU64,
    schemas: std::sync::RwLock<Vec<PluginSchema>>,
}

impl<K: Kv> Runtime<K> {
    pub fn new(kv: K, device: String) -> Self {
        Self {
            kv,
            device,
            seq: std::sync::atomic::AtomicU64::new(0),
            schemas: std::sync::RwLock::new(Vec::new()),
        }
    }

    pub fn register(&self, schema: PluginSchema) -> Result<(), SchemaError> {
        self.schemas
            .write()
            .map_err(|_| SchemaError::InvalidAction("schema lock".to_string()))?
            .push(schema);
        Ok(())
    }

    fn next_version(&self) -> Version {
        let seq = self.seq.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        Version {
            seq,
            device: self.device.clone(),
        }
    }

    /// 一次写入：校验 -> 写中层记录 -> 调投影失效（调用方做）-> KV 落盘。
    /// 同步不在 UI 线程做，只搬运已提交版本。
    pub fn dispatch(&self, action: Action) -> Result<ActionResult, CoreError> {
        match action.name.as_str() {
            "put_record" => {
                let mut record: Record = serde_json::from_slice(&action.payload)
                    .map_err(|e| CoreError::Codec(e.to_string()))?;
                record.version = self.next_version();
                let id = record.id;
                let key = record_key(&id);
                let bytes = encode_record(&record)?;
                self.kv.put(&key, &bytes)?;
                Ok(ActionResult {
                    record_id: Some(id),
                    version: record.version,
                })
            }
            "get_record" => {
                let id: RecordId = serde_json::from_slice(&action.payload)
                    .map_err(|e| CoreError::Codec(e.to_string()))?;
                let key = record_key(&id);
                let bytes = self
                    .kv
                    .get(&key)?
                    .ok_or_else(|| CoreError::NotFound(id.to_string()))?;
                let record = decode_record(&bytes)?;
                Ok(ActionResult {
                    record_id: Some(record.id),
                    version: record.version,
                })
            }
            other => Err(CoreError::Storage(format!("unknown action: {other}"))),
        }
    }
}
