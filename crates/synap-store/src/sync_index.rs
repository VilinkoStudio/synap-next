//! 同步索引。摘要存在 `meta` 里，入库时只重算被改到的那一个桶。
//!
//! 根不落盘。读摘要时用存着的 256 个桶现算，避免根和桶各写各的。

use redb::{ReadableDatabase, ReadableTable};
use synap_core::{sync_bucket_hash, BlockHash, MerkleDigest, SyncId};

use crate::error::StoreError;
use crate::ingest::open_block;
use crate::tables::{BLOCKS, META};
use crate::volume::Volume;

const BUCKET_TAG: u8 = 0x02;
const INDEX_TAG: u8 = 0x03;
const COUNT_KEY: [u8; 1] = [0x04];

impl Volume {
    /// 本卷的同步摘要。不扫块。
    pub fn sync_digest(&self) -> Result<MerkleDigest, StoreError> {
        let txn = self.database().begin_read()?;
        let meta = txn.open_table(META)?;
        let mut buckets = [[0u8; 32]; 256];
        for prefix in 0..=255u8 {
            let key = bucket_key(prefix);
            let Some(guard) = meta.get(key.as_slice())? else {
                return Err(StoreError::CorruptValue);
            };
            copy_array(guard.value(), &mut buckets[prefix as usize])?;
        }
        let count = read_count(&meta)?;
        Ok(MerkleDigest::from_bucket_hashes(count, buckets))
    }

    /// 这些桶里的同步身份。
    pub fn sync_ids_in_buckets(&self, prefixes: &[u8]) -> Result<Vec<SyncId>, StoreError> {
        let txn = self.database().begin_read()?;
        let meta = txn.open_table(META)?;
        let mut ids = Vec::new();
        for prefix in prefixes {
            ids.extend(ids_in_prefix(&meta, *prefix)?);
        }
        Ok(ids)
    }

    /// 按同步身份取出帧。声称有、库里却没有，是索引坏了。
    pub fn sync_frames(&self, ids: &[SyncId]) -> Result<Vec<(SyncId, Vec<u8>)>, StoreError> {
        let txn = self.database().begin_read()?;
        let mut frames = Vec::with_capacity(ids.len());
        for id in ids {
            let hash = {
                let meta = txn.open_table(META)?;
                let Some(guard) = meta.get(index_key(id).as_slice())? else {
                    return Err(StoreError::CorruptValue);
                };
                let mut bytes = [0u8; 32];
                copy_array(guard.value(), &mut bytes)?;
                BlockHash::from_bytes(bytes)
            };
            let blocks = txn.open_table(BLOCKS)?;
            let Some(guard) = blocks.get(hash.as_bytes())? else {
                return Err(StoreError::CorruptValue);
            };
            frames.push((*id, guard.value().to_vec()));
        }
        Ok(frames)
    }

    /// 核对帧和声称的同步身份，再走普通入库。
    pub fn ingest_frame(
        &self,
        id: &SyncId,
        frame: &[u8],
    ) -> Result<crate::ingest::Ingested, StoreError> {
        let block = verified_block(self, id, frame)?;
        self.ingest(&block)
    }

    /// 按顺序入库。前面的帧已经提交，失败后整批再来一次仍然幂等。
    pub fn ingest_frames(&self, frames: &[(SyncId, Vec<u8>)]) -> Result<usize, StoreError> {
        for (id, frame) in frames {
            self.ingest_frame(id, frame)?;
        }
        Ok(frames.len())
    }
}

pub(crate) fn ensure(volume: &Volume) -> Result<(), StoreError> {
    let present = {
        let txn = volume.database().begin_read()?;
        let meta = txn.open_table(META)?;
        let key = bucket_key(0);
        let found = meta.get(key.as_slice())?.is_some();
        found
    };
    if present {
        return Ok(());
    }
    rebuild(volume)
}

/// 用全部块重算索引。换内容密钥之后也走这里，身份会跟着变。
pub(crate) fn rebuild(volume: &Volume) -> Result<(), StoreError> {
    let txn = volume.database().begin_write()?;
    let pairs = scan_blocks(volume, &txn)?;
    write_index(&txn, &pairs)?;
    txn.commit()?;
    Ok(())
}

