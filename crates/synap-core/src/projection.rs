use std::collections::HashMap;
use std::sync::RwLock;

use crate::{CoreError, Record};

/// 响应式投影：底层记录变了，投影结果跟着变。
/// 缓存是优化，丢了能重算，不做第二份真相。
pub trait Projector {
    type View: Clone + Send + Sync;
    fn name(&self) -> &str;
    fn project(&self, records: &[Record]) -> Result<Self::View, CoreError>;
}

/// 最小的内存缓存：版本推进即失效，丢了重算。
pub struct ProjectionCache<V: Clone> {
    inner: RwLock<HashMap<String, V>>,
}

impl<V: Clone> Default for ProjectionCache<V> {
    fn default() -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
        }
    }
}

impl<V: Clone> ProjectionCache<V> {
    pub fn get(&self, name: &str) -> Option<V> {
        self.inner.read().ok()?.get(name).cloned()
    }

    pub fn insert(&self, name: &str, view: V) {
        if let Ok(mut guard) = self.inner.write() {
            guard.insert(name.to_string(), view);
        }
    }

    pub fn invalidate(&self, name: &str) {
        if let Ok(mut guard) = self.inner.write() {
            guard.remove(name);
        }
    }
}
