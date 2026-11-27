//! 密钥层次和逐层解码。
//!
//! * **主密钥（MK）**：本地根密钥。页密钥由它经 HKDF 派生，不另存。内容密钥和设备种子用它包裹。
//! * **内容密钥（CEK）**：独立随机密钥，加密要交换的帧。换设备时用 X25519 共享秘密包出去。
//! * **设备种子**：本机 X25519 私钥。[`DeviceId`] 是它的公钥。
//!
//! 用户不加密时，MK 明文写在头部。页和帧的密文格式不变，开关保护不用重写数据库。
//! 帧的 Zstd 由 `zrip` 完成。

mod aead;
mod device;
mod error;
mod frame;
mod header;
mod keys;
mod page;

pub use device::DeviceId;
pub use error::CryptoError;
pub use frame::{FrameCodec, FrameKind, OpenedFrame};
pub use header::{Created, CryptoHeader, MasterCustody, Session, CRYPTO_HEADER_VERSION};
pub use keys::{MasterKey, WrappedSecret};
pub use page::PageCodec;
