use thiserror::Error;

/// 密码设施的失败。不把算法库的错误类型漏出去。
#[derive(Debug, Error)]
pub enum CryptoError {
    /// 头部版本不是本库能读的。
    #[error("unsupported crypto header version")]
    UnsupportedVersion,
    /// 主密钥的存放方式和这次操作不一致。
    #[error("master key custody mismatch")]
    CustodyMismatch,
    /// 头部字段组合不合法，或设备公钥和私钥对不上。
    #[error("corrupt crypto header")]
    Corrupt,
    /// 主密钥、包裹或标签校验失败。
    #[error("key unwrap failed")]
    Unwrap,
    /// 全零之类不能当密钥的材料。
    #[error("weak key")]
    WeakKey,
    /// X25519 得到了全零共享秘密。
    #[error("weak shared secret")]
    WeakSharedSecret,
    /// 帧标志位不认识，或保留位被置上。
    #[error("bad frame flags")]
    BadFlags,
    /// 字节比这一层要求的最短长度还短。
    #[error("truncated crypto blob")]
    Truncated,
    /// AEAD 加密失败。
    #[error("encrypt failed")]
    Encrypt,
    /// AEAD 标签校验失败，或页被换到了别的页号。
    #[error("decrypt failed")]
    Decrypt,
    /// `zrip` 压缩失败。
    #[error("compress failed")]
    Compress,
    /// Zstd 帧解压失败。
    #[error("decompress failed")]
    Decompress,
}
