use thiserror::Error;

use crate::crypto::CryptoError;

/// 卷和页后端的失败。密码错误仍用 [`CryptoError`] 表达。
#[derive(Debug, Error)]
pub enum StoreError {
    /// 解头部、页槽或帧失败。
    #[error(transparent)]
    Crypto(#[from] CryptoError),
    /// 读写卷文件失败。
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// `redb` 拒绝了这次操作，或页槽解不开。
    #[error(transparent)]
    Database(#[from] redb::Error),
    /// 卷头魔数、长度或页槽边界不合法。
    #[error("corrupt volume")]
    CorruptVolume,
    /// 卷文件上的锁被污染。
    #[error("volume lock poisoned")]
    Poison,
    /// 规范块编解码失败。
    #[error(transparent)]
    Core(#[from] synap_core::CoreError),
    /// 父块还没入库。
    #[error("parent block is missing")]
    MissingParent,
    /// 父块属于另一个实体。
    #[error("parent belongs to another entity")]
    ParentEntity,
    /// 父块的 `schema_id` 和这块不一样。
    #[error("parent schema does not match")]
    SchemaMismatch,
    /// 帧解开了，但不是一块规范块。
    #[error("frame is not a canonical block")]
    BadFrame,
    /// 导出目录里的清单或块文件不对。
    #[error("export file: {0}")]
    Export(String),
    /// 表里的头列表或序号对不上格式。
    #[error("stored value is corrupt")]
    CorruptValue,
    /// `local_seq` 已经到 `u64::MAX`。
    #[error("local sequence exhausted")]
    SeqExhausted,
    /// 库里记下的投影水位不是这次提交声明的起点。
    #[error("projection checkpoint is {stored}, expected {expected}")]
    CheckpointMismatch {
        /// 已经落盘的水位。
        stored: synap_core::LocalSeq,
        /// 调用方认为的起点。
        expected: synap_core::LocalSeq,
    },
    /// 新水位比起点小。
    #[error("projection checkpoint moved backwards")]
    CheckpointRewind,
    /// 行有变化，水位却停在原地。
    #[error("projection rows changed without a new checkpoint")]
    CheckpointStalled,
    /// 序号区间不在到达日志里。
    #[error("sequence range is outside the arrival log")]
    SeqRange,
    /// 追平还没结束，驱动又调用了摄入或追平。
    #[error("projection driver re-entered ingest")]
    Reentrant,
    /// 一轮里同一个投影写了两次。
    #[error("a catch-up round writes the same projection twice")]
    DuplicateNamespace,
    /// 帧解开之后的内容和声称的同步身份对不上。
    #[error("frame does not match the claimed sync id")]
    SyncMismatch,
}

impl From<redb::DatabaseError> for StoreError {
    fn from(err: redb::DatabaseError) -> Self {
        Self::Database(err.into())
    }
}

impl From<redb::TransactionError> for StoreError {
    fn from(err: redb::TransactionError) -> Self {
        Self::Database(err.into())
    }
}

impl From<redb::CommitError> for StoreError {
    fn from(err: redb::CommitError) -> Self {
        Self::Database(err.into())
    }
}

impl From<redb::TableError> for StoreError {
    fn from(err: redb::TableError) -> Self {
        Self::Database(err.into())
    }
}

impl From<redb::StorageError> for StoreError {
    fn from(err: redb::StorageError) -> Self {
        Self::Database(err.into())
    }
}
