use thiserror::Error;

/// 全系统共用的底层错误。store 与 runtime 按需包装，不另起同义词。
#[derive(Debug, Error, PartialEq, Eq)]
pub enum CoreError {
    /// 字节不是 UUIDv7。
    #[error("invalid entity id")]
    InvalidEntityId,
    /// `SchemaId` 不是 `namespace/name`。
    #[error("invalid schema id: {0}")]
    InvalidSchemaId(String),
    /// `TraitId` 的名字不是 `namespace/name`。
    #[error("invalid trait id: {0}")]
    InvalidTraitId(String),
    /// `postcard` 规范编码失败。
    #[error("canonical encoding failed")]
    CanonicalEncode,
    /// 字节不是本版本的规范块。
    #[error("canonical decoding failed")]
    CanonicalDecode,
    /// 有序键的字节对不上编码。
    #[error("invalid ordered key")]
    InvalidKey,
}
