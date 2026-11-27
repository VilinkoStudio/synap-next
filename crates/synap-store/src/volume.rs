//! 卷文件。头部固定占前 4096 字节，落在页加密之外。
//!
//! 把主密钥从调用方保管改成明文写入时，头部会变长，但不会挪动后面的页槽。
//! 逻辑长度记在这 4096 字节的末尾 8 字节。`redb` 看见的长度是逻辑长度，
//! 每一页再密封进后面的槽。

use redb::ReadableDatabase;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};

use zeroize::Zeroize;

use crate::backend::SealedBackend;
use crate::crypto::{
    CryptoError, CryptoHeader, DeviceId, FrameCodec, MasterCustody, MasterKey, Session,
    WrappedSecret,
};
use crate::error::StoreError;
use crate::tables::{BLOCKS, ENTITY_HEADS, LOCAL_SEQ, META, PROJECTIONS};

/// 和 `redb` 默认页长一致。改这一刀时两边要一起改。
pub(crate) const CHUNK: usize = 4096;
/// nonce 12 + 页 4096 + Poly1305 标签 16。
pub(crate) const SLOT: usize = 12 + CHUNK + 16;
/// 卷头保留区。页槽从这里之后开始，嵌入主密钥也不会把它们往后挤。
pub(crate) const HEADER_REGION: usize = CHUNK;
pub(crate) const LOGICAL_LEN_OFFSET: usize = HEADER_REGION - 8;

const MAGIC: &[u8; 8] = b"SYNAPVOL";
const FORMAT_VERSION: u8 = 1;
const CRYPTO_OFFSET: usize = 13;

pub(crate) struct State {
    pub(crate) file: File,
    pub(crate) logical_len: u64,
}

pub(crate) struct VolumeFile {
    inner: Mutex<State>,
}

impl VolumeFile {
    pub(crate) fn lock(&self) -> Result<std::sync::MutexGuard<'_, State>, StoreError> {
        self.inner.lock().map_err(|_| StoreError::Poison)
    }
}

/// 打开之后的卷。页已经由派生页密钥密封，帧用 [`Volume::frame_codec`]。
pub struct Volume {
    db: redb::Database,
    file: Arc<VolumeFile>,
    header: Mutex<CryptoHeader>,
    session: Session,
    pub(crate) coord: Arc<crate::round::LoopState>,
}

/// [`Volume::create`] 的结果。`External` 时卷头里没有主密钥，调用方必须留下它。
pub struct CreatedVolume {
    /// 已经建好五张空表的卷。
    pub volume: Volume,
    /// 刚生成的主密钥。`Embedded` 时卷头里还有一份明文。
    pub master: MasterKey,
}

