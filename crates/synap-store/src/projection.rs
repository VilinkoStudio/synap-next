//! 投影索引的读写。管道在 runtime 里算，这里保存行和水位。
//!
//! 所有常驻投影共用 `projections` 这一棵有序树。一轮追平里多个命名空间同一次提交，
//! 见 [`crate::round`]。值是覆盖索引，范围扫描按键返回，不回读块。

use std::ops::Bound;

use redb::ReadableTable;
use synap_core::{
    encode_prefix, exclusive_prefix_end, namespace_prefix, CoreError, IndexKey, KeyComponent,
    LocalSeq, ProjNamespace,
};

use crate::error::StoreError;
use crate::tables::{META, PROJECTIONS};
use crate::volume::Volume;

/// 投影水位在 `meta` 里的键标签。别的状态以后用其它标签。
const CHECKPOINT_TAG: u8 = 0x01;

/// 一次投影提交里的一行变更。同一键出现多次时，后者生效。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjChange {
    /// 写入或替换一行。值是展示用的覆盖字节。
    Put {
        /// 排序键加实体。
        key: IndexKey,
        /// 覆盖索引的值。
        value: Vec<u8>,
    },
    /// 删掉一行。键不存在也算成功。
    Delete {
        /// 要删的键。
        key: IndexKey,
    },
}

impl ProjChange {
    pub(crate) fn key(&self) -> &IndexKey {
        match self {
            Self::Put { key, .. } | Self::Delete { key } => key,
        }
    }
}

/// 扫描窗口。游标翻页时窗口保持不变，起点换成上一页的 `resume`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyWindow {
    /// 整个投影。
    All,
    /// 排序键前缀，例如某一天、某一棵子树。不含收尾的实体标识。
    Prefix(Vec<KeyComponent>),
}

/// 这一页从哪个键开始。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyBound {
    /// 包含这个键。
    Inclusive(IndexKey),
    /// 跳过这个键。翻页用上一页的 `resume`。
    Exclusive(IndexKey),
}

impl KeyBound {
    fn key(&self) -> &IndexKey {
        match self {
            Self::Inclusive(key) | Self::Exclusive(key) => key,
        }
    }
}

/// 扫描方向。这是树上的行走方向，和某个分量自己的升序降序无关。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanDir {
    /// 字节序从小到大。
    Forward,
    /// 字节序从大到小。
    Backward,
}

/// 一行投影。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjRow {
    /// 逻辑键。
    pub key: IndexKey,
    /// 覆盖索引的值。
    pub value: Vec<u8>,
}

/// 一页结果。`resume` 有值时，用同样的窗口和方向、`KeyBound::Exclusive(resume)` 取下一页。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjPage {
    /// 这一页的行，顺序就是扫描方向。
    pub rows: Vec<ProjRow>,
    /// 下一页的起点。没有更多行时是 `None`。
    pub resume: Option<IndexKey>,
}

impl Volume {
    /// 把一个命名空间的行变更和水位一次提交。`to == from` 只在变更为空时成立。
    pub fn apply_projection(
        &self,
        namespace: &ProjNamespace,
        from: LocalSeq,
        to: LocalSeq,
        ops: &[ProjChange],
    ) -> Result<(), StoreError> {
        self.commit_round(&[crate::round::NamespaceWrite {
            namespace: namespace.clone(),
            from,
            to,
            ops: ops.to_vec(),
        }])
    }
}

impl super::ingest::Snapshot {
    /// 投影追赶到的本机序号。还没提交过是 [`LocalSeq::ZERO`]。
    pub fn projection_checkpoint(&self, namespace: &ProjNamespace) -> Result<LocalSeq, StoreError> {
        let meta = self.txn.open_table(META)?;
        let bytes = meta
            .get(checkpoint_key(namespace)?.as_slice())?
            .map(|guard| guard.value().to_vec());
        seq_from(bytes.as_deref())
    }

    /// 取一行。没有则是 `None`。
    pub fn projection_get(
        &self,
        namespace: &ProjNamespace,
        key: &IndexKey,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        let table = self.txn.open_table(PROJECTIONS)?;
        let encoded = key.encode(namespace)?;
        let Some(guard) = table.get(encoded.as_slice())? else {
            return Ok(None);
        };
        Ok(Some(guard.value().to_vec()))
    }

