use serde::{Deserialize, Serialize};

use super::device::{DeviceSecret, SharedSecret};
use super::frame::FrameCodec;
use super::keys::{
    keys_equal, ContentKey, MasterKey, PageKey, WrappedSecret, AAD_CONTENT, AAD_CONTENT_PEER,
    AAD_DEVICE,
};
use super::page::PageCodec;
use super::CryptoError;

/// 头部格式版本。字段顺序变了就加一。
pub const CRYPTO_HEADER_VERSION: u8 = 1;

/// 主密钥放在哪。页和帧的密文格式与此无关。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MasterCustody {
    /// 调用方保管主密钥，头部里没有它。
    External,
    /// 主密钥明文写在头部。
    Embedded,
}

/// 写在页加密之外的头部。打开页之前只能读到这里。
pub struct CryptoHeader {
    version: u8,
    custody: MasterCustody,
    embedded_master: Option<MasterKey>,
    wrapped_content: WrappedSecret,
    device_id: super::DeviceId,
    device_secret: WrappedSecret,
}

/// [`CryptoHeader::create`] 的结果。两种存放方式都会把主密钥交还调用方。
pub struct Created {
    /// 可以落盘的头部。
    pub header: CryptoHeader,
    /// 刚生成的主密钥。`Embedded` 时头部里还有一份明文。
    pub master: MasterKey,
}

/// 解开头部之后的会话。页编码器和帧编码器都从这里拿。
pub struct Session {
    page: PageKey,
    content: ContentKey,
    device: DeviceSecret,
}

#[derive(Serialize, Deserialize)]
struct Record {
    version: u8,
    custody: MasterCustody,
    embedded_master: Option<[u8; 32]>,
    wrapped_content: WrappedSecret,
    device_id: super::DeviceId,
    device_secret: WrappedSecret,
}

impl CryptoHeader {
    /// 新建头部。同时生成主密钥、内容密钥和设备密钥对。
    pub fn create(custody: MasterCustody) -> Created {
        let master = MasterKey::generate();
        let device = DeviceSecret::generate();
        let device_id = device.public_id();
        let content = ContentKey::generate();
        let header = Self {
            version: CRYPTO_HEADER_VERSION,
            custody,
            embedded_master: match custody {
                MasterCustody::External => None,
                MasterCustody::Embedded => Some(master.clone()),
            },
            wrapped_content: content.seal_with(master.as_bytes(), AAD_CONTENT),
            device_id,
            device_secret: wrap_device(&master, &device),
        };
        Created { header, master }
    }

    /// 主密钥是调用方保管，还是明文写在头部。
    pub fn custody(&self) -> MasterCustody {
        self.custody
    }

    /// 本机设备号。它总是明文，配对时对端要在开库前认得它。
    pub fn device_id(&self) -> super::DeviceId {
        self.device_id
    }

    /// 编码头部。`External` 的产物里没有主密钥。
    pub fn to_bytes(&self) -> Result<Vec<u8>, CryptoError> {
        let record = Record {
            version: self.version,
            custody: self.custody,
            embedded_master: self.embedded_master.as_ref().map(|key| *key.as_bytes()),
            wrapped_content: self.wrapped_content.clone(),
            device_id: self.device_id,
            device_secret: self.device_secret.clone(),
        };
        postcard::to_allocvec(&record).map_err(|_| CryptoError::Corrupt)
    }

    /// 从 [`CryptoHeader::to_bytes`] 的产物还原。
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, CryptoError> {
        let record: Record = postcard::from_bytes(bytes).map_err(|_| CryptoError::Corrupt)?;
        if record.version != CRYPTO_HEADER_VERSION {
            return Err(CryptoError::UnsupportedVersion);
        }
        let embedded_master = match (record.custody, record.embedded_master) {
            (MasterCustody::External, None) => None,
            (MasterCustody::Embedded, Some(bytes)) => Some(MasterKey::try_from_bytes(bytes)?),
            _ => return Err(CryptoError::Corrupt),
        };
        Ok(Self {
            version: record.version,
            custody: record.custody,
            embedded_master,
            wrapped_content: record.wrapped_content,
            device_id: record.device_id,
            device_secret: record.device_secret,
        })
    }

    /// 用调用方保管的主密钥打开。头部里已经有明文主密钥时，这把密钥必须和它相同。
    pub fn unlock(&self, master: &MasterKey) -> Result<Session, CryptoError> {
        if let Some(stored) = &self.embedded_master {
            if !keys_equal(stored.as_bytes(), master.as_bytes()) {
                return Err(CryptoError::Unwrap);
            }
        }
        self.open_session(master)
    }

    /// 用头部里的明文主密钥打开。主密钥不在头部时失败。
    pub fn unlock_embedded(&self) -> Result<Session, CryptoError> {
        let master = self
            .embedded_master
            .as_ref()
            .ok_or(CryptoError::CustodyMismatch)?;
        self.open_session(master)
    }