impl Volume {
    /// 新建卷文件。两种存放方式都会把主密钥交还调用方。
    pub fn create(path: &Path, custody: MasterCustody) -> Result<CreatedVolume, StoreError> {
        let created = CryptoHeader::create(custody);
        let session = created.header.unlock(&created.master)?;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)?;
        match finish(file, created.header, session) {
            Ok(volume) => Ok(CreatedVolume {
                volume,
                master: created.master,
            }),
            Err(err) => {
                let _ = std::fs::remove_file(path);
                Err(err)
            }
        }
    }

    /// 打开已有的卷。主密钥明文在卷头里时，`master` 可以是 `None`。
    pub fn open(path: &Path, master: Option<&MasterKey>) -> Result<Self, StoreError> {
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)?;
        let mut region = vec![0u8; HEADER_REGION];
        file.read_exact(&mut region)?;
        let parsed = parse_region(&region);
        region.zeroize();
        let (header, logical_len) = parsed?;
        let session = match (header.custody(), master) {
            (MasterCustody::Embedded, None) => header.unlock_embedded()?,
            (_, Some(master)) => header.unlock(master)?,
            (MasterCustody::External, None) => {
                return Err(CryptoError::CustodyMismatch.into());
            }
        };
        let shared = Arc::new(VolumeFile {
            inner: Mutex::new(State { file, logical_len }),
        });
        let db = open_database(&shared, &session)?;
        ensure_tables(&db)?;
        let volume = Self {
            db,
            file: shared,
            header: Mutex::new(header),
            session,
            coord: Arc::new(crate::round::LoopState::new()),
        };
        crate::sync_index::ensure(&volume)?;
        Ok(volume)
    }

    /// 主密钥目前放在哪。
    pub fn custody(&self) -> Result<MasterCustody, StoreError> {
        Ok(self
            .header
            .lock()
            .map_err(|_| StoreError::Poison)?
            .custody())
    }

    /// 把主密钥明文写入卷头。页槽不用重写。
    pub fn embed_master(&self, master: &MasterKey) -> Result<(), StoreError> {
        let mut header = self.header.lock().map_err(|_| StoreError::Poison)?;
        header.embed_master(master)?;
        if let Err(err) = self.persist_header(&header) {
            let _ = header.extract_master();
            return Err(err);
        }
        Ok(())
    }

    /// 从卷头取出明文主密钥。页槽不用重写。
    pub fn extract_master(&self) -> Result<MasterKey, StoreError> {
        let mut header = self.header.lock().map_err(|_| StoreError::Poison)?;
        let master = header.extract_master()?;
        if let Err(err) = self.persist_header(&header) {
            let _ = header.embed_master(&master);
            return Err(err);
        }
        Ok(master)
    }

    /// 本机设备号。配对时交给对端。
    pub fn device_id(&self) -> DeviceId {
        self.session.device_id()
    }

    /// 把内容密钥包给 `peer`。对端用它换成和本卷相同的库密钥。
    pub fn export_content_key(&self, peer: &DeviceId) -> Result<WrappedSecret, StoreError> {
        let shared = self.session.agree(peer)?;
        Ok(self.session.export_content_key(&shared)?)
    }

    /// 装上对端送来的内容密钥。页密钥仍由本机主密钥派生。
    ///
    /// 已有的帧必须能用这把新密钥解开，否则头部会退回安装之前。
    pub fn install_content_key(
        &mut self,
        master: &MasterKey,
        peer: &DeviceId,
        exported: &WrappedSecret,
    ) -> Result<(), StoreError> {
        let shared = self.session.agree(peer)?;
        let backup = {
            let header = self.header.lock().map_err(|_| StoreError::Poison)?;
            header.to_bytes()?
        };
        let new_session = {
            let mut header = self.header.lock().map_err(|_| StoreError::Poison)?;
            if let Err(err) = header.install_content_key(master, &shared, exported) {
                restore_header(&mut header, &backup);
                return Err(err.into());
            }
            let session = match header.unlock(master) {
                Ok(session) => session,
                Err(err) => {
                    restore_header(&mut header, &backup);
                    return Err(err.into());
                }
            };
            if let Err(err) = self.persist_header(&header) {
                restore_header(&mut header, &backup);
                return Err(err);
            }
            session
        };
        let previous = std::mem::replace(&mut self.session, new_session);
        if let Err(err) = crate::sync_index::rebuild(self) {
            self.session = previous;
            if let Ok(mut header) = self.header.lock() {
                restore_header(&mut header, &backup);
                let _ = self.persist_header(&header);
            }
            return Err(err);
        }
        Ok(())
    }

    /// 帧编码器。压缩走 `zrip`，再套内容密钥。
    pub fn frame_codec(&self) -> FrameCodec {
        self.session.frame_codec()
    }

    pub(crate) fn sync_key(&self) -> [u8; 32] {
        self.session.sync_key()
    }

    /// 已经按页密封的数据库。表定义是 [`BLOCKS`] 这些常量。
    pub fn database(&self) -> &redb::Database {
        &self.db
    }

    fn persist_header(&self, header: &CryptoHeader) -> Result<(), StoreError> {
        let mut bytes = header.to_bytes()?;
        let result = (|| -> Result<(), StoreError> {
            let mut state = self.file.lock()?;
            let logical_len = state.logical_len;
            write_region(&mut state.file, &bytes, logical_len)?;
            state.file.sync_all()?;
            Ok(())
        })();
        bytes.zeroize();
        result
    }
}

fn finish(file: File, header: CryptoHeader, session: Session) -> Result<Volume, StoreError> {
    let mut bytes = header.to_bytes()?;
    let shared = Arc::new(VolumeFile {
        inner: Mutex::new(State {
            file,
            logical_len: 0,
        }),
    });
    let written = (|| -> Result<(), StoreError> {
        let mut state = shared.lock()?;
        write_region(&mut state.file, &bytes, 0)?;
        state.file.sync_all()?;
        Ok(())
    })();
    bytes.zeroize();
    written?;
    let db = open_database(&shared, &session)?;
    ensure_tables(&db)?;
    let volume = Volume {
        db,
        file: shared,
        header: Mutex::new(header),
        session,
        coord: Arc::new(crate::round::LoopState::new()),
    };
    crate::sync_index::ensure(&volume)?;
    Ok(volume)
}

