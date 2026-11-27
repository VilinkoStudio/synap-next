//! 摄入。本地写入和远端到达走这一条：块、序号、头列表同一次提交。
//!
//! 哈希已经在库里时不推进序号，头列表保持现状。多个头是正常状态，这里不合并。

use redb::{ReadableDatabase, ReadableTable};
use synap_core::{Block, BlockHash, EntityId, LocalSeq};

use crate::crypto::{FrameCodec, FrameKind};
use crate::error::StoreError;
use crate::heads::{decode_heads, encode_heads};
use crate::tables::{BLOCKS, ENTITY_HEADS, LOCAL_SEQ};
use crate::volume::Volume;

/// 一次摄入的结果。
#[derive(Debug)]
pub struct Ingested {
    /// 这块的内容哈希。
    pub hash: BlockHash,
    /// 新分配的本机序号。这块已经在库里时是 `None`。
    pub seq: Option<LocalSeq>,
    /// 该实体此刻的头列表。已经入库的旧块不一定还在里面。
    pub heads: Vec<BlockHash>,
}

/// 一次 MVCC 读事务。块、头列表和序号扫描看到的是同一份快照。
pub struct Snapshot {
    pub(crate) txn: redb::ReadTransaction,
    frames: FrameCodec,
}

/// `local_seq` 上从某个序号之后开始的到达记录。
pub struct Arrival {
    inner: Option<redb::OwnedRange<u64, [u8; 32]>>,
}

impl Volume {
    /// 把一块写入库。父块必须已经在库里，并且属于同一实体、同一 `schema_id`。
    ///
    /// 块提交之后会追平已登记的投影。追平失败时块仍在，水位停在原地。
    pub fn ingest(&self, block: &Block) -> Result<Ingested, StoreError> {
        if self.coord.is_catching_up() {
            return Err(StoreError::Reentrant);
        }
        let ingested = self.write_block(block)?;
        self.catch_up()?;
        Ok(ingested)
    }

    fn write_block(&self, block: &Block) -> Result<Ingested, StoreError> {
        let canonical = block.canonical_bytes()?;
        let hash = block.hash()?;
        let parents = block.sorted_parents();
        let entity = block.header.entity_id;
        let txn = self.database().begin_write()?;
        if exists(&txn, &hash)? {
            let heads = {
                let table = txn.open_table(ENTITY_HEADS)?;
                let owned = table
                    .get(entity.as_bytes())?
                    .map(|guard| guard.value().to_vec());
                heads_from(owned.as_deref())?
            };
            return Ok(Ingested {
                hash,
                seq: None,
                heads,
            });
        }
        verify_parents(self, &txn, block, &parents)?;
        let (seq, heads) = append(self, &txn, &hash, &canonical, entity, &parents)?;
        txn.commit()?;
        Ok(Ingested {
            hash,
            seq: Some(seq),
            heads,
        })
    }

    /// 打开一份只读快照。
    pub fn snapshot(&self) -> Result<Snapshot, StoreError> {
        Ok(Snapshot {
            txn: self.database().begin_read()?,
            frames: self.frame_codec(),
        })
    }
}

impl Snapshot {
    /// 按内容哈希取块。没有则是 `None`。
    pub fn block(&self, hash: &BlockHash) -> Result<Option<Block>, StoreError> {
        let table = self.txn.open_table(BLOCKS)?;
        let Some(guard) = table.get(hash.as_bytes())? else {
            return Ok(None);
        };
        let frame = guard.value().to_vec();
        drop(guard);
        let block = open_block(&self.frames, &frame)?;
        if block.hash()? != *hash {
            return Err(StoreError::BadFrame);
        }
        Ok(Some(block))
    }

    /// 实体当前的头。没有这个实体时是空列表。
    pub fn heads(&self, entity: &EntityId) -> Result<Vec<BlockHash>, StoreError> {
        let table = self.txn.open_table(ENTITY_HEADS)?;
        let owned = table
            .get(entity.as_bytes())?
            .map(|guard| guard.value().to_vec());
        heads_from(owned.as_deref())
    }

    /// 到达日志目前的最大序号。库里还没有块时是 [`LocalSeq::ZERO`]。
    pub fn tip(&self) -> Result<LocalSeq, StoreError> {
        let table = self.txn.open_table(LOCAL_SEQ)?;
        let tip = match table.last()? {
            None => LocalSeq::ZERO,
            Some((key, _)) => LocalSeq::new(key.value()),
        };
        Ok(tip)
    }

    /// 序号严格大于 `after` 的到达记录，按序号升序。`LocalSeq::ZERO` 表示从头开始。
    pub fn since(&self, after: LocalSeq) -> Result<Arrival, StoreError> {
        let Some(start) = after.checked_next() else {
            return Ok(Arrival { inner: None });
        };
        let table = self.txn.open_table(LOCAL_SEQ)?;
        let inner = table.range_owned(start.get()..)?;
        Ok(Arrival { inner: Some(inner) })
    }
}

