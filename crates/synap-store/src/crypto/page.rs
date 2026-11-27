use super::aead;
use super::keys::PageKey;
use super::CryptoError;

/// 页槽。密钥来自主密钥派生，不存在明文页格式。
pub struct PageCodec {
    page: PageKey,
}

impl PageCodec {
    pub(in crate::crypto) fn new(page: PageKey) -> Self {
        Self { page }
    }

    /// 密封一页。`page_id` 写进认证数据，换页号就会解不开。
    pub fn seal(&self, page_id: u64, plaintext: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let sealed = aead::seal(self.page.as_bytes(), &page_aad(page_id), plaintext)?;
        let mut out = Vec::with_capacity(12 + sealed.ciphertext.len());
        out.extend_from_slice(&sealed.nonce);
        out.extend_from_slice(&sealed.ciphertext);
        Ok(out)
    }

    /// 打开一页。认证数据包含 `page_id`。
    pub fn open(&self, page_id: u64, slot: &[u8]) -> Result<Vec<u8>, CryptoError> {
        if slot.len() < 12 + 16 {
            return Err(CryptoError::Truncated);
        }
        let (nonce, ciphertext) = slot.split_at(12);
        let nonce: [u8; 12] = nonce.try_into().expect("split_at yields 12 bytes");
        aead::open(self.page.as_bytes(), &page_aad(page_id), &nonce, ciphertext)
    }
}

fn page_aad(page_id: u64) -> [u8; 21] {
    let mut aad = [0u8; 21];
    aad[..13].copy_from_slice(b"synap.page.v1");
    aad[13..].copy_from_slice(&page_id.to_le_bytes());
    aad
}