fn restore_header(header: &mut CryptoHeader, backup: &[u8]) {
    if let Ok(restored) = CryptoHeader::from_bytes(backup) {
        *header = restored;
    }
}

fn open_database(file: &Arc<VolumeFile>, session: &Session) -> Result<redb::Database, StoreError> {
    let backend = SealedBackend::new(Arc::clone(file), session.page_codec());
    // 默认页长是 4096，和 [`CHUNK`] 相同。`set_page_size` 不在 redb 的正式接口里。
    Ok(redb::Database::builder().create_with_backend(backend)?)
}

fn ensure_tables(db: &redb::Database) -> Result<(), StoreError> {
    // 表已经在时不要再提交。空提交会重写页槽，嵌入主密钥前后的页就对不上。
    const NAMES: [&str; 5] = ["blocks", "local_seq", "entity_heads", "projections", "meta"];
    {
        let txn = db.begin_read()?;
        let mut found = [false; NAMES.len()];
        for handle in txn.list_tables()? {
            let name = redb::TableHandle::name(&handle);
            for (index, expected) in NAMES.iter().enumerate() {
                if name == *expected {
                    found[index] = true;
                }
            }
        }
        if found.iter().all(|present| *present) {
            return Ok(());
        }
    }
    let txn = db.begin_write()?;
    {
        let _blocks = txn.open_table(BLOCKS)?;
        let _local_seq = txn.open_table(LOCAL_SEQ)?;
        let _heads = txn.open_table(ENTITY_HEADS)?;
        let _projections = txn.open_table(PROJECTIONS)?;
        let _meta = txn.open_table(META)?;
    }
    txn.commit()?;
    Ok(())
}

pub(crate) fn store_logical_len(file: &mut File, logical_len: u64) -> Result<(), StoreError> {
    file.seek(SeekFrom::Start(LOGICAL_LEN_OFFSET as u64))?;
    file.write_all(&logical_len.to_le_bytes())?;
    Ok(())
}

fn write_region(file: &mut File, crypto: &[u8], logical_len: u64) -> Result<(), StoreError> {
    if crypto.len() > LOGICAL_LEN_OFFSET - CRYPTO_OFFSET {
        return Err(StoreError::CorruptVolume);
    }
    let mut region = vec![0u8; HEADER_REGION];
    region[..MAGIC.len()].copy_from_slice(MAGIC);
    region[MAGIC.len()] = FORMAT_VERSION;
    let len = u32::try_from(crypto.len()).map_err(|_| StoreError::CorruptVolume)?;
    region[9..CRYPTO_OFFSET].copy_from_slice(&len.to_le_bytes());
    region[CRYPTO_OFFSET..CRYPTO_OFFSET + crypto.len()].copy_from_slice(crypto);
    region[LOGICAL_LEN_OFFSET..].copy_from_slice(&logical_len.to_le_bytes());
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&region)?;
    region.zeroize();
    Ok(())
}