impl Iterator for Arrival {
    type Item = Result<(LocalSeq, BlockHash), StoreError>;

    fn next(&mut self) -> Option<Self::Item> {
        let (key, value) = match self.inner.as_mut()?.next()? {
            Ok(pair) => pair,
            Err(err) => return Some(Err(err.into())),
        };
        let bytes: [u8; 32] = value.value();
        Some(Ok((
            LocalSeq::new(key.value()),
            BlockHash::from_bytes(bytes),
        )))
    }
}

fn exists(txn: &redb::WriteTransaction, hash: &BlockHash) -> Result<bool, StoreError> {
    let blocks = txn.open_table(BLOCKS)?;
    let found = blocks.get(hash.as_bytes())?.is_some();
    Ok(found)
}

fn verify_parents(
    volume: &Volume,
    txn: &redb::WriteTransaction,
    block: &Block,
    parents: &[BlockHash],
) -> Result<(), StoreError> {
    let frames = {
        let blocks = txn.open_table(BLOCKS)?;
        let mut frames = Vec::with_capacity(parents.len());
        for parent in parents {
            let Some(guard) = blocks.get(parent.as_bytes())? else {
                return Err(StoreError::MissingParent);
            };
            frames.push(guard.value().to_vec());
        }
        frames
    };
    let frames_codec = volume.frame_codec();
    for frame in frames {
        let parent = open_block(&frames_codec, &frame)?;
        if parent.header.entity_id != block.header.entity_id {
            return Err(StoreError::ParentEntity);
        }
        if parent.header.schema_id != block.header.schema_id {
            return Err(StoreError::SchemaMismatch);
        }
    }
    Ok(())
}

fn append(
    volume: &Volume,
    txn: &redb::WriteTransaction,
    hash: &BlockHash,
    canonical: &[u8],
    entity: EntityId,
    parents: &[BlockHash],
) -> Result<(LocalSeq, Vec<BlockHash>), StoreError> {
    let frame = volume.frame_codec().seal(FrameKind::Block, canonical)?;
    {
        let mut blocks = txn.open_table(BLOCKS)?;
        blocks.insert(hash.as_bytes(), frame.as_slice())?;
    }
    let seq = {
        let mut seqs = txn.open_table(LOCAL_SEQ)?;
        let seq = match seqs.last()? {
            None => LocalSeq::new(1),
            Some((key, _)) => LocalSeq::new(key.value())
                .checked_next()
                .ok_or(StoreError::SeqExhausted)?,
        };
        seqs.insert(seq.get(), hash.as_bytes())?;
        seq
    };
    let mut heads = {
        let table = txn.open_table(ENTITY_HEADS)?;
        let owned = table
            .get(entity.as_bytes())?
            .map(|guard| guard.value().to_vec());
        heads_from(owned.as_deref())?
    };
    heads.retain(|head| !parents.contains(head));
    heads.push(*hash);
    heads.sort_unstable();
    heads.dedup();
    {
        let mut table = txn.open_table(ENTITY_HEADS)?;
        table.insert(entity.as_bytes(), encode_heads(&heads)?.as_slice())?;
    }
    let sync_key = volume.sync_key();
    crate::sync_index::record(txn, &sync_key, hash)?;
    Ok((seq, heads))
}

fn heads_from(bytes: Option<&[u8]>) -> Result<Vec<BlockHash>, StoreError> {
    match bytes {
        None => Ok(Vec::new()),
        Some(bytes) => decode_heads(bytes),
    }
}