    /// 把主密钥明文写入头部。页和帧不用重写。主密钥必须能解开当前的内容密钥。
    pub fn embed_master(&mut self, master: &MasterKey) -> Result<(), CryptoError> {
        if self.embedded_master.is_some() {
            return Err(CryptoError::CustodyMismatch);
        }
        self.open_session(master)?;
        self.custody = MasterCustody::Embedded;
        self.embedded_master = Some(master.clone());
        Ok(())
    }

    /// 从头部取出明文主密钥。页和帧不用重写。
    pub fn extract_master(&mut self) -> Result<MasterKey, CryptoError> {
        let master = self
            .embedded_master
            .take()
            .ok_or(CryptoError::CustodyMismatch)?;
        self.custody = MasterCustody::External;
        Ok(master)
    }

    /// 用和某台设备的共享秘密解开它送来的内容密钥，再改由本机主密钥包裹。
    ///
    /// 成功后要重新打开会话。页密钥仍由这把主密钥派生，不用重加密页。
    pub fn install_content_key(
        &mut self,
        master: &MasterKey,
        shared: &SharedSecret,
        exported: &WrappedSecret,
    ) -> Result<(), CryptoError> {
        if let Some(stored) = &self.embedded_master {
            if !keys_equal(stored.as_bytes(), master.as_bytes()) {
                return Err(CryptoError::Unwrap);
            }
        }
        let _current =
            ContentKey::open_with(master.as_bytes(), AAD_CONTENT, &self.wrapped_content)?;
        let content =
            ContentKey::open_with(shared.wrap_key().as_bytes(), AAD_CONTENT_PEER, exported)?;
        self.wrapped_content = content.seal_with(master.as_bytes(), AAD_CONTENT);
        Ok(())
    }

    fn open_session(&self, master: &MasterKey) -> Result<Session, CryptoError> {
        let content = ContentKey::open_with(master.as_bytes(), AAD_CONTENT, &self.wrapped_content)?;
        let device = open_device(master, &self.device_secret)?;
        if device.public_id() != self.device_id {
            return Err(CryptoError::Corrupt);
        }
        Ok(Session {
            page: PageKey::derive(master),
            content,
            device,
        })
    }
}

impl Session {
    /// 本机设备号。
    pub fn device_id(&self) -> super::DeviceId {
        self.device.public_id()
    }

    /// 和 `peer` 做 X25519。共享秘密没有贡献时拒绝。
    pub fn agree(&self, peer: &super::DeviceId) -> Result<SharedSecret, CryptoError> {
        self.device.agree(peer)
    }

    /// 用共享秘密把内容密钥包给对端。
    pub fn export_content_key(&self, shared: &SharedSecret) -> Result<WrappedSecret, CryptoError> {
        Ok(self
            .content
            .seal_with(shared.wrap_key().as_bytes(), AAD_CONTENT_PEER))
    }

    /// 页槽编码器。
    pub fn page_codec(&self) -> PageCodec {
        PageCodec::new(self.page.clone())
    }

    /// 帧编码器。
    pub fn frame_codec(&self) -> FrameCodec {
        FrameCodec::new(self.content.clone())
    }

    /// 同步身份的密钥。由内容密钥派生，不随本机主密钥变化。
    pub(crate) fn sync_key(&self) -> [u8; 32] {
        self.content.sync_key()
    }

    #[cfg(test)]
    fn device_seed(&self) -> &[u8; 32] {
        self.device.seed_bytes()
    }

    #[cfg(test)]
    fn page_key_bytes(&self) -> &[u8; 32] {
        self.page.as_bytes()
    }

    #[cfg(test)]
    fn content_key_bytes(&self) -> &[u8; 32] {
        self.content.as_bytes()
    }
}

fn wrap_device(master: &MasterKey, device: &DeviceSecret) -> WrappedSecret {
    super::keys::seal_secret(master.as_bytes(), AAD_DEVICE, device.seed_bytes())
}

