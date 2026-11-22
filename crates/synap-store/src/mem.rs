use std::collections::BTreeMap;
use std::sync::RwLock;

use synap_core::{CoreError, Kv, KvPairs};

/// 测试 / 预览用内存引擎：行为与文件引擎一致，数据不落地。
#[derive(Debug, Default)]
pub struct MemKv {
    inner: RwLock<BTreeMap<Vec<u8>, Vec<u8>>>,
}

impl MemKv {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Kv for MemKv {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, CoreError> {
        self.inner
            .read()
            .map_err(|e| CoreError::Storage(e.to_string()))
            .map(|m| m.get(key).cloned())
    }

    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), CoreError> {
        self.inner
            .write()
            .map_err(|e| CoreError::Storage(e.to_string()))
            .map(|mut m| {
                m.insert(key.to_vec(), value.to_vec());
            })
    }

    fn delete(&self, key: &[u8]) -> Result<(), CoreError> {
        self.inner
            .write()
            .map_err(|e| CoreError::Storage(e.to_string()))
            .map(|mut m| {
                m.remove(key);
            })
    }

    fn scan_prefix(&self, prefix: &[u8]) -> Result<KvPairs, CoreError> {
        self.inner
            .read()
            .map_err(|e| CoreError::Storage(e.to_string()))
            .map(|m| {
                m.range(prefix.to_vec()..)
                    .take_while(|(k, _)| k.starts_with(prefix))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect()
            })
    }
}
