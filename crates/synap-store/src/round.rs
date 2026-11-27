//! 一轮追平。计算图在 runtime 里跑，这里提供前后两个世界，并把多个命名空间一次提交。
//!
//! 驱动在事务外读快照、算出每个命名空间的行变更。store 核对各自的水位，一起写入，成功之后才通知监听。
//! 上游先落盘会丢掉「上一轮长什么样」，所以这一轮里谁也不单独提交。

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

use redb::ReadableTable;
use synap_core::{
    exclusive_prefix_end, namespace_prefix, Block, BlockHash, EntityId, LocalSeq, ProjNamespace,
};

use crate::error::StoreError;
use crate::ingest::Snapshot;
use crate::projection::{self, KeyWindow, ProjChange};
use crate::tables::{META, PROJECTIONS};
use crate::volume::Volume;

/// 一个实体在水位两侧的头块。`before` 是区间起点，`after` 是区间终点。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityUnit {
    /// 哪个实体。
    pub entity: EntityId,
    /// 起点处的头。这段序号里才出现的实体，这里是空的。
    pub before: Vec<Block>,
    /// 终点处的头。
    pub after: Vec<Block>,
}

/// `(start, end]` 这一段序号上的头集合。没在这段里出现的实体，两侧都是终点处的头。
pub struct World<'a> {
    snap: &'a Snapshot,
    start: LocalSeq,
    end: LocalSeq,
    states: BTreeMap<EntityId, (Vec<BlockHash>, Vec<BlockHash>)>,
    changed: Vec<EntityId>,
    blocks: HashMap<BlockHash, Block>,
}

impl<'a> World<'a> {
    /// 这段序号的起点，不含。
    pub fn start(&self) -> LocalSeq {
        self.start
    }

    /// 这段序号的终点，包含。
    pub fn end(&self) -> LocalSeq {
        self.end
    }

    /// 头集合在这段里变过的实体，按第一次到达的序号排列。
    pub fn changed(&self) -> impl Iterator<Item = Result<EntityUnit, StoreError>> + '_ {
        self.changed.iter().map(|entity| self.unit(*entity))
    }

    /// 起点处的头块。
    pub fn before(&self, entity: EntityId) -> Result<Vec<Block>, StoreError> {
        self.side(entity, Side::Before)
    }

    /// 终点处的头块。
    pub fn after(&self, entity: EntityId) -> Result<Vec<Block>, StoreError> {
        self.side(entity, Side::After)
    }

    fn unit(&self, entity: EntityId) -> Result<EntityUnit, StoreError> {
        Ok(EntityUnit {
            entity,
            before: self.before(entity)?,
            after: self.after(entity)?,
        })
    }

    fn side(&self, entity: EntityId, side: Side) -> Result<Vec<Block>, StoreError> {
        let Some((before, after)) = self.states.get(&entity) else {
            return self.load_current(entity);
        };
        let hashes = match side {
            Side::Before => before,
            Side::After => after,
        };
        hashes
            .iter()
            .map(|hash| {
                self.blocks
                    .get(hash)
                    .cloned()
                    .ok_or(StoreError::CorruptValue)
            })
            .collect()
    }

    fn load_current(&self, entity: EntityId) -> Result<Vec<Block>, StoreError> {
        let hashes = self.snap.heads(&entity)?;
        let mut blocks = Vec::with_capacity(hashes.len());
        for hash in hashes {
            let Some(block) = self.snap.block(&hash)? else {
                return Err(StoreError::CorruptValue);
            };
            blocks.push(block);
        }
        Ok(blocks)
    }
}

enum Side {
    Before,
    After,
}

