use super::aead;
use super::keys::ContentKey;
use super::CryptoError;

/// 帧里装的是逻辑块还是 Blob。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameKind {
    /// 逻辑块的规范字节。
    Block,
    /// 大二进制。
    Blob,
}

/// 解开一帧之后的明文。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenedFrame {
    /// 种类。
    pub kind: FrameKind,
    /// 压缩和加密都去掉之后的字节。
    pub plaintext: Vec<u8>,
}

/// 帧的压缩和 AEAD。内容密钥总是存在，帧格式不随用户是否保护主密钥而改变。
pub struct FrameCodec {
    content: ContentKey,
}

impl FrameCodec {
    pub(in crate::crypto) fn new(content: ContentKey) -> Self {
        Self { content }
    }

    /// 先用 `zrip` 压成 Zstd 帧，再 ChaCha20-Poly1305。认证数据是标志字节。
    pub fn seal(&self, kind: FrameKind, plaintext: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let compressed =
            zrip::compress(plaintext, ZSTD_LEVEL).map_err(|_| CryptoError::Compress)?;
        let flags = FrameFlags {
            kind,
            compress: Compress::Zstd,
            aead: AeadAlg::ChaCha20Poly1305,
        };
        let flag_byte = flags.to_byte();
        let sealed = aead::seal(self.content.as_bytes(), &[flag_byte], &compressed)?;
        let mut out = Vec::with_capacity(1 + 12 + sealed.ciphertext.len());
        out.push(flag_byte);
        out.extend_from_slice(&sealed.nonce);
        out.extend_from_slice(&sealed.ciphertext);
        Ok(out)
    }

    /// 先核对标志位，再解 AEAD，最后解 Zstd。
    pub fn open(&self, frame: &[u8]) -> Result<OpenedFrame, CryptoError> {
        let (flags, rest) = split_flags(frame)?;
        if !matches!(flags.aead, AeadAlg::ChaCha20Poly1305) {
            return Err(CryptoError::BadFlags);
        }
        if rest.len() < 12 + 16 {
            return Err(CryptoError::Truncated);
        }
        let (nonce, ciphertext) = rest.split_at(12);
        let nonce: [u8; 12] = nonce.try_into().expect("split_at yields 12 bytes");
        let compressed = aead::open(
            self.content.as_bytes(),
            &[flags.to_byte()],
            &nonce,
            ciphertext,
        )?;
        let plaintext = match flags.compress {
            Compress::None => compressed,
            Compress::Zstd => zrip::decompress(&compressed).map_err(|_| CryptoError::Decompress)?,
        };
        Ok(OpenedFrame {
            kind: flags.kind,
            plaintext,
        })
    }
}

/// `zrip` 的级别范围是 -7 到 4。3 落在压缩率更好的一档，产出的仍是标准 Zstd 帧。
const ZSTD_LEVEL: i32 = 3;

#[derive(Clone, Copy)]
enum Compress {
    None,
    Zstd,
}

#[derive(Clone, Copy)]
enum AeadAlg {
    None,
    ChaCha20Poly1305,
}

#[derive(Clone, Copy)]
struct FrameFlags {
    kind: FrameKind,
    compress: Compress,
    aead: AeadAlg,
}

impl FrameFlags {
    fn to_byte(self) -> u8 {
        let kind = match self.kind {
            FrameKind::Block => 1,
            FrameKind::Blob => 2,
        };
        let compress = match self.compress {
            Compress::None => 0,
            Compress::Zstd => 1,
        };
        let aead = match self.aead {
            AeadAlg::None => 0,
            AeadAlg::ChaCha20Poly1305 => 1,
        };
        kind | (compress << 2) | (aead << 4)
    }
}

fn split_flags(frame: &[u8]) -> Result<(FrameFlags, &[u8]), CryptoError> {
    let (&flag_byte, rest) = frame.split_first().ok_or(CryptoError::Truncated)?;
    if flag_byte & 0b1100_0000 != 0 {
        return Err(CryptoError::BadFlags);
    }
    let kind = match flag_byte & 0b11 {
        1 => FrameKind::Block,
        2 => FrameKind::Blob,
        _ => return Err(CryptoError::BadFlags),
    };
    let compress = match (flag_byte >> 2) & 0b11 {
        0 => Compress::None,
        1 => Compress::Zstd,
        _ => return Err(CryptoError::BadFlags),
    };
    let aead = match (flag_byte >> 4) & 0b11 {
        0 => AeadAlg::None,
        1 => AeadAlg::ChaCha20Poly1305,
        _ => return Err(CryptoError::BadFlags),
    };
    Ok((
        FrameFlags {
            kind,
            compress,
            aead,
        },
        rest,
    ))
}
