use serde::{Deserialize, Serialize};

/// 索引项：上层 schema 声明，core 只存键，不解释字段含义。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexKey(pub Vec<u8>);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexEntry {
    pub index: String,
    pub key: Vec<u8>,
    pub record_key: Vec<u8>,
}

/// 索引键编码：`idx/<name>/<key> -> rec/<id>`，扫描即查询。
pub fn index_key(index: &str, key: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + index.len() + 1 + key.len());
    out.extend_from_slice(b"idx/");
    out.extend_from_slice(index.as_bytes());
    out.push(b'/');
    out.extend_from_slice(key);
    out
}
