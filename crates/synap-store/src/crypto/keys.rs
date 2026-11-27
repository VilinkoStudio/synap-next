use std::fmt;

use hkdf::Hkdf;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use zeroize::{Zeroize, ZeroizeOnDrop};

use super::aead;
use super::CryptoError;

pub(in crate::crypto) const PAGE_KEY_INFO: &[u8] = b"synap.page-key.v1";
pub(in crate::crypto) const SYNC_KEY_INFO: &[u8] = b"synap.sync-id.v1";
pub(in crate::crypto) const AAD_CONTENT: &[u8] = b"synap.content-key.v1";
pub(in crate::crypto) const AAD_DEVICE: &[u8] = b"synap.device-seed.v1";
pub(in crate::crypto) const AAD_CONTENT_PEER: &[u8] = b"synap.content-key.peer.v1";

/// 主密钥。包裹内容密钥和设备种子，并派生页密钥。
///
/// 用户不加密时，这把密钥明文写在头部。用户要保护时，由调用方另行保管。
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct MasterKey([u8; 32]);

impl MasterKey {
    /// 生成一把新的主密钥。
    pub fn generate() -> Self {
        Self(random_bytes())
    }

    /// 从调用方保管的 32 字节还原。全零拒绝。
    pub fn try_from_bytes(bytes: [u8; 32]) -> Result<Self, CryptoError> {
        if bytes == [0; 32] {
            return Err(CryptoError::WeakKey);
        }
        Ok(Self(bytes))
    }

    /// 交给系统密钥库或以后的口令包裹层保存。
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for MasterKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MasterKey([redacted])")
    }
}

/// 页加密密钥。由主密钥经 HKDF 派生，不单独落盘。
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub(in crate::crypto) struct PageKey([u8; 32]);

impl PageKey {
    pub(in crate::crypto) fn derive(master: &MasterKey) -> Self {
        let hk = Hkdf::<Sha256>::new(None, master.as_bytes());
        let mut okm = [0u8; 32];
        hk.expand(PAGE_KEY_INFO, &mut okm)
            .expect("32-byte HKDF output is within the limit");
        Self(okm)
    }

    pub(in crate::crypto) fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for PageKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PageKey([redacted])")
    }
}

/// 内容加密密钥。加密帧，也是以后交给另一台设备的那把。
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub(in crate::crypto) struct ContentKey([u8; 32]);

impl ContentKey {
    pub(in crate::crypto) fn generate() -> Self {
        Self(random_bytes())
    }

    pub(in crate::crypto) fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// 派生同步身份用的密钥。同一把内容密钥在每台设备上算出同一个 `SyncId`。
    pub(in crate::crypto) fn sync_key(&self) -> [u8; 32] {
        let hk = Hkdf::<Sha256>::new(None, self.as_bytes());
        let mut okm = [0u8; 32];
        hk.expand(SYNC_KEY_INFO, &mut okm)
            .expect("32-byte HKDF output is within the limit");
        okm
    }

    pub(in crate::crypto) fn seal_with(&self, key: &[u8; 32], aad: &[u8]) -> WrappedSecret {
        seal_secret(key, aad, &self.0)
    }

    pub(in crate::crypto) fn open_with(
        key: &[u8; 32],
        aad: &[u8],
        wrapped: &WrappedSecret,
    ) -> Result<Self, CryptoError> {
        Ok(Self(open_secret(key, aad, wrapped)?))
    }
}

impl fmt::Debug for ContentKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ContentKey([redacted])")
    }
}

/// 从 ECDH 共享秘密派生的包裹密钥，只用于把内容密钥交给对端。
#[derive(Zeroize, ZeroizeOnDrop)]
pub(in crate::crypto) struct WrapKey([u8; 32]);

impl WrapKey {
    pub(in crate::crypto) fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub(in crate::crypto) fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

/// 被某把密钥包起来的 32 字节秘密。nonce 和密文可以落盘、也可以交给对端。
#[derive(Clone, Serialize, Deserialize)]
pub struct WrappedSecret {
    nonce: [u8; 12],
    ciphertext: Vec<u8>,
}

impl WrappedSecret {
    /// 编码成可以送去另一台设备的字节。
    pub fn to_bytes(&self) -> Result<Vec<u8>, CryptoError> {
        postcard::to_allocvec(self).map_err(|_| CryptoError::Corrupt)
    }

    /// 从 [`WrappedSecret::to_bytes`] 的产物还原。
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, CryptoError> {
        postcard::from_bytes(bytes).map_err(|_| CryptoError::Corrupt)
    }
}

pub(in crate::crypto) fn seal_secret(
    key: &[u8; 32],
    aad: &[u8],
    plain: &[u8; 32],
) -> WrappedSecret {
    let sealed = aead::seal(key, aad, plain).expect("sealing a 32-byte key");
    WrappedSecret {
        nonce: sealed.nonce,
        ciphertext: sealed.ciphertext,
    }
}

pub(in crate::crypto) fn open_secret(
    key: &[u8; 32],
    aad: &[u8],
    wrapped: &WrappedSecret,
) -> Result<[u8; 32], CryptoError> {
    if wrapped.ciphertext.len() != 32 + 16 {
        return Err(CryptoError::Unwrap);
    }
    let plain =
        aead::open(key, aad, &wrapped.nonce, &wrapped.ciphertext).map_err(|error| match error {
            CryptoError::Decrypt => CryptoError::Unwrap,
            other => other,
        })?;
    plain.try_into().map_err(|_| CryptoError::Unwrap)
}

pub(in crate::crypto) fn keys_equal(left: &[u8; 32], right: &[u8; 32]) -> bool {
    let mut diff = 0u8;
    for (a, b) in left.iter().zip(right) {
        diff |= a ^ b;
    }
    diff == 0
}

fn random_bytes() -> [u8; 32] {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    bytes
}
