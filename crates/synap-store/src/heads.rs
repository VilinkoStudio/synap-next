//! `entity_heads` 的值。小端 `u32` 个数，随后每个头 32 字节，严格递增。

use synap_core::BlockHash;

use crate::error::StoreError;

pub(crate) fn encode_heads(heads: &[BlockHash]) -> Result<Vec<u8>, StoreError> {
    let count = u32::try_from(heads.len()).map_err(|_| StoreError::CorruptValue)?;
    let mut out = Vec::with_capacity(4 + heads.len() * 32);
    out.extend_from_slice(&count.to_le_bytes());
    for head in heads {
        out.extend_from_slice(head.as_bytes());
    }
    Ok(out)
}

pub(crate) fn decode_heads(bytes: &[u8]) -> Result<Vec<BlockHash>, StoreError> {
    let Some((len_bytes, rest)) = bytes.split_first_chunk::<4>() else {
        return Err(StoreError::CorruptValue);
    };
    let count =
        usize::try_from(u32::from_le_bytes(*len_bytes)).map_err(|_| StoreError::CorruptValue)?;
    let width = count.checked_mul(32).ok_or(StoreError::CorruptValue)?;
    if rest.len() != width {
        return Err(StoreError::CorruptValue);
    }
    let mut heads = Vec::with_capacity(count);
    for chunk in rest.chunks_exact(32) {
        let bytes: [u8; 32] = chunk.try_into().expect("chunks_exact yields 32 bytes");
        let hash = BlockHash::from_bytes(bytes);
        if heads.last().is_some_and(|prev| *prev >= hash) {
            return Err(StoreError::CorruptValue);
        }
        heads.push(hash);
    }
    Ok(heads)
}
