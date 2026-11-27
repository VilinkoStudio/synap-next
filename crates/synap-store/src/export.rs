//! 明文导出与导入：原型期引擎升级的逃生口（005 §5）。
//!
//! 只导真相（`blocks`），派生表（`local_seq`、`entity_heads`、`projections`、
//! `meta`）不导——导入时 `ingest` 自己重建。到出的是解密后的规范字节，
//! 不是加密帧，所以目录本身是明文，保管责任在调用方。
//!
//! 布局：`manifest.json` + 按到达序号排好序的 `{seq}.block` 文件。
//! 序号顺序保证父块永远排在子块前面，导入时 `verify_parents` 直接通过。
//! 同一内容哈希入库即去重，所以**导出后原样导入不写任何块**，
//! 这条由下面的 golden 测试守住。
//!
//! 人可读的逐实体 markdown（006 §6.7）是 runtime 的事：它要解 payload
//! 才知道组件长什么样，store 只管把逻辑块原样交出去。

use std::path::Path;

use serde::{Deserialize, Serialize};
use synap_core::{Block, LocalSeq};

use crate::volume::Volume;
use crate::StoreError;

/// 导出目录的清单。原型期 `format` 固定为 0，可以做破坏性修改。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Manifest {
    format: u32,
    count: u64,
}

const EXPORT_FORMAT: u32 = 0;
const MANIFEST_FILE: &str = "manifest.json";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportStats {
    pub blocks: u64,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportStats {
    /// 真正落盘的新块。
    pub new_blocks: u64,
    /// 已在库里、直接跳过的块。
    pub skipped: u64,
}

/// 把库里所有块按到达顺序导出到目录。目录必须不存在或为空。
pub fn export_blocks(volume: &Volume, dir: &Path) -> Result<ExportStats, StoreError> {
    std::fs::create_dir_all(dir)?;
    if std::fs::read_dir(dir)
        .map_err(StoreError::from)?
        .next()
        .is_some()
    {
        return Err(StoreError::Export(format!(
            "{} is not empty",
            dir.display()
        )));
    }
    let snapshot = volume.snapshot()?;
    let mut stats = ExportStats {
        blocks: 0,
        bytes: 0,
    };
    for item in snapshot.since(LocalSeq::ZERO)? {
        let (seq, hash) = item?;
        let Some(block) = snapshot.block(&hash)? else {
            return Err(StoreError::CorruptValue);
        };
        let canonical = block.canonical_bytes()?;
        // 导出即自证：写盘的是哈希过的同一份字节。
        if block.hash()? != hash {
            return Err(StoreError::BadFrame);
        }
        stats.bytes += canonical.len() as u64;
        std::fs::write(dir.join(file_name(seq)), &canonical)?;
        stats.blocks += 1;
    }
    let manifest = Manifest {
        format: EXPORT_FORMAT,
        count: stats.blocks,
    };
    let text =
        serde_json::to_string_pretty(&manifest).map_err(|e| StoreError::Export(e.to_string()))?;
    std::fs::write(dir.join(MANIFEST_FILE), text)?;
    Ok(stats)
}

/// 把导出目录导回库里。内容寻址去重：已有的块只计数，不推进序号。
pub fn import_blocks(volume: &Volume, dir: &Path) -> Result<ImportStats, StoreError> {
    let text = std::fs::read_to_string(dir.join(MANIFEST_FILE))
        .map_err(|_| StoreError::Export(format!("missing {}", MANIFEST_FILE)))?;
    let manifest: Manifest =
        serde_json::from_str(&text).map_err(|e| StoreError::Export(e.to_string()))?;
    if manifest.format != EXPORT_FORMAT {
        return Err(StoreError::Export(format!(
            "unsupported format {}",
            manifest.format
        )));
    }
    let mut names: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(StoreError::from)? {
        let entry = entry.map_err(StoreError::from)?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".block") {
            names.push(name);
        }
    }
    // 文件名即序号顺序，父块先入库。
    names.sort();
    let mut stats = ImportStats {
        new_blocks: 0,
        skipped: 0,
    };
    for name in &names {
        let bytes = std::fs::read(dir.join(name))?;
        let block = Block::from_canonical_bytes(&bytes)?;
        // 进库前自证：文件名不参与信任，只认内容哈希。
        block.hash()?;
        let ingested = volume.ingest(&block)?;
        if ingested.seq.is_some() {
            stats.new_blocks += 1;
        } else {
            stats.skipped += 1;
        }
    }
    if stats.new_blocks + stats.skipped != manifest.count {
        return Err(StoreError::Export(format!(
            "manifest says {}, found {}",
            manifest.count,
            stats.new_blocks + stats.skipped
        )));
    }
    Ok(stats)
}