fn open_device(master: &MasterKey, stored: &WrappedSecret) -> Result<DeviceSecret, CryptoError> {
    let seed = super::keys::open_secret(master.as_bytes(), AAD_DEVICE, stored)?;
    Ok(DeviceSecret::from_seed(seed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{CryptoError, DeviceId, FrameKind, MasterKey};

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        needle.len() <= haystack.len()
            && haystack
                .windows(needle.len())
                .any(|window| window == needle)
    }

    #[test]
    fn embedded_header_stores_only_the_master_key() {
        let Created { header, master } = CryptoHeader::create(MasterCustody::Embedded);
        let bytes = header.to_bytes().unwrap();
        assert!(contains(&bytes, master.as_bytes()));
        let session = header.unlock_embedded().unwrap();
        assert!(!contains(&bytes, session.device_seed()));
        assert!(!contains(&bytes, session.page_key_bytes()));
        assert!(!contains(&bytes, session.content_key_bytes()));
        assert_eq!(
            session
                .frame_codec()
                .seal(FrameKind::Block, b"hello")
                .unwrap()[0],
            0x15
        );
        let slot = session.page_codec().seal(3, b"page-body").unwrap();
        assert_ne!(&slot[..], b"page-body");
        assert_eq!(session.page_codec().open(3, &slot).unwrap(), b"page-body");
    }

    #[test]
    fn external_header_hides_the_master_key() {
        let Created { header, master } = CryptoHeader::create(MasterCustody::External);
        let bytes = header.to_bytes().unwrap();
        assert!(!contains(&bytes, master.as_bytes()));
        let session = header.unlock(&master).unwrap();
        assert!(!contains(&bytes, session.device_seed()));
        assert!(!contains(&bytes, session.page_key_bytes()));
        assert!(!contains(&bytes, session.content_key_bytes()));
        assert!(contains(&bytes, header.device_id().as_bytes()));
        let restored = CryptoHeader::from_bytes(&bytes).unwrap();
        let again = restored.unlock(&master).unwrap();
        let frame = session
            .frame_codec()
            .seal(FrameKind::Blob, &[7u8; 128])
            .unwrap();
        assert_eq!(frame[0], 0x16);
        assert_eq!(
            again.frame_codec().open(&frame).unwrap().plaintext,
            vec![7u8; 128]
        );
        assert!(matches!(
            session
                .page_codec()
                .open(4, &session.page_codec().seal(3, b"x").unwrap()),
            Err(CryptoError::Decrypt)
        ));
    }

    #[test]
    fn moving_the_master_key_does_not_rewrite_ciphertext() {
        let Created { mut header, master } = CryptoHeader::create(MasterCustody::External);
        let session = header.unlock(&master).unwrap();
        let frame = session
            .frame_codec()
            .seal(FrameKind::Block, b"same")
            .unwrap();
        let slot = session.page_codec().seal(1, b"page").unwrap();
        header.embed_master(&master).unwrap();
        assert!(contains(&header.to_bytes().unwrap(), master.as_bytes()));
        let session = header.unlock_embedded().unwrap();
        assert_eq!(
            session.frame_codec().open(&frame).unwrap().plaintext,
            b"same"
        );
        assert_eq!(session.page_codec().open(1, &slot).unwrap(), b"page");
        let taken = header.extract_master().unwrap();
        assert_eq!(taken.as_bytes(), master.as_bytes());
        assert!(!contains(&header.to_bytes().unwrap(), master.as_bytes()));
        let session = header.unlock(&master).unwrap();
        assert_eq!(
            session.frame_codec().open(&frame).unwrap().plaintext,
            b"same"
        );
        assert_eq!(session.page_codec().open(1, &slot).unwrap(), b"page");
    }

    #[test]
    fn wrong_master_and_wrong_custody_fail() {
        let Created { mut header, master } = CryptoHeader::create(MasterCustody::External);
        let mut bad = *master.as_bytes();
        bad[0] ^= 0x5a;
        let bad = MasterKey::try_from_bytes(bad).unwrap();
        assert!(matches!(header.unlock(&bad), Err(CryptoError::Unwrap)));
        assert!(matches!(
            header.unlock_embedded(),
            Err(CryptoError::CustodyMismatch)
        ));
        assert!(matches!(
            header.extract_master(),
            Err(CryptoError::CustodyMismatch)
        ));
        let session = header.unlock(&master).unwrap();
        assert!(matches!(
            session.agree(&DeviceId::from_bytes([0; 32])),
            Err(CryptoError::WeakSharedSecret)
        ));
    }

    #[test]
    fn swapped_wraps_do_not_unlock() {
        let Created { mut header, master } = CryptoHeader::create(MasterCustody::External);
        std::mem::swap(&mut header.wrapped_content, &mut header.device_secret);
        assert!(matches!(header.unlock(&master), Err(CryptoError::Unwrap)));
    }

    #[test]
    fn content_key_moves_across_devices() {
        let Created {
            header: header_a,
            master: master_a,
        } = CryptoHeader::create(MasterCustody::External);
        let Created {
            header: mut header_b,
            master: master_b,
        } = CryptoHeader::create(MasterCustody::Embedded);
        let session_a = header_a.unlock(&master_a).unwrap();
        let session_b = header_b.unlock_embedded().unwrap();
        let shared_a = session_a.agree(&session_b.device_id()).unwrap();
        let shared_b = session_b.agree(&session_a.device_id()).unwrap();
        let exported = session_a.export_content_key(&shared_a).unwrap();
        header_b
            .install_content_key(&master_b, &shared_b, &exported)
            .unwrap();
        let session_b = header_b.unlock_embedded().unwrap();
        let frame = session_a
            .frame_codec()
            .seal(FrameKind::Block, b"synced")
            .unwrap();
        assert_eq!(
            session_b.frame_codec().open(&frame).unwrap().plaintext,
            b"synced"
        );
    }
}
