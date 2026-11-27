//! synap-relay：盲中继独立服务，无密钥、无解密、无 runtime。
//!
//! 只存 `SyncId -> 密文帧` 不透明集合，在该集合上维护 Merkle 摘要参与对账。
//! 不依赖 `synap-store`（那是端侧加密库）；存储后端可换：内存 / Redis。
//! 同步走集合并集，幂等无租约；邮箱退为一次性信令通道（配对交密钥/唤醒）。
//!
//! ```text
//! 设备 <--REST digest/diff/offer/want/batch--> relay
//! 设备 <---WebSocket /v1/sync/ws（双向对账）---> relay
//! 设备 <--mailbox（一次性密封信令）--> relay
//! ```

pub mod auth;
pub mod http;
pub mod mailbox;
pub mod proto;
pub mod store;
pub use store::{MemoryRelayStore, RedisRelayStore, RelayBackend, RelayStore};

pub use auth::RelayAuth;
pub use http::{build_router, AppState};
pub use mailbox::MailboxStore;
