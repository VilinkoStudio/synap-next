use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SchemaError {
    #[error("unknown plugin: {0}")]
    UnknownPlugin(String),
    #[error("invalid action: {0}")]
    InvalidAction(String),
}

/// 插件声明自己的结构：有哪些记录、索引、投影、动作。
/// 内核只存字节，这里的字段是插件自己的事。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginSchema {
    pub id: String,
    pub version: u32,
    pub kinds: Vec<String>,
    pub indexes: Vec<String>,
    pub projections: Vec<String>,
}
