use thiserror::Error;

/// 腰部唯一的错误面：存储和投影失败。不按业务分。
#[derive(Debug, Error)]
pub enum CoreError {
    #[error("storage: {0}")]
    Storage(String),
    #[error("record not found: {0}")]
    NotFound(String),
    #[error("projection: {0}")]
    Projection(String),
    #[error("codec: {0}")]
    Codec(String),
}