    /// 按窗口扫描一页。`limit` 为 0 时返回空页。
    pub fn projection_range(
        &self,
        namespace: &ProjNamespace,
        window: &KeyWindow,
        direction: ScanDir,
        from: Option<&KeyBound>,
        limit: usize,
    ) -> Result<ProjPage, StoreError> {
        if limit == 0 {
            return Ok(ProjPage {
                rows: Vec::new(),
                resume: None,
            });
        }
        let Some((lower, upper)) = clip(namespace, window, direction, from)? else {
            return Ok(ProjPage {
                rows: Vec::new(),
                resume: None,
            });
        };
        let table = self.txn.open_table(PROJECTIONS)?;
        let mut range = table.range_owned((borrowed(&lower), borrowed(&upper)))?;
        let mut raw = Vec::new();
        let fetch = limit.saturating_add(1);
        while raw.len() < fetch {
            let next = match direction {
                ScanDir::Forward => range.next(),
                ScanDir::Backward => range.next_back(),
            };
            match next {
                None => break,
                Some(Err(err)) => return Err(err.into()),
                Some(Ok((key, value))) => {
                    raw.push((key.value().to_vec(), value.value().to_vec()));
                }
            }
        }
        drop(range);
        let more = raw.len() > limit;
        if more {
            raw.pop();
        }
        let mut rows = Vec::with_capacity(raw.len());
        for (key_bytes, value) in raw {
            rows.push(ProjRow {
                key: decode_stored(&key_bytes, namespace)?,
                value,
            });
        }
        let resume = if more {
            rows.last().map(|row| row.key.clone())
        } else {
            None
        };
        Ok(ProjPage { rows, resume })
    }
}

pub(crate) fn checkpoint_key(namespace: &ProjNamespace) -> Result<Vec<u8>, StoreError> {
    let mut key = Vec::with_capacity(1 + namespace.proj_id.len() + 32);
    key.push(CHECKPOINT_TAG);
    key.extend(namespace_prefix(namespace)?);
    Ok(key)
}

pub(crate) fn seq_from(bytes: Option<&[u8]>) -> Result<LocalSeq, StoreError> {
    match bytes {
        None => Ok(LocalSeq::ZERO),
        Some(bytes) if bytes.len() == 8 => {
            let mut buf = [0u8; 8];
            buf.copy_from_slice(bytes);
            Ok(LocalSeq::new(u64::from_le_bytes(buf)))
        }
        Some(_) => Err(StoreError::CorruptValue),
    }
}

fn decode_stored(bytes: &[u8], namespace: &ProjNamespace) -> Result<IndexKey, StoreError> {
    let (found, key) = IndexKey::decode(bytes).map_err(|err| match err {
        CoreError::InvalidKey => StoreError::CorruptValue,
        other => StoreError::Core(other),
    })?;
    if found != *namespace {
        return Err(StoreError::CorruptValue);
    }
    Ok(key)
}

pub(crate) fn key_in_window(
    namespace: &ProjNamespace,
    window: &KeyWindow,
    key: &IndexKey,
) -> Result<bool, StoreError> {
    let (lower, upper) = window_edges(namespace, window)?;
    let encoded = key.encode(namespace)?;
    if encoded.as_slice() < lower.as_slice() {
        return Ok(false);
    }
    if let Some(upper) = upper {
        if encoded.as_slice() >= upper.as_slice() {
            return Ok(false);
        }
    }
    Ok(true)
}

fn window_edges(
    namespace: &ProjNamespace,
    window: &KeyWindow,
) -> Result<(Vec<u8>, Option<Vec<u8>>), CoreError> {
    let prefix = match window {
        KeyWindow::All => namespace_prefix(namespace)?,
        KeyWindow::Prefix(parts) => encode_prefix(namespace, parts)?,
    };
    let namespace_end = exclusive_prefix_end(&namespace_prefix(namespace)?);
    let prefix_end = exclusive_prefix_end(&prefix);
    Ok((prefix, tighter(prefix_end, namespace_end)))
}

fn tighter(left: Option<Vec<u8>>, right: Option<Vec<u8>>) -> Option<Vec<u8>> {
    match (left, right) {
        (None, None) => None,
        (Some(bound), None) | (None, Some(bound)) => Some(bound),
        (Some(left), Some(right)) => Some(if left <= right { left } else { right }),
    }
}

type KeyBounds = (Bound<Vec<u8>>, Bound<Vec<u8>>);

