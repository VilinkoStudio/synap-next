//! synap-store：下层，允许按平台分叉，允许以后换实现。
//! 对上只暴露同一套 `Kv` 读写，不出现在插件作者的 API 里。

mod mem;
mod redb_store;

pub use mem::MemKv;
pub use redb_store::RedbKv;