fn parse_region(region: &[u8]) -> Result<(CryptoHeader, u64), StoreError> {
    if region.len() != HEADER_REGION || region[..MAGIC.len()] != MAGIC[..] {
        return Err(StoreError::CorruptVolume);
    }
    if region[MAGIC.len()] != FORMAT_VERSION {
        return Err(CryptoError::UnsupportedVersion.into());
    }
    let crypto_len = u32::from_le_bytes(
        region[9..CRYPTO_OFFSET]
            .try_into()
            .expect("crypto length is 4 bytes"),
    ) as usize;
    let crypto_end = CRYPTO_OFFSET
        .checked_add(crypto_len)
        .ok_or(StoreError::CorruptVolume)?;
    if crypto_end > LOGICAL_LEN_OFFSET {
        return Err(StoreError::CorruptVolume);
    }
    let header = CryptoHeader::from_bytes(&region[CRYPTO_OFFSET..crypto_end])?;
    let logical_len = u64::from_le_bytes(
        region[LOGICAL_LEN_OFFSET..]
            .try_into()
            .expect("logical length is 8 bytes"),
    );
    Ok((header, logical_len))
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Seek, SeekFrom, Write};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use redb::{ReadableDatabase, ReadableTable};

    use crate::crypto::{CryptoError, FrameKind, MasterCustody, MasterKey};
    use crate::error::StoreError;
    use crate::tables::META;

    use super::{Volume, HEADER_REGION};

    const MARKER: &[u8] = b"synap-volume-marker";

    struct TempFile {
        path: PathBuf,
    }

    impl TempFile {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let mut path = std::env::temp_dir();
            path.push(format!(
                "synap-vol-{}-{}",
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

    fn write_meta(volume: &Volume, key: &[u8], value: &[u8]) {
        let txn = volume.db.begin_write().unwrap();
        {
            let mut table = txn.open_table(META).unwrap();
            table.insert(key, value).unwrap();
        }
        txn.commit().unwrap();
    }

    fn read_meta(volume: &Volume, key: &[u8]) -> Vec<u8> {
        let txn = volume.db.begin_read().unwrap();
        let table = txn.open_table(META).unwrap();
        let value = table.get(key).unwrap().unwrap().value().to_vec();
        value
    }

    fn slots(path: &Path) -> Vec<u8> {
        let raw = std::fs::read(path).unwrap();
        raw[HEADER_REGION..].to_vec()
    }

    #[test]
    fn embedded_roundtrip_and_a_torn_page_fails() {
        let file = TempFile::new();
        let created = Volume::create(&file.path, MasterCustody::Embedded).unwrap();
        assert_eq!(created.volume.custody().unwrap(), MasterCustody::Embedded);
        let frame = created
            .volume
            .frame_codec()
            .seal(FrameKind::Block, b"frame-body")
            .unwrap();
        assert_eq!(frame[0], 0x15);
        let opened = created.volume.frame_codec().open(&frame).unwrap();
        assert_eq!(opened.plaintext, b"frame-body");
        write_meta(&created.volume, b"k", MARKER);
        let master = created.master;
        drop(created.volume);

        assert!(matches!(
            Volume::open(&file.path, Some(&MasterKey::generate())),
            Err(StoreError::Crypto(CryptoError::Unwrap))
        ));
        let volume = Volume::open(&file.path, None).unwrap();
        assert_eq!(read_meta(&volume, b"k"), MARKER);
        let raw = std::fs::read(&file.path).unwrap();
        assert!(!raw.windows(MARKER.len()).any(|window| window == MARKER));
        assert!(matches!(
            volume.embed_master(&master),
            Err(StoreError::Crypto(CryptoError::CustodyMismatch))
        ));
        drop(volume);

        flip_slot(&file.path);
        assert!(Volume::open(&file.path, None).is_err());
    }

    #[test]
    fn moving_the_master_does_not_rewrite_pages() {
        let file = TempFile::new();
        let created = Volume::create(&file.path, MasterCustody::External).unwrap();
        assert!(matches!(
            Volume::open(&file.path, None),
            Err(StoreError::Crypto(CryptoError::CustodyMismatch))
        ));
        write_meta(&created.volume, b"k", MARKER);
        let before = slots(&file.path);
        let raw = std::fs::read(&file.path).unwrap();
        assert!(!raw
            .windows(32)
            .any(|window| window == created.master.as_bytes()));
        created.volume.embed_master(&created.master).unwrap();
        assert_eq!(created.volume.custody().unwrap(), MasterCustody::Embedded);
        assert_eq!(slots(&file.path), before);
        assert!(!std::fs::read(&file.path)
            .unwrap()
            .windows(MARKER.len())
            .any(|window| window == MARKER));
        drop(created.volume);

        // 关闭时 redb 会记下分配器状态，那一次提交会重密封被改写的页。
        // 取出主密钥本身不再改页。
        let volume = Volume::open(&file.path, None).unwrap();
        assert_eq!(read_meta(&volume, b"k"), MARKER);
        let after_open = slots(&file.path);
        let master = volume.extract_master().unwrap();
        assert_eq!(volume.custody().unwrap(), MasterCustody::External);
        assert_eq!(slots(&file.path), after_open);
        drop(volume);

        assert!(matches!(
            Volume::open(&file.path, None),
            Err(StoreError::Crypto(CryptoError::CustodyMismatch))
        ));
        let volume = Volume::open(&file.path, Some(&master)).unwrap();
        assert_eq!(read_meta(&volume, b"k"), MARKER);
    }

    fn flip_slot(path: &Path) {
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap();
        let at = HEADER_REGION as u64 + 32;
        file.seek(SeekFrom::Start(at)).unwrap();
        let mut byte = [0u8; 1];
        file.read_exact(&mut byte).unwrap();
        byte[0] ^= 0xff;
        file.seek(SeekFrom::Start(at)).unwrap();
        file.write_all(&byte).unwrap();
        file.sync_all().unwrap();
    }
}
