//! synap-core：沙漏的腰，只回答数据库问题.
//!
//! 不许依赖 FFI（flutter_rust_bridge / uniffi / dart:ffi 相关都不许出现在这里）。
//! 不认 Todo / Wiki / Loam 等业务字段，那些是 `synap-runtime` 的 schema.

pub mod error;
pub mod index;
pub mod kv;
pub mod projection;
pub mod record;

pub use error::CoreError;
pub use index::{IndexEntry, IndexKey};
pub use kv::{decode_record, encode_record, record_key, Kv, KvPairs};
pub use projection::{ProjectionCache, Projector};
pub use record::{Record, RecordId, Version};