impl Snapshot {
    /// `(start, end]` 上的前后两个世界。`end` 不能超过 [`Snapshot::tip`]。
    pub fn world(&self, start: LocalSeq, end: LocalSeq) -> Result<World<'_>, StoreError> {
        let tip = self.tip()?;
        if start > end || end > tip {
            return Err(StoreError::SeqRange);
        }
        let mut grouped: BTreeMap<EntityId, Vec<(LocalSeq, Block)>> = BTreeMap::new();
        if start < tip {
            for arrived in self.since(start)? {
                let (seq, hash) = arrived?;
                let Some(block) = self.block(&hash)? else {
                    return Err(StoreError::CorruptValue);
                };
                grouped
                    .entry(block.header.entity_id)
                    .or_default()
                    .push((seq, block));
            }
        }
        let mut states = BTreeMap::new();
        let mut blocks = HashMap::new();
        let mut changed = Vec::new();
        for (entity, entries) in &grouped {
            let mut at_end = self.heads(entity)?;
            undo_between(&mut at_end, entries, end, tip)?;
            let mut at_start = at_end.clone();
            undo_between(&mut at_start, entries, start, end)?;
            for hash in at_start.iter().chain(at_end.iter()) {
                if blocks.contains_key(hash) {
                    continue;
                }
                let Some(block) = self.block(hash)? else {
                    return Err(StoreError::CorruptValue);
                };
                blocks.insert(*hash, block);
            }
            for (_, block) in entries {
                blocks.insert(block.hash()?, block.clone());
            }
            if let Some(first) = entries
                .iter()
                .find(|(seq, _)| *seq > start && *seq <= end)
                .map(|(seq, _)| *seq)
            {
                changed.push((first, *entity));
            }
            states.insert(*entity, (at_start, at_end));
        }
        changed.sort_by_key(|(seq, _)| *seq);
        Ok(World {
            snap: self,
            start,
            end,
            states,
            changed: changed.into_iter().map(|(_, entity)| entity).collect(),
            blocks,
        })
    }
}

fn undo_between(
    heads: &mut Vec<BlockHash>,
    entries: &[(LocalSeq, Block)],
    low: LocalSeq,
    high: LocalSeq,
) -> Result<(), StoreError> {
    for (seq, block) in entries.iter().rev() {
        if *seq > high {
            continue;
        }
        if *seq <= low {
            break;
        }
        undo(heads, block)?;
    }
    Ok(())
}

fn undo(heads: &mut Vec<BlockHash>, block: &Block) -> Result<(), StoreError> {
    let hash = block.hash()?;
    let Some(position) = heads.iter().position(|head| *head == hash) else {
        return Err(StoreError::CorruptValue);
    };
    heads.remove(position);
    for parent in block.sorted_parents() {
        if !heads.contains(&parent) {
            heads.push(parent);
        }
    }
    heads.sort_unstable();
    heads.dedup();
    Ok(())
}

/// 一个命名空间在这一轮里要写的内容。`from` 必须等于库里的水位。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamespaceWrite {
    /// 写到哪个投影。
    pub namespace: ProjNamespace,
    /// 驱动看到的水位。
    pub from: LocalSeq,
    /// 这一轮追到的序号。
    pub to: LocalSeq,
    /// 行变更。同一键多次出现时后者生效。
    pub ops: Vec<ProjChange>,
}

/// runtime 登记的计算图。store 不关心里面有哪些算子。
pub trait ProjectionDriver: Send + Sync {
    /// 在事务外读快照，返回这一轮要写的全部分命名空间。
    fn plan(&self, snap: &Snapshot) -> Result<Vec<NamespaceWrite>, StoreError>;
}

/// 一次提交后，落在监听区间里的行变更。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchEvent {
    /// 哪个投影。
    pub namespace: ProjNamespace,
    /// 命中的变更，顺序与提交时一致。
    pub changes: Vec<ProjChange>,
}

/// 一段键区间的监听。放下就解除。
pub struct Watch {
    id: u64,
    rx: Receiver<WatchEvent>,
    coord: Arc<LoopState>,
}

impl Watch {
    /// 取一条已经送到的通知。没有则是 `None`。
    pub fn try_recv(&self) -> Option<WatchEvent> {
        self.rx.try_recv().ok()
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        if let Ok(mut watches) = self.coord.watches.lock() {
            watches.remove(&self.id);
        }
    }
}

struct WatchReg {
    namespace: ProjNamespace,
    window: KeyWindow,
    tx: Sender<WatchEvent>,
}

pub(crate) struct LoopState {
    driver: Mutex<Option<Arc<dyn ProjectionDriver>>>,
    watches: Mutex<BTreeMap<u64, WatchReg>>,
    next_watch: AtomicU64,
    catching_up: AtomicBool,
}

impl LoopState {
    pub(crate) fn new() -> Self {
        Self {
            driver: Mutex::new(None),
            watches: Mutex::new(BTreeMap::new()),
            next_watch: AtomicU64::new(1),
            catching_up: AtomicBool::new(false),
        }
    }