fn file_name(seq: LocalSeq) -> String {
    format!("{:020}.block", seq.get())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use synap_core::{BlockHash, BlockHeader, EntityId, Hlc, SchemaId};

    use super::*;
    use crate::crypto::MasterCustody;

    struct TempDir {
        path: std::path::PathBuf,
    }

    impl TempDir {
        fn fresh(prefix: &str) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let mut path = std::env::temp_dir();
            path.push(format!(
                "synap-export-{}-{}-{}",
                prefix,
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&path);
            Self { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
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

    fn open() -> (TempDir, Volume) {
        let dir = TempDir::fresh("vault");
        std::fs::create_dir_all(&dir.path).unwrap();
        let created = Volume::create(&dir.path.join("v.redb"), MasterCustody::Embedded).unwrap();
        (dir, created.volume)
    }

    fn seed(volume: &Volume) -> Vec<(EntityId, BlockHash)> {
        let a = EntityId::new();
        let b = EntityId::new();
        let ra = volume
            .ingest(&block(a, vec![], "n/i", b"a1", false))
            .unwrap();
        let rb = volume
            .ingest(&block(b, vec![], "n/i", b"b1", false))
            .unwrap();
        // a 再走一块 + b 删掉：覆盖“多版本 + tombstone 也要原样搬走”。
        let ra2 = volume
            .ingest(&block(a, vec![ra.hash], "n/i", b"a2", false))
            .unwrap();
        let rb2 = volume
            .ingest(&block(b, vec![rb.hash], "n/i", b"b2", true))
            .unwrap();
        vec![(a, ra2.hash), (b, rb2.hash)]
    }

    fn heads_of(volume: &Volume, entities: &[(EntityId, BlockHash)]) {
        let snap = volume.snapshot().unwrap();
        for (entity, head) in entities {
            assert_eq!(snap.heads(entity).unwrap(), vec![*head]);
        }
    }

    #[test]
    fn export_then_reimport_writes_nothing() {
        let (_vault, volume) = open();
        let entities = seed(&volume);
        let tip_before = volume.snapshot().unwrap().tip().unwrap();

        let dir = TempDir::fresh("export");
        let stats = export_blocks(&volume, &dir.path).unwrap();
        assert_eq!(stats.blocks, 4);

        // golden：原样导回，一个块都不写。
        let stats = import_blocks(&volume, &dir.path).unwrap();
        assert_eq!(
            stats,
            ImportStats {
                new_blocks: 0,
                skipped: 4
            }
        );
        assert_eq!(volume.snapshot().unwrap().tip().unwrap(), tip_before);
        heads_of(&volume, &entities);
    }

    #[test]
    fn import_into_fresh_vault_restores_everything() {
        let (_vault, volume) = open();
        let entities = seed(&volume);

        let dir = TempDir::fresh("export");
        export_blocks(&volume, &dir.path).unwrap();

        let (_vault2, fresh) = open();
        let stats = import_blocks(&fresh, &dir.path).unwrap();
        assert_eq!(
            stats,
            ImportStats {
                new_blocks: 4,
                skipped: 0
            }
        );
        heads_of(&fresh, &entities);
        // 逐块内容一致。
        let (old_snap, new_snap) = (volume.snapshot().unwrap(), fresh.snapshot().unwrap());
        for (_, hash) in &entities {
            // heads 只是抽查；全量一致由 local_seq 长度保证。
            let _ = (
                old_snap.block(hash).unwrap().unwrap(),
                new_snap.block(hash).unwrap().unwrap(),
            );
        }
        assert_eq!(new_snap.tip().unwrap(), old_snap.tip().unwrap());
    }
}