fn clip(
    namespace: &ProjNamespace,
    window: &KeyWindow,
    direction: ScanDir,
    from: Option<&KeyBound>,
) -> Result<Option<KeyBounds>, StoreError> {
    let (lo, hi) = window_edges(namespace, window)?;
    let mut lower = Bound::Included(lo.clone());
    let mut upper = match hi.clone() {
        Some(bytes) => Bound::Excluded(bytes),
        None => Bound::Unbounded,
    };
    let Some(from) = from else {
        return Ok(Some((lower, upper)));
    };
    let key = from.key().encode(namespace)?;
    let before = key.as_slice() < lo.as_slice();
    let past = hi
        .as_ref()
        .is_some_and(|end| key.as_slice() >= end.as_slice());
    match (direction, from, before, past) {
        (ScanDir::Forward, KeyBound::Inclusive(_), true, _) => {}
        (ScanDir::Forward, KeyBound::Inclusive(_), false, true) => return Ok(None),
        (ScanDir::Forward, KeyBound::Inclusive(_), false, false) => {
            lower = Bound::Included(key);
        }
        (ScanDir::Forward, KeyBound::Exclusive(_), true, _) => {}
        (ScanDir::Forward, KeyBound::Exclusive(_), false, true) => return Ok(None),
        (ScanDir::Forward, KeyBound::Exclusive(_), false, false) => {
            lower = Bound::Excluded(key);
        }
        (ScanDir::Backward, KeyBound::Inclusive(_), true, _) => return Ok(None),
        (ScanDir::Backward, KeyBound::Inclusive(_), false, true) => {}
        (ScanDir::Backward, KeyBound::Inclusive(_), false, false) => {
            upper = Bound::Included(key);
        }
        (ScanDir::Backward, KeyBound::Exclusive(_), true, _) => return Ok(None),
        (ScanDir::Backward, KeyBound::Exclusive(_), false, true) => {}
        (ScanDir::Backward, KeyBound::Exclusive(_), false, false) => {
            upper = Bound::Excluded(key);
        }
    }
    Ok(Some((lower, upper)))
}