pub(crate) fn record(
    txn: &redb::WriteTransaction,
    sync_key: &[u8; 32],
    hash: &BlockHash,
) -> Result<(), StoreError> {
    let id = SyncId::derive(hash, sync_key);
    let mut meta = txn.open_table(META)?;
    if let Some(existing) = lookup_hash(&meta, &id)? {
        if existing == *hash {
            return Ok(());
        }
        return Err(StoreError::CorruptValue);
    }
    let prefix = id.as_bytes()[0];
    let mut ids = ids_in_prefix(&meta, prefix)?;
    ids.push(id);
    let bucket = sync_bucket_hash(prefix, &ids);
    meta.insert(index_key(&id).as_slice(), hash.as_bytes().as_slice())?;
    let key = bucket_key(prefix);
    meta.insert(key.as_slice(), bucket.as_slice())?;
    let count = read_count(&meta)?
        .checked_add(1)
        .ok_or(StoreError::SeqExhausted)?;
    let bytes = count.to_le_bytes();
    meta.insert(COUNT_KEY.as_slice(), bytes.as_slice())?;
    Ok(())
}

fn scan_blocks(
    volume: &Volume,
    txn: &redb::WriteTransaction,
) -> Result<Vec<(SyncId, BlockHash)>, StoreError> {
    let frames = {
        let blocks = txn.open_table(BLOCKS)?;
        let mut frames = Vec::new();
        for row in blocks.range(..)? {
            let (key, value) = row?;
            frames.push((BlockHash::from_bytes(key.value()), value.value().to_vec()));
        }
        frames
    };
    let codec = volume.frame_codec();
    let sync_key = volume.sync_key();
    let mut pairs = Vec::with_capacity(frames.len());
    for (hash, frame) in frames {
        let block = open_block(&codec, &frame)?;
        if block.hash()? != hash {
            return Err(StoreError::BadFrame);
        }
        pairs.push((SyncId::derive(&hash, &sync_key), hash));
    }
    Ok(pairs)
}

fn write_index(
    txn: &redb::WriteTransaction,
    pairs: &[(SyncId, BlockHash)],
) -> Result<(), StoreError> {
    let mut meta = txn.open_table(META)?;
    clear_index(&mut meta)?;
    let mut grouped = vec![Vec::new(); 256];
    for (id, hash) in pairs {
        meta.insert(index_key(id).as_slice(), hash.as_bytes().as_slice())?;
        grouped[id.as_bytes()[0] as usize].push(*id);
    }
    for prefix in 0..=255u8 {
        let hash = sync_bucket_hash(prefix, &grouped[prefix as usize]);
        let key = bucket_key(prefix);
        meta.insert(key.as_slice(), hash.as_slice())?;
    }
    let count = u64::try_from(pairs.len()).map_err(|_| StoreError::CorruptValue)?;
    let bytes = count.to_le_bytes();
    meta.insert(COUNT_KEY.as_slice(), bytes.as_slice())?;
    Ok(())
}

fn clear_index(meta: &mut redb::Table<'_, &'static [u8], &'static [u8]>) -> Result<(), StoreError> {
    let start = index_key(&SyncId::from_bytes([0; 32]));
    let end = index_key(&SyncId::from_bytes([0xff; 32]));
    let keys = {
        let mut keys = Vec::new();
        for row in meta.range(start.as_slice()..=end.as_slice())? {
            let (key, _) = row?;
            keys.push(key.value().to_vec());
        }
        keys
    };
    for key in keys {
        meta.remove(key.as_slice())?;
    }
    Ok(())
}

fn ids_in_prefix(
    meta: &impl ReadableTable<&'static [u8], &'static [u8]>,
    prefix: u8,
) -> Result<Vec<SyncId>, StoreError> {
    let (start, end) = bucket_bounds(prefix);
    let mut ids = Vec::new();
    for row in meta.range(start.as_slice()..=end.as_slice())? {
        let (key, _) = row?;
        let raw = key.value();
        if raw.len() != 33 || raw[0] != INDEX_TAG {
            return Err(StoreError::CorruptValue);
        }
        let mut body = [0u8; 32];
        body.copy_from_slice(&raw[1..]);
        ids.push(SyncId::from_bytes(body));
    }
    Ok(ids)
}

fn lookup_hash(
    meta: &impl ReadableTable<&'static [u8], &'static [u8]>,
    id: &SyncId,
) -> Result<Option<BlockHash>, StoreError> {
    let Some(guard) = meta.get(index_key(id).as_slice())? else {
        return Ok(None);
    };
    let mut bytes = [0u8; 32];
    copy_array(guard.value(), &mut bytes)?;
    Ok(Some(BlockHash::from_bytes(bytes)))
}

