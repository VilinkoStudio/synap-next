//! synap-sync：同步的一轮对账。
//!
//! 卷里保存分桶摘要。这里向两边要摘要、算出差集、交换帧。
//! 不绑定 tokio。盲中继仍然只用 [`SyncIdSet`]，不打开卷。

mod exchange;
mod merkle;
mod session;

pub use exchange::{sync_with, SyncEndpoint, SyncReport, VolumeEndpoint};
pub use merkle::{MerkleDigest, SyncId, SyncIdSet};
pub use session::{SyncError, SyncSession, WantList};
