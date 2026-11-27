//! 字节层。
//!
//! 读一条块的顺序从外到内，写的时候反过来：
//! 1. 固定 4096 字节的卷头，在页加密之外。主密钥要么调用方保管，要么明文写在这里。
//! 2. 页槽：[`PageCodec`]，密钥由主密钥派生。
//! 3. `redb` 页。
//! 4. 帧：[`FrameCodec`] 先解 AEAD，再用 `zrip` 解 Zstd。
//!
//! 内容密钥始终独立。换主密钥的存放方式不用重写页。
#![forbid(unsafe_code)]

mod backend;
mod crypto;
mod error;
mod export;
mod heads;
mod ingest;
mod projection;
mod round;
mod sync_index;
mod tables;
mod volume;

pub use crypto::{
    Created, CryptoError, CryptoHeader, DeviceId, FrameCodec, FrameKind, MasterCustody, MasterKey,
    OpenedFrame, PageCodec, Session, WrappedSecret, CRYPTO_HEADER_VERSION,
};
pub use error::StoreError;
pub use export::{export_blocks, import_blocks, ExportStats, ImportStats};
pub use ingest::{Arrival, Ingested, Snapshot};
pub use projection::{KeyBound, KeyWindow, ProjChange, ProjPage, ProjRow, ScanDir};
pub use round::{EntityUnit, NamespaceWrite, ProjectionDriver, Watch, WatchEvent, World};
pub use tables::{BLOCKS, ENTITY_HEADS, LOCAL_SEQ, META, PROJECTIONS};
pub use volume::{CreatedVolume, Volume};