fn borrowed(bound: &Bound<Vec<u8>>) -> Bound<&[u8]> {
    match bound {
        Bound::Unbounded => Bound::Unbounded,
        Bound::Included(bytes) => Bound::Included(bytes.as_slice()),
        Bound::Excluded(bytes) => Bound::Excluded(bytes.as_slice()),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use synap_core::{
        Block, BlockHash, BlockHeader, EntityId, Hlc, IndexKey, KeyComponent, KeyScalar, LocalSeq,
        ProjNamespace, SchemaId,
    };

    use super::*;
    use crate::crypto::MasterCustody;
    use crate::volume::Volume;

    struct TempFile {
        path: PathBuf,
    }

    impl TempFile {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let mut path = std::env::temp_dir();
            path.push(format!(
                "synap-proj-{}-{}",
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
            def_hash: [7; 32],
        }
    }

    fn text_key(title: &str, entity: EntityId) -> IndexKey {
        IndexKey::new(
            vec![KeyComponent::asc(KeyScalar::Text(title.into()))],
            entity,
        )
    }

    fn put(key: IndexKey, value: &[u8]) -> ProjChange {
        ProjChange::Put {
            key,
            value: value.to_vec(),
        }
    }

    #[test]
    fn rows_follow_key_order_and_the_checkpoint_pages_with_them() {
        let file = TempFile::new();
        let created = Volume::create(&file.path, MasterCustody::Embedded).unwrap();
        let namespace = ns("todo/open");
        let first = text_key("a", eid(1));
        let second = text_key("b", eid(1));
        let third = text_key("c", eid(2));
        created
            .volume
            .apply_projection(
                &namespace,
                LocalSeq::ZERO,
                LocalSeq::new(3),
                &[
                    put(second.clone(), b"B"),
                    put(first.clone(), b"A"),
                    put(third.clone(), b"C"),
                ],
            )
            .unwrap();
        let again = put(first.clone(), b"A2");
        created
            .volume
            .apply_projection(
                &namespace,
                LocalSeq::new(3),
                LocalSeq::new(4),
                &[
                    again,
                    ProjChange::Delete {
                        key: second.clone(),
                    },
                ],
            )
            .unwrap();

        let snapshot = created.volume.snapshot().unwrap();
        assert_eq!(
            snapshot.projection_checkpoint(&namespace).unwrap(),
            LocalSeq::new(4)
        );
        assert_eq!(
            snapshot
                .projection_get(&namespace, &first)
                .unwrap()
                .unwrap(),
            b"A2"
        );
        assert!(snapshot
            .projection_get(&namespace, &second)
            .unwrap()
            .is_none());
        let page = snapshot
            .projection_range(&namespace, &KeyWindow::All, ScanDir::Forward, None, 1)
            .unwrap();
        assert_eq!(page.rows.len(), 1);
        assert_eq!(page.rows[0].key, first);
        let next = snapshot
            .projection_range(
                &namespace,
                &KeyWindow::All,
                ScanDir::Forward,
                Some(&KeyBound::Exclusive(page.resume.unwrap())),
                10,
            )
            .unwrap();
        assert_eq!(next.rows.len(), 1);
        assert_eq!(next.rows[0].key, third);
        assert!(next.resume.is_none());
        let backward = snapshot
            .projection_range(&namespace, &KeyWindow::All, ScanDir::Backward, None, 10)
            .unwrap();
        assert_eq!(
            backward
                .rows
                .iter()
                .map(|row| row.key.clone())
                .collect::<Vec<_>>(),
            vec![third.clone(), first.clone()]
        );
        drop(snapshot);
        drop(created);

        let volume = Volume::open(&file.path, None).unwrap();
        let snapshot = volume.snapshot().unwrap();
        assert_eq!(
            snapshot
                .projection_get(&namespace, &first)
                .unwrap()
                .unwrap(),
            b"A2"
        );
        assert_eq!(
            snapshot.projection_checkpoint(&namespace).unwrap(),
            LocalSeq::new(4)
        );
    }

    #[test]
    fn a_prefix_page_does_not_spill_into_the_next_prefix() {
        let file = TempFile::new();
        let created = Volume::create(&file.path, MasterCustody::Embedded).unwrap();
        let namespace = ns("calendar/day");
        let day = |n: i64, entity| {
            IndexKey::new(
                vec![
                    KeyComponent::asc(KeyScalar::Int(n)),
                    KeyComponent::asc(KeyScalar::Text("item".into())),
                ],
                entity,
            )
        };
        let march_a = day(3, eid(1));
        let march_b = day(3, eid(2));
        let april = day(4, eid(1));
        created
            .volume
            .apply_projection(
                &namespace,
                LocalSeq::ZERO,
                LocalSeq::new(1),
                &[
                    put(april.clone(), b"apr"),
                    put(march_b.clone(), b"b"),
                    put(march_a.clone(), b"a"),
                ],
            )
            .unwrap();
        let window = KeyWindow::Prefix(vec![KeyComponent::asc(KeyScalar::Int(3))]);
        let snapshot = created.volume.snapshot().unwrap();
        let first = snapshot
            .projection_range(&namespace, &window, ScanDir::Forward, None, 1)
            .unwrap();
        assert_eq!(first.rows[0].key, march_a);
        let second = snapshot
            .projection_range(
                &namespace,
                &window,
                ScanDir::Forward,
                Some(&KeyBound::Exclusive(first.resume.unwrap())),
                10,
            )
            .unwrap();
        assert_eq!(second.rows.len(), 1);
        assert_eq!(second.rows[0].key, march_b);
        assert!(second.resume.is_none());
        let desc = snapshot
            .projection_range(
                &ns("score/desc"),
                &KeyWindow::All,
                ScanDir::Forward,
                None,
                10,
            )
            .unwrap();
        assert!(desc.rows.is_empty());
    }

    #[test]
    fn descending_components_reverse_the_forward_scan() {
        let file = TempFile::new();
        let created = Volume::create(&file.path, MasterCustody::Embedded).unwrap();
        let namespace = ns("score/top");
        let score =
            |n: i64, entity| IndexKey::new(vec![KeyComponent::desc(KeyScalar::Int(n))], entity);
        let low = score(1, eid(1));
        let mid = score(3, eid(1));
        let high = score(10, eid(1));
        created
            .volume
            .apply_projection(
                &namespace,
                LocalSeq::ZERO,
                LocalSeq::new(1),
                &[
                    put(low.clone(), b"1"),
                    put(high.clone(), b"10"),
                    put(mid.clone(), b"3"),
                ],
            )
            .unwrap();
        let rows = created
            .volume
            .snapshot()
            .unwrap()
            .projection_range(&namespace, &KeyWindow::All, ScanDir::Forward, None, 10)
            .unwrap()
            .rows;
        assert_eq!(
            rows.into_iter().map(|row| row.key).collect::<Vec<_>>(),
            vec![high, mid, low]
        );
    }

    #[test]
    fn the_checkpoint_rejects_a_gap_a_rewind_and_a_stalled_write() {
        let file = TempFile::new();
        let created = Volume::create(&file.path, MasterCustody::Embedded).unwrap();
        let namespace = ns("todo/open");
        let key = text_key("a", eid(1));
        created
            .volume
            .apply_projection(
                &namespace,
                LocalSeq::ZERO,
                LocalSeq::new(2),
                &[put(key.clone(), b"v")],
            )
            .unwrap();
        let mismatch = created
            .volume
            .apply_projection(
                &namespace,
                LocalSeq::ZERO,
                LocalSeq::new(3),
                &[put(key.clone(), b"no")],
            )
            .unwrap_err();
        assert!(matches!(
            mismatch,
            StoreError::CheckpointMismatch {
                stored,
                expected
            } if stored == LocalSeq::new(2) && expected == LocalSeq::ZERO
        ));
        assert!(matches!(
            created
                .volume
                .apply_projection(&namespace, LocalSeq::new(2), LocalSeq::new(1), &[],),
            Err(StoreError::CheckpointRewind)
        ));
        assert!(matches!(
            created.volume.apply_projection(
                &namespace,
                LocalSeq::new(2),
                LocalSeq::new(2),
                &[put(key.clone(), b"no")],
            ),
            Err(StoreError::CheckpointStalled)
        ));
        created
            .volume
            .apply_projection(&namespace, LocalSeq::new(2), LocalSeq::new(2), &[])
            .unwrap();
        let snapshot = created.volume.snapshot().unwrap();
        assert_eq!(
            snapshot.projection_get(&namespace, &key).unwrap().unwrap(),
            b"v"
        );
        assert_eq!(
            snapshot.projection_checkpoint(&namespace).unwrap(),
            LocalSeq::new(2)
        );
        let other = ns("todo/done");
        assert_eq!(
            snapshot.projection_checkpoint(&other).unwrap(),
            LocalSeq::ZERO
        );
        assert!(snapshot
            .projection_range(&other, &KeyWindow::All, ScanDir::Forward, None, 10)
            .unwrap()
            .rows
            .is_empty());
    }

    #[test]
    fn a_corrupt_index_key_fails_the_scan() {
        let file = TempFile::new();
        let created = Volume::create(&file.path, MasterCustody::Embedded).unwrap();
        let namespace = ns("todo/open");
        let mut bad = namespace_prefix(&namespace).unwrap();
        bad.push(0);
        let txn = created.volume.database().begin_write().unwrap();
        {
            let mut table = txn.open_table(PROJECTIONS).unwrap();
            table.insert(bad.as_slice(), b"x".as_slice()).unwrap();
        }
        txn.commit().unwrap();
        let err = created
            .volume
            .snapshot()
            .unwrap()
            .projection_range(&namespace, &KeyWindow::All, ScanDir::Forward, None, 10)
            .unwrap_err();
        assert!(matches!(err, StoreError::CorruptValue));
    }

    #[test]
    fn applying_after_ingest_leaves_the_arrival_log_intact() {
        let file = TempFile::new();
        let created = Volume::create(&file.path, MasterCustody::Embedded).unwrap();
        let entity = eid(4);
        let block = Block {
            header: BlockHeader {
                entity_id: entity,
                parents: Vec::new(),
                schema_id: SchemaId::parse("notes/item").unwrap(),
                schema_version: BlockHash::ZERO,
                hlc: Hlc {
                    physical_ms: 1,
                    logical: 0,
                    node_id: 1,
                },
                is_tombstone: false,
            },
            payload: b"hello".to_vec(),
        };
        let ingested = created.volume.ingest(&block).unwrap();
        let seq = ingested.seq.unwrap();
        let namespace = ns("notes/by-title");
        let key = text_key("hello", entity);
        created
            .volume
            .apply_projection(
                &namespace,
                LocalSeq::ZERO,
                seq,
                &[put(key.clone(), b"hello")],
            )
            .unwrap();
        let snapshot = created.volume.snapshot().unwrap();
        assert_eq!(snapshot.projection_checkpoint(&namespace).unwrap(), seq);
        assert_eq!(
            snapshot.projection_get(&namespace, &key).unwrap().unwrap(),
            b"hello"
        );
        let arrived = snapshot
            .since(LocalSeq::ZERO)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(arrived, vec![(seq, ingested.hash)]);
        assert!(snapshot
            .projection_range(&namespace, &KeyWindow::All, ScanDir::Forward, None, 0)
            .unwrap()
            .rows
            .is_empty());
    }
}