    pub(crate) fn is_catching_up(&self) -> bool {
        self.catching_up.load(Ordering::Acquire)
    }

    fn begin(&self) -> Result<CatchGuard<'_>, StoreError> {
        if self.catching_up.swap(true, Ordering::AcqRel) {
            return Err(StoreError::Reentrant);
        }
        Ok(CatchGuard {
            flag: &self.catching_up,
        })
    }
}

struct CatchGuard<'a> {
    flag: &'a AtomicBool,
}

impl Drop for CatchGuard<'_> {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::Release);
    }
}

impl Volume {
    /// 登记计算图。传 `None` 则摄入只写块。登记之后要自己调用 [`Volume::catch_up`] 补上已有的序号。
    pub fn set_driver(&self, driver: Option<Arc<dyn ProjectionDriver>>) -> Result<(), StoreError> {
        *self.coord.driver.lock().map_err(|_| StoreError::Poison)? = driver;
        Ok(())
    }

    /// 跑一遍已登记的计算图，并把结果一次提交。没有驱动时什么都不做。
    pub fn catch_up(&self) -> Result<(), StoreError> {
        let _guard = self.coord.begin()?;
        let driver = self
            .coord
            .driver
            .lock()
            .map_err(|_| StoreError::Poison)?
            .clone();
        let Some(driver) = driver else {
            return Ok(());
        };
        let snap = self.snapshot()?;
        let writes = driver.plan(&snap)?;
        drop(snap);
        self.commit_round(&writes)
    }

    /// 把多个命名空间的行变更和水位放进同一次提交。任一水位对不上，整轮都不写。
    pub fn commit_round(&self, writes: &[NamespaceWrite]) -> Result<(), StoreError> {
        let mut seen = BTreeSet::new();
        for write in writes {
            if !seen.insert(write.namespace.clone()) {
                return Err(StoreError::DuplicateNamespace);
            }
            if write.to < write.from {
                return Err(StoreError::CheckpointRewind);
            }
            if write.to == write.from && !write.ops.is_empty() {
                return Err(StoreError::CheckpointStalled);
            }
        }
        let active: Vec<&NamespaceWrite> = writes
            .iter()
            .filter(|write| write.to > write.from)
            .collect();
        if active.is_empty() {
            return Ok(());
        }
        let txn = self.database().begin_write()?;
        {
            let meta = txn.open_table(META)?;
            for write in &active {
                let bytes = meta
                    .get(projection::checkpoint_key(&write.namespace)?.as_slice())?
                    .map(|guard| guard.value().to_vec());
                let stored = projection::seq_from(bytes.as_deref())?;
                if stored != write.from {
                    return Err(StoreError::CheckpointMismatch {
                        stored,
                        expected: write.from,
                    });
                }
            }
        }
        {
            let mut table = txn.open_table(PROJECTIONS)?;
            for write in &active {
                for op in &write.ops {
                    let encoded = op.key().encode(&write.namespace)?;
                    let replaced = match op {
                        ProjChange::Put { value, .. } => {
                            table.insert(encoded.as_slice(), value.as_slice())?
                        }
                        ProjChange::Delete { .. } => table.remove(encoded.as_slice())?,
                    };
                    drop(replaced);
                }
            }
        }
        {
            let mut meta = txn.open_table(META)?;
            for write in &active {
                let seq = write.to.get().to_le_bytes();
                let replaced = meta.insert(
                    projection::checkpoint_key(&write.namespace)?.as_slice(),
                    seq.as_slice(),
                )?;
                drop(replaced);
            }
        }
        txn.commit()?;
        self.notify(&active)?;
        Ok(())
    }

    /// 监听一个投影的键区间。通知在提交成功之后送到。
    pub fn watch(&self, namespace: ProjNamespace, window: KeyWindow) -> Result<Watch, StoreError> {
        let id = self.coord.next_watch.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        self.coord
            .watches
            .lock()
            .map_err(|_| StoreError::Poison)?
            .insert(
                id,
                WatchReg {
                    namespace,
                    window,
                    tx,
                },
            );
        Ok(Watch {
            id,
            rx,
            coord: Arc::clone(&self.coord),
        })
    }

