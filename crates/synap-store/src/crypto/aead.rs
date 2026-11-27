use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use rand::RngCore;

use super::CryptoError;

pub(in crate::crypto) struct SealedBlob {
    pub nonce: [u8; 12],
    pub ciphertext: Vec<u8>,
}

pub(in crate::crypto) fn seal(
    key: &[u8; 32],
    aad: &[u8],
    plain: &[u8],
) -> Result<SealedBlob, CryptoError> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let mut nonce_bytes = [0u8; 12];
    rand::rngs::OsRng.fill_bytes(&mut nonce_bytes);
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), Payload { msg: plain, aad })
        .map_err(|_| CryptoError::Encrypt)?;
    Ok(SealedBlob {
        nonce: nonce_bytes,
        ciphertext,
    })
}

pub(in crate::crypto) fn open(
    key: &[u8; 32],
    aad: &[u8],
    nonce: &[u8; 12],
    ciphertext: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    cipher
        .decrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map_err(|_| CryptoError::Decrypt)
}
