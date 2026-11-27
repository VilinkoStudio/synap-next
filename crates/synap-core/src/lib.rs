//! 共享原语。不碰磁盘、网络、Lua 和 FFI。
#![forbid(unsafe_code)]

mod block;
mod codec;
mod error;
mod id;
mod key;
mod sync;
mod value;

pub use block::{Block, BlockHeader};
pub use codec::BLOCK_ENCODING_VERSION;
pub use error::CoreError;
pub use id::{BlobHash, BlockHash, EntityId, Hlc, LocalSeq, ProjNamespace, SchemaId, TraitId};
pub use key::{
    encode_prefix, exclusive_prefix_end, namespace_prefix, Direction, IndexKey, KeyComponent,
    KeyScalar,
};
pub use sync::{sync_bucket_hash, MerkleDigest, SyncId};
pub use value::{CanonicalValue, VALUE_ENCODING_VERSION};