    /// 删掉一个投影的全部行和水位。
    pub fn drop_namespace(&self, namespace: &ProjNamespace) -> Result<(), StoreError> {
        let prefix = namespace_prefix(namespace)?;
        let end = exclusive_prefix_end(&prefix);
        let txn = self.database().begin_write()?;
        {
            let mut table = txn.open_table(PROJECTIONS)?;
            let keys = {
                let mut found = Vec::new();
                let rows = match &end {
                    Some(end) => table.range(prefix.as_slice()..end.as_slice())?,
                    None => table.range(prefix.as_slice()..)?,
                };
                for row in rows {
                    let (key, _) = row?;
                    found.push(key.value().to_vec());
                }
                found
            };
            for key in keys {
                let removed = table.remove(key.as_slice())?;
                drop(removed);
            }
        }
        {
            let mut meta = txn.open_table(META)?;
            let removed = meta.remove(projection::checkpoint_key(namespace)?.as_slice())?;
            drop(removed);
        }
        txn.commit()?;
        Ok(())
    }

    fn notify(&self, writes: &[&NamespaceWrite]) -> Result<(), StoreError> {
        let watches = self.coord.watches.lock().map_err(|_| StoreError::Poison)?;
        for write in writes {
            for reg in watches.values() {
                if reg.namespace != write.namespace {
                    continue;
                }
                let mut changes = Vec::new();
                for op in &write.ops {
                    if projection::key_in_window(&write.namespace, &reg.window, op.key())? {
                        changes.push(op.clone());
                    }
                }
                if changes.is_empty() {
                    continue;
                }
                let _ = reg.tx.send(WatchEvent {
                    namespace: write.namespace.clone(),
                    changes,
                });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::sync::{Arc, Mutex};
    use std::thread;

    use synap_core::{
        Block, BlockHash, BlockHeader, EntityId, Hlc, IndexKey, LocalSeq, ProjNamespace, SchemaId,
    };

    use super::*;
    use crate::crypto::MasterCustody;
    use crate::projection::{KeyWindow, ProjChange};
    use crate::volume::Volume;

    struct TempFile {
        path: PathBuf,
    }

    impl TempFile {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let mut path = std::env::temp_dir();
            path.push(format!(
                "synap-round-{}-{}",
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

    fn eid(n: u8) -> EntityId {
        let mut bytes = [0u8; 16];
        bytes[6] = 0x70;
        bytes[8] = 0x80;
        bytes[15] = n;
        EntityId::try_from_bytes(bytes).unwrap()
    }

    fn ns(name: &str) -> ProjNamespace {
        ProjNamespace {
            proj_id: name.to_owned(),
            def_hash: [9; 32],
        }
    }

    fn block(entity: EntityId, parents: Vec<BlockHash>, payload: &[u8], tombstone: bool) -> Block {
        Block {
            header: BlockHeader {
                entity_id: entity,
                parents,
                schema_id: SchemaId::parse("notes/item").unwrap(),
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

    fn payloads(blocks: &[Block]) -> Vec<&[u8]> {
        let mut payloads: Vec<&[u8]> = blocks
            .iter()
            .map(|block| block.payload.as_slice())
            .collect();
        payloads.sort_unstable();
        payloads
    }

    #[test]
    fn a_world_rebuilds_heads_on_both_sides_of_a_gap() {
        let file = TempFile::new();
        let created = Volume::create(&file.path, MasterCustody::Embedded).unwrap();
        let entity = eid(1);
        let other = eid(2);
        created
            .volume
            .ingest(&block(other, Vec::new(), b"other", false))
            .unwrap();
        let root = created
            .volume
            .ingest(&block(entity, Vec::new(), b"a", false))
            .unwrap();
        let left = created
            .volume
            .ingest(&block(entity, vec![root.hash], b"b", false))
            .unwrap();
        created
            .volume
            .ingest(&block(entity, vec![root.hash], b"c", false))
            .unwrap();
        created
            .volume
            .ingest(&block(entity, vec![left.hash], b"d", false))
            .unwrap();
        let snap = created.volume.snapshot().unwrap();
        let world = snap.world(LocalSeq::new(2), LocalSeq::new(4)).unwrap();
        let unit = world.changed().next().unwrap().unwrap();
        assert_eq!(unit.entity, entity);
        assert_eq!(payloads(&unit.before), vec![b"a".as_slice()]);
        assert_eq!(
            payloads(&unit.after),
            vec![b"b".as_slice(), b"c".as_slice()]
        );
        assert!(world.changed().nth(1).is_none());
        assert_eq!(
            payloads(&world.before(other).unwrap()),
            vec![b"other".as_slice()]
        );
        assert_eq!(world.after(other).unwrap(), world.before(other).unwrap());
        let full = snap.world(LocalSeq::ZERO, snap.tip().unwrap()).unwrap();
        let entities: Vec<_> = full.changed().map(|unit| unit.unwrap().entity).collect();
        assert_eq!(entities, vec![other, entity]);
        let latest = full.after(entity).unwrap();
        assert_eq!(payloads(&latest), vec![b"c".as_slice(), b"d".as_slice()]);
        assert!(full.before(entity).unwrap().is_empty());
    }

    #[test]
    fn a_failed_namespace_rolls_the_whole_round_back() {
        let file = TempFile::new();
        let created = Volume::create(&file.path, MasterCustody::Embedded).unwrap();
        let namespace = ns("todo/open");
        let other = ns("todo/done");
        let key = IndexKey::new(Vec::new(), eid(1));
        created
            .volume
            .apply_projection(
                &namespace,
                LocalSeq::ZERO,
                LocalSeq::new(1),
                &[ProjChange::Put {
                    key: key.clone(),
                    value: b"v1".to_vec(),
                }],
            )
            .unwrap();
        let err = created
            .volume
            .commit_round(&[
                NamespaceWrite {
                    namespace: namespace.clone(),
                    from: LocalSeq::new(1),
                    to: LocalSeq::new(2),
                    ops: vec![ProjChange::Put {
                        key: key.clone(),
                        value: b"v2".to_vec(),
                    }],
                },
                NamespaceWrite {
                    namespace: other,
                    from: LocalSeq::new(4),
                    to: LocalSeq::new(5),
                    ops: Vec::new(),
                },
            ])
            .unwrap_err();
        assert!(matches!(
            err,
            StoreError::CheckpointMismatch { stored, expected }
                if stored == LocalSeq::ZERO && expected == LocalSeq::new(4)
        ));
        let snap = created.volume.snapshot().unwrap();
        assert_eq!(
            snap.projection_checkpoint(&namespace).unwrap(),
            LocalSeq::new(1)
        );
        assert_eq!(
            snap.projection_get(&namespace, &key).unwrap().unwrap(),
            b"v1"
        );
    }

    struct IndexPayloads {
        namespace: ProjNamespace,
    }

    impl ProjectionDriver for IndexPayloads {
        fn plan(&self, snap: &Snapshot) -> Result<Vec<NamespaceWrite>, StoreError> {
            let tip = snap.tip()?;
            let from = snap.projection_checkpoint(&self.namespace)?;
            if from == tip {
                return Ok(Vec::new());
            }
            let world = snap.world(from, tip)?;
            let mut ops = Vec::new();
            for unit in world.changed() {
                let unit = unit?;
                let key = IndexKey::new(Vec::new(), unit.entity);
                let tombstone = unit.after.iter().any(|block| block.header.is_tombstone);
                if tombstone || unit.after.is_empty() {
                    ops.push(ProjChange::Delete { key });
                    continue;
                }
                let value = unit
                    .after
                    .last()
                    .expect("after is not empty")
                    .payload
                    .clone();
                ops.push(ProjChange::Put { key, value });
            }
            Ok(vec![NamespaceWrite {
                namespace: self.namespace.clone(),
                from,
                to: tip,
                ops,
            }])
        }
    }

    #[test]
    fn ingest_runs_the_driver_and_a_watch_sees_the_rows() {
        let file = TempFile::new();
        let created = Volume::create(&file.path, MasterCustody::Embedded).unwrap();
        let namespace = ns("notes/title");
        created
            .volume
            .set_driver(Some(Arc::new(IndexPayloads {
                namespace: namespace.clone(),
            })))
            .unwrap();
        let watch = created
            .volume
            .watch(namespace.clone(), KeyWindow::All)
            .unwrap();
        let entity = eid(3);
        let ingested = created
            .volume
            .ingest(&block(entity, Vec::new(), b"hello", false))
            .unwrap();
        let snap = created.volume.snapshot().unwrap();
        let key = IndexKey::new(Vec::new(), entity);
        assert_eq!(
            snap.projection_checkpoint(&namespace).unwrap(),
            ingested.seq.unwrap()
        );
        assert_eq!(
            snap.projection_get(&namespace, &key).unwrap().unwrap(),
            b"hello"
        );
        let event = watch.try_recv().unwrap();
        assert_eq!(event.changes.len(), 1);
        created
            .volume
            .ingest(&block(entity, vec![ingested.hash], b"gone", true))
            .unwrap();
        assert!(snap.projection_get(&namespace, &key).unwrap().is_some());
        let snap = created.volume.snapshot().unwrap();
        assert!(snap.projection_get(&namespace, &key).unwrap().is_none());
        created.volume.drop_namespace(&namespace).unwrap();
        let snap = created.volume.snapshot().unwrap();
        assert_eq!(
            snap.projection_checkpoint(&namespace).unwrap(),
            LocalSeq::ZERO
        );
    }

    struct FailOnce {
        namespace: ProjNamespace,
        failed: AtomicBool,
    }

    impl ProjectionDriver for FailOnce {
        fn plan(&self, snap: &Snapshot) -> Result<Vec<NamespaceWrite>, StoreError> {
            if !self.failed.swap(true, Ordering::AcqRel) {
                return Err(StoreError::Reentrant);
            }
            IndexPayloads {
                namespace: self.namespace.clone(),
            }
            .plan(snap)
        }
    }

    #[test]
    fn a_failed_plan_keeps_the_block_and_the_next_ingest_catches_up() {
        let file = TempFile::new();
        let created = Volume::create(&file.path, MasterCustody::Embedded).unwrap();
        let namespace = ns("notes/title");
        created
            .volume
            .set_driver(Some(Arc::new(FailOnce {
                namespace: namespace.clone(),
                failed: AtomicBool::new(false),
            })))
            .unwrap();
        let entity = eid(5);
        let err = created
            .volume
            .ingest(&block(entity, Vec::new(), b"kept", false))
            .unwrap_err();
        assert!(matches!(err, StoreError::Reentrant));
        let snap = created.volume.snapshot().unwrap();
        assert_eq!(snap.tip().unwrap(), LocalSeq::new(1));
        assert_eq!(
            snap.projection_checkpoint(&namespace).unwrap(),
            LocalSeq::ZERO
        );
        drop(snap);
        created
            .volume
            .ingest(&block(eid(6), Vec::new(), b"next", false))
            .unwrap();
        let snap = created.volume.snapshot().unwrap();
        assert_eq!(
            snap.projection_checkpoint(&namespace).unwrap(),
            LocalSeq::new(2)
        );
        assert_eq!(
            snap.projection_get(&namespace, &IndexKey::new(Vec::new(), entity))
                .unwrap()
                .unwrap(),
            b"kept"
        );
    }

    struct Reenter {
        start: Sender<()>,
        result: Mutex<Receiver<Result<(), StoreError>>>,
    }

    impl ProjectionDriver for Reenter {
        fn plan(&self, _snap: &Snapshot) -> Result<Vec<NamespaceWrite>, StoreError> {
            self.start.send(()).unwrap();
            self.result.lock().unwrap().recv().unwrap()?;
            Ok(Vec::new())
        }
    }

    #[test]
    fn the_driver_cannot_ingest_while_a_round_is_open() {
        let file = TempFile::new();
        let created = Volume::create(&file.path, MasterCustody::Embedded).unwrap();
        let (start_tx, start_rx) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();
        created
            .volume
            .set_driver(Some(Arc::new(Reenter {
                start: start_tx,
                result: Mutex::new(result_rx),
            })))
            .unwrap();
        let volume = &created.volume;
        thread::scope(|scope| {
            scope.spawn(move || {
                start_rx.recv().unwrap();
                let err = volume.ingest(&block(eid(7), Vec::new(), b"nope", false));
                result_tx.send(err.map(|_| ())).unwrap();
            });
            let err = volume
                .ingest(&block(eid(8), Vec::new(), b"outer", false))
                .unwrap_err();
            assert!(matches!(err, StoreError::Reentrant));
        });
        let snap = created.volume.snapshot().unwrap();
        assert_eq!(snap.tip().unwrap(), LocalSeq::new(1));
        assert_eq!(snap.heads(&eid(7)).unwrap(), Vec::<BlockHash>::new());
    }
}