pub(crate) fn open_block(frames: &FrameCodec, frame: &[u8]) -> Result<Block, StoreError> {
    let opened = frames.open(frame)?;
    if opened.kind != FrameKind::Block {
        return Err(StoreError::BadFrame);
    }
    Block::from_canonical_bytes(&opened.plaintext).map_err(|_| StoreError::BadFrame)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use synap_core::{Block, BlockHash, BlockHeader, EntityId, Hlc, LocalSeq, SchemaId};

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
                "synap-ingest-{}-{}",
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

    fn block(
        entity: EntityId,
        parents: Vec<BlockHash>,
        schema: &str,
        payload: &[u8],
        tombstone: bool,
    ) -> Block {
        Block {
            header: BlockHeader {
                entity_id: entity,
                parents,
                schema_id: SchemaId::parse(schema).unwrap(),
                schema_version: BlockHash::ZERO,
                hlc: Hlc {
                    physical_ms: 1,
                    logical: 0,
                    node_id: 1,
                },
                is_tombstone: tombstone,
            },
            payload: payload.to_vec(),
        }
    }

    fn arrived(volume: &Volume, after: LocalSeq) -> Vec<(LocalSeq, BlockHash)> {
        volume
            .snapshot()
            .unwrap()
            .since(after)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    }

    #[test]
    fn ingest_roundtrips_and_a_repeat_does_not_advance_the_sequence() {
        let file = TempFile::new();
        let created = Volume::create(&file.path, MasterCustody::Embedded).unwrap();
        let entity = EntityId::new();
        let root = block(entity, Vec::new(), "notes/item", b"v1", false);
        let ingested = created.volume.ingest(&root).unwrap();
        assert_eq!(ingested.seq, Some(LocalSeq::new(1)));
        assert_eq!(ingested.heads, vec![ingested.hash]);
        let again = created.volume.ingest(&root).unwrap();
        assert_eq!(again.seq, None);
        assert_eq!(again.heads, vec![ingested.hash]);

        let snapshot = created.volume.snapshot().unwrap();
        let loaded = snapshot.block(&ingested.hash).unwrap().unwrap();
        assert_eq!(loaded, root);
        assert_eq!(snapshot.heads(&entity).unwrap(), vec![ingested.hash]);
        assert!(snapshot
            .block(&BlockHash::from_bytes([3; 32]))
            .unwrap()
            .is_none());
        drop(snapshot);
        assert_eq!(
            arrived(&created.volume, LocalSeq::ZERO),
            vec![(LocalSeq::new(1), ingested.hash)]
        );
        assert!(arrived(&created.volume, LocalSeq::new(1)).is_empty());
        drop(created);

        let volume = Volume::open(&file.path, None).unwrap();
        let loaded = volume
            .snapshot()
            .unwrap()
            .block(&ingested.hash)
            .unwrap()
            .unwrap();
        assert_eq!(loaded.payload, b"v1");
    }

    #[test]
    fn a_fork_keeps_both_heads_until_one_is_replaced() {
        let file = TempFile::new();
        let created = Volume::create(&file.path, MasterCustody::Embedded).unwrap();
        let entity = EntityId::new();
        let root = created
            .volume
            .ingest(&block(entity, Vec::new(), "notes/item", b"root", false))
            .unwrap();
        let left = created
            .volume
            .ingest(&block(
                entity,
                vec![root.hash],
                "notes/item",
                b"left",
                false,
            ))
            .unwrap();
        let right = created
            .volume
            .ingest(&block(
                entity,
                vec![root.hash],
                "notes/item",
                b"right",
                false,
            ))
            .unwrap();
        let mut expected = vec![left.hash, right.hash];
        expected.sort_unstable();
        assert_eq!(right.heads, expected);
        assert_eq!(left.seq, Some(LocalSeq::new(2)));
        assert_eq!(right.seq, Some(LocalSeq::new(3)));

        let next = created
            .volume
            .ingest(&block(entity, vec![left.hash], "notes/item", b"next", true))
            .unwrap();
        assert!(next.heads.contains(&right.hash));
        assert!(next.heads.contains(&next.hash));
        assert!(!next.heads.contains(&left.hash));
        assert_eq!(next.heads.len(), 2);
        let loaded = created
            .volume
            .snapshot()
            .unwrap()
            .block(&next.hash)
            .unwrap()
            .unwrap();
        assert!(loaded.header.is_tombstone);

        let again = created
            .volume
            .ingest(&block(entity, Vec::new(), "notes/item", b"root", false))
            .unwrap();
        assert_eq!(again.seq, None);
        assert_eq!(again.heads, next.heads);
    }

    #[test]
    fn missing_parent_other_entity_and_other_schema_are_rejected() {
        let file = TempFile::new();
        let created = Volume::create(&file.path, MasterCustody::Embedded).unwrap();
        let entity = EntityId::new();
        let missing = block(
            entity,
            vec![BlockHash::from_bytes([9; 32])],
            "notes/item",
            b"x",
            false,
        );
        assert!(matches!(
            created.volume.ingest(&missing),
            Err(StoreError::MissingParent)
        ));

        let root = created
            .volume
            .ingest(&block(entity, Vec::new(), "notes/item", b"root", false))
            .unwrap();
        let cross = block(EntityId::new(), vec![root.hash], "notes/item", b"x", false);
        assert!(matches!(
            created.volume.ingest(&cross),
            Err(StoreError::ParentEntity)
        ));
        let other_schema = block(entity, vec![root.hash], "notes/other", b"x", false);
        assert!(matches!(
            created.volume.ingest(&other_schema),
            Err(StoreError::SchemaMismatch)
        ));
        assert_eq!(
            created.volume.snapshot().unwrap().heads(&entity).unwrap(),
            vec![root.hash]
        );
        assert_eq!(arrived(&created.volume, LocalSeq::ZERO).len(), 1);
    }
}