fn read_count(meta: &impl ReadableTable<&'static [u8], &'static [u8]>) -> Result<u64, StoreError> {
    let Some(guard) = meta.get(COUNT_KEY.as_slice())? else {
        return Err(StoreError::CorruptValue);
    };
    let raw = guard.value();
    if raw.len() != 8 {
        return Err(StoreError::CorruptValue);
    }
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(raw);
    Ok(u64::from_le_bytes(bytes))
}

fn verified_block(
    volume: &Volume,
    id: &SyncId,
    frame: &[u8],
) -> Result<synap_core::Block, StoreError> {
    let opened = volume.frame_codec().open(frame)?;
    if opened.kind != crate::crypto::FrameKind::Block {
        return Err(StoreError::BadFrame);
    }
    let block = synap_core::Block::from_canonical_bytes(&opened.plaintext)?;
    let hash = block.hash()?;
    if SyncId::derive(&hash, &volume.sync_key()) != *id {
        return Err(StoreError::SyncMismatch);
    }
    Ok(block)
}

fn bucket_key(prefix: u8) -> [u8; 2] {
    [BUCKET_TAG, prefix]
}

fn index_key(id: &SyncId) -> [u8; 33] {
    let mut key = [0u8; 33];
    key[0] = INDEX_TAG;
    key[1..].copy_from_slice(id.as_bytes());
    key
}

fn bucket_bounds(prefix: u8) -> ([u8; 33], [u8; 33]) {
    let mut start = [0u8; 33];
    let mut end = [0xff; 33];
    start[0] = INDEX_TAG;
    end[0] = INDEX_TAG;
    start[1] = prefix;
    end[1] = prefix;
    (start, end)
}

fn copy_array(raw: &[u8], dest: &mut [u8]) -> Result<(), StoreError> {
    if raw.len() != dest.len() {
        return Err(StoreError::CorruptValue);
    }
    dest.copy_from_slice(raw);
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use synap_core::{Block, BlockHeader, EntityId, Hlc, MerkleDigest, SchemaId, SyncId};

    use super::*;
    use crate::crypto::MasterCustody;

    struct TempFile {
        path: PathBuf,
    }

    impl TempFile {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let mut path = std::env::temp_dir();
            path.push(format!(
                "synap-sync-index-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            Self { path }
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    fn block(payload: &[u8]) -> Block {
        Block {
            header: BlockHeader {
                entity_id: EntityId::new(),
                parents: Vec::new(),
                schema_id: SchemaId::parse("notes/item").unwrap(),
                schema_version: synap_core::BlockHash::ZERO,
                hlc: Hlc {
                    physical_ms: 1,
                    logical: 0,
                    node_id: 1,
                },
                is_tombstone: false,
            },
            payload: payload.to_vec(),
        }
    }

    #[test]
    fn digest_tracks_ingest_and_survives_reopen() {
        let file = TempFile::new();
        let created = Volume::create(&file.path, MasterCustody::Embedded).unwrap();
        assert_eq!(
            created.volume.sync_digest().unwrap(),
            MerkleDigest::from_ids([])
        );
        created.volume.ingest(&block(b"one")).unwrap();
        created.volume.ingest(&block(b"two")).unwrap();
        let ids = created
            .volume
            .sync_ids_in_buckets(&(0..=255).collect::<Vec<_>>())
            .unwrap();
        assert_eq!(ids.len(), 2);
        let digest = created.volume.sync_digest().unwrap();
        assert_eq!(digest, MerkleDigest::from_ids(ids.iter().copied()));
        let frames = created.volume.sync_frames(&ids).unwrap();
        let wrong = created
            .volume
            .ingest_frame(&SyncId::from_bytes([9; 32]), &frames[0].1);
        assert!(matches!(wrong, Err(StoreError::SyncMismatch)));
        created
            .volume
            .ingest_frame(&frames[0].0, &frames[0].1)
            .unwrap();
        assert_eq!(created.volume.sync_digest().unwrap().count, 2);
        drop(created);

        let volume = Volume::open(&file.path, None).unwrap();
        assert_eq!(volume.sync_digest().unwrap(), digest);
    }
}
