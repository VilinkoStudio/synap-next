use std::fmt;

use hkdf::Hkdf;
use sha2::Sha256;
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::{Zeroize, ZeroizeOnDrop};

use super::keys::{WrapKey, AAD_CONTENT_PEER};
use super::CryptoError;

/// 设备身份。就是本机 X25519 公钥，交换时直接把它交给对端。
#[derive(Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct DeviceId([u8; 32]);

impl DeviceId {
    /// 公钥字节。
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// 从对端发来的 32 字节公钥构造。是否是低阶点，要等 `agree` 的结果来拒绝。
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl fmt::Debug for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DeviceId(")?;
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        write!(f, ")")
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// 本机 X25519 私钥。种子是已经钳位过的 32 字节。
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub(in crate::crypto) struct DeviceSecret {
    seed: [u8; 32],
}

impl DeviceSecret {
    pub(in crate::crypto) fn generate() -> Self {
        let secret = StaticSecret::random_from_rng(rand::rngs::OsRng);
        Self {
            seed: secret.to_bytes(),
        }
    }

    pub(in crate::crypto) fn from_seed(seed: [u8; 32]) -> Self {
        Self { seed }
    }

    pub(in crate::crypto) fn seed_bytes(&self) -> &[u8; 32] {
        &self.seed
    }

    pub(in crate::crypto) fn public_id(&self) -> DeviceId {
        let secret = StaticSecret::from(self.seed);
        DeviceId(*PublicKey::from(&secret).as_bytes())
    }

    pub(in crate::crypto) fn agree(&self, peer: &DeviceId) -> Result<SharedSecret, CryptoError> {
        let secret = StaticSecret::from(self.seed);
        let shared = secret.diffie_hellman(&PublicKey::from(peer.0));
        if !shared.was_contributory() {
            return Err(CryptoError::WeakSharedSecret);
        }
        Ok(SharedSecret(*shared.as_bytes()))
    }
}

impl fmt::Debug for DeviceSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DeviceSecret([redacted])")
    }
}

/// 两台设备算出的共享秘密。只用来派生包裹内容密钥的 [`WrapKey`]。
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct SharedSecret([u8; 32]);

impl SharedSecret {
    pub(in crate::crypto) fn wrap_key(&self) -> WrapKey {
        let hk = Hkdf::<Sha256>::new(None, &self.0);
        let mut okm = [0u8; 32];
        // 32 字节远小于 HKDF 的上限，展开失败就是实现错误。
        hk.expand(AAD_CONTENT_PEER, &mut okm)
            .expect("32-byte HKDF output is within the limit");
        WrapKey::from_bytes(okm)
    }
}

impl fmt::Debug for SharedSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SharedSecret([redacted])")
    }
}
