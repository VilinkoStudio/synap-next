//! 一轮对账。两边先比摘要，再只交换有差异的桶里缺的帧。

use std::collections::BTreeSet;

use synap_core::{MerkleDigest, SyncId};
use synap_store::{StoreError, Volume};

use crate::session::SyncError;

/// 一轮对账搬了多少帧。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncReport {
    /// 对端有、本端刚写入的帧数。已经有的也算一次。
    pub fetched: usize,
    /// 本端有、写入对端的帧数。
    pub pushed: usize,
}

/// 能拿出摘要、按桶列身份、按身份交帧的一端。
///
/// 卷和测试里的内存集合都实现它。盲中继不打开卷，继续用自己的 [`crate::SyncIdSet`]。
pub trait SyncEndpoint {
    /// 当前摘要。
    fn digest(&self) -> Result<MerkleDigest, SyncError>;

    /// 这些桶里的身份。
    fn ids_in_buckets(&self, buckets: &[u8]) -> Result<Vec<SyncId>, SyncError>;

    /// 这些身份对应的帧。
    fn frames(&self, ids: &[SyncId]) -> Result<Vec<(SyncId, Vec<u8>)>, SyncError>;

    /// 写入对端送来的帧。
    fn ingest_frames(&mut self, frames: &[(SyncId, Vec<u8>)]) -> Result<usize, SyncError>;
}

/// 把一个已经打开的卷接成对账的一端。
pub struct VolumeEndpoint<'a> {
    volume: &'a Volume,
}

impl<'a> VolumeEndpoint<'a> {
    /// 借用一个卷。入库走卷自己的方法，不要求独占。
    pub fn new(volume: &'a Volume) -> Self {
        Self { volume }
    }
}

impl SyncEndpoint for VolumeEndpoint<'_> {
    fn digest(&self) -> Result<MerkleDigest, SyncError> {
        self.volume.sync_digest().map_err(store_err)
    }

    fn ids_in_buckets(&self, buckets: &[u8]) -> Result<Vec<SyncId>, SyncError> {
        self.volume.sync_ids_in_buckets(buckets).map_err(store_err)
    }

    fn frames(&self, ids: &[SyncId]) -> Result<Vec<(SyncId, Vec<u8>)>, SyncError> {
        self.volume.sync_frames(ids).map_err(store_err)
    }

    fn ingest_frames(&mut self, frames: &[(SyncId, Vec<u8>)]) -> Result<usize, SyncError> {
        self.volume.ingest_frames(frames).map_err(store_err)
    }
}

/// 两边对账一次。身份在入库之前就算好，所以刚拉下来的帧不会被再推回去。
pub fn sync_with(
    local: &mut dyn SyncEndpoint,
    remote: &mut dyn SyncEndpoint,
) -> Result<SyncReport, SyncError> {
    let buckets = local.digest()?.diff_buckets(&remote.digest()?);
    if buckets.is_empty() {
        return Ok(SyncReport {
            fetched: 0,
            pushed: 0,
        });
    }
    let local_ids = local.ids_in_buckets(&buckets)?;
    let remote_ids = remote.ids_in_buckets(&buckets)?;
    let local_set: BTreeSet<SyncId> = local_ids.iter().copied().collect();
    let remote_set: BTreeSet<SyncId> = remote_ids.iter().copied().collect();
    let want_remote: Vec<SyncId> = remote_ids
        .into_iter()
        .filter(|id| !local_set.contains(id))
        .collect();
    let want_local: Vec<SyncId> = local_ids
        .into_iter()
        .filter(|id| !remote_set.contains(id))
        .collect();
    let incoming = remote.frames(&want_remote)?;
    let fetched = local.ingest_frames(&incoming)?;
    let outgoing = local.frames(&want_local)?;
    let pushed = remote.ingest_frames(&outgoing)?;
    Ok(SyncReport { fetched, pushed })
}

fn store_err(err: StoreError) -> SyncError {
    SyncError::Store(err.to_string())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use synap_core::{Block, BlockHeader, EntityId, Hlc, SchemaId};
    use synap_store::{MasterCustody, Volume};

    use super::*;

    struct TempFile {
        path: PathBuf,
    }

    impl TempFile {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let mut path = std::env::temp_dir();
            path.push(format!(
                "synap-exchange-{}-{}",
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
    fn two_volumes_meet_in_the_middle() {
        let left_file = TempFile::new();
        let right_file = TempFile::new();
        let left = Volume::create(&left_file.path, MasterCustody::Embedded).unwrap();
        let mut right = Volume::create(&right_file.path, MasterCustody::Embedded).unwrap();
        let exported = left
            .volume
            .export_content_key(&right.volume.device_id())
            .unwrap();
        right
            .volume
            .install_content_key(&right.master, &left.volume.device_id(), &exported)
            .unwrap();
        left.volume.ingest(&block(b"from-left")).unwrap();

        let mut local = VolumeEndpoint::new(&left.volume);
        let mut remote = VolumeEndpoint::new(&right.volume);
        let first = sync_with(&mut local, &mut remote).unwrap();
        assert_eq!(
            first,
            SyncReport {
                fetched: 0,
                pushed: 1
            }
        );
        assert_eq!(
            left.volume.sync_digest().unwrap(),
            right.volume.sync_digest().unwrap()
        );

        right.volume.ingest(&block(b"from-right")).unwrap();
        let second = sync_with(&mut local, &mut remote).unwrap();
        assert_eq!(
            second,
            SyncReport {
                fetched: 1,
                pushed: 0
            }
        );
        assert_eq!(
            left.volume.sync_digest().unwrap(),
            right.volume.sync_digest().unwrap()
        );
        let third = sync_with(&mut local, &mut remote).unwrap();
        assert_eq!(
            third,
            SyncReport {
                fetched: 0,
                pushed: 0
            }
        );
    }
}
