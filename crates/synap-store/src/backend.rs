//! 把 `redb` 的逻辑地址映射成固定大小的密封页槽。
//!
//! 逻辑上每 4096 字节是一块，和打开数据库时的页长相同。槽是 12 字节 nonce、
//! 4096 字节密文和 16 字节标签。缺槽读出来是全零。改写一块就换一把新 nonce。

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};

use redb::{BackendError, StorageBackend};

use crate::crypto::PageCodec;
use crate::error::StoreError;
use crate::volume::{store_logical_len, VolumeFile, CHUNK, HEADER_REGION, SLOT};

pub(crate) struct SealedBackend {
    file: std::sync::Arc<VolumeFile>,
    pages: PageCodec,
}

impl SealedBackend {
    pub(crate) fn new(file: std::sync::Arc<VolumeFile>, pages: PageCodec) -> Self {
        Self { file, pages }
    }
}

impl std::fmt::Debug for SealedBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SealedBackend").finish_non_exhaustive()
    }
}

impl StorageBackend for SealedBackend {
    fn len(&self) -> Result<u64, BackendError> {
        Ok(lock(&self.file)?.logical_len)
    }

    fn read(&self, offset: u64, out: &mut [u8]) -> Result<(), BackendError> {
        let mut state = lock(&self.file)?;
        read_range(&mut state, &self.pages, offset, out).map_err(into_backend)
    }

    fn set_len(&self, len: u64) -> Result<(), BackendError> {
        let mut state = lock(&self.file)?;
        set_logical(&mut state, &self.pages, len).map_err(into_backend)
    }

    fn sync_data(&self) -> Result<(), BackendError> {
        lock(&self.file)?.file.sync_all().map_err(BackendError::Io)
    }

    fn write(&self, offset: u64, data: &[u8]) -> Result<(), BackendError> {
        let mut state = lock(&self.file)?;
        write_range(&mut state, &self.pages, offset, data).map_err(into_backend)
    }

    fn close(&self) -> Result<(), BackendError> {
        self.sync_data()
    }
}

fn lock(
    file: &VolumeFile,
) -> Result<std::sync::MutexGuard<'_, crate::volume::State>, BackendError> {
    file.lock().map_err(into_backend)
}

fn into_backend(err: StoreError) -> BackendError {
    match err {
        StoreError::Io(err) => BackendError::Io(err),
        other => BackendError::Other(Box::new(other)),
    }
}

fn read_range(
    state: &mut crate::volume::State,
    pages: &PageCodec,
    offset: u64,
    out: &mut [u8],
) -> Result<(), StoreError> {
    if out.is_empty() {
        return Ok(());
    }
    let end = offset
        .checked_add(u64::try_from(out.len()).map_err(|_| StoreError::CorruptVolume)?)
        .ok_or(StoreError::CorruptVolume)?;
    if end > state.logical_len {
        return Err(StoreError::Io(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "read past volume length",
        )));
    }
    let first = offset / CHUNK as u64;
    let last = (end - 1) / CHUNK as u64;
    for index in first..=last {
        let plain = load_chunk(state, pages, index)?;
        copy_chunk(&plain, index, offset, end, out);
    }
    Ok(())
}

fn write_range(
    state: &mut crate::volume::State,
    pages: &PageCodec,
    offset: u64,
    data: &[u8],
) -> Result<(), StoreError> {
    if data.is_empty() {
        return Ok(());
    }
    let end = offset
        .checked_add(u64::try_from(data.len()).map_err(|_| StoreError::CorruptVolume)?)
        .ok_or(StoreError::CorruptVolume)?;
    if end > state.logical_len {
        state.logical_len = end;
        store_logical_len(&mut state.file, state.logical_len)?;
    }
    let first = offset / CHUNK as u64;
    let last = (end - 1) / CHUNK as u64;
    for index in first..=last {
        let mut plain = load_chunk(state, pages, index)?;
        let chunk_off = index * CHUNK as u64;
        let start = offset.max(chunk_off);
        let stop = end.min(chunk_off + CHUNK as u64);
        let dst = usize::try_from(start - chunk_off).map_err(|_| StoreError::CorruptVolume)?;
        let src = usize::try_from(start - offset).map_err(|_| StoreError::CorruptVolume)?;
        let n = usize::try_from(stop - start).map_err(|_| StoreError::CorruptVolume)?;
        plain[dst..dst + n].copy_from_slice(&data[src..src + n]);
        let keep = usize::try_from(state.logical_len.saturating_sub(chunk_off))
            .map_err(|_| StoreError::CorruptVolume)?
            .min(CHUNK);
        plain[keep..].fill(0);
        store_chunk(state, pages, index, &plain)?;
    }
    Ok(())
}

fn set_logical(
    state: &mut crate::volume::State,
    pages: &PageCodec,
    len: u64,
) -> Result<(), StoreError> {
    if len == state.logical_len {
        return Ok(());
    }
    if len < state.logical_len {
        let width = CHUNK as u64;
        if !len.is_multiple_of(width) {
            let index = len / width;
            if index < sealed_slots(&state.file)? {
                let mut plain = load_chunk(state, pages, index)?;
                let keep = usize::try_from(len % width).map_err(|_| StoreError::CorruptVolume)?;
                plain[keep..].fill(0);
                store_chunk(state, pages, index, &plain)?;
            }
        }
        let slots = len.div_ceil(width);
        let max_physical = HEADER_REGION as u64
            + slots
                .checked_mul(SLOT as u64)
                .ok_or(StoreError::CorruptVolume)?;
        let current = state.file.metadata()?.len();
        if current > max_physical {
            state.file.set_len(max_physical)?;
        }
    }
    state.logical_len = len;
    store_logical_len(&mut state.file, len)
}

fn copy_chunk(plain: &[u8], index: u64, offset: u64, end: u64, out: &mut [u8]) {
    let chunk_off = index * CHUNK as u64;
    let start = offset.max(chunk_off);
    let stop = end.min(chunk_off + CHUNK as u64);
    let src = (start - chunk_off) as usize;
    let dst = (start - offset) as usize;
    let n = (stop - start) as usize;
    out[dst..dst + n].copy_from_slice(&plain[src..src + n]);
}

fn load_chunk(
    state: &mut crate::volume::State,
    pages: &PageCodec,
    index: u64,
) -> Result<Vec<u8>, StoreError> {
    if index >= sealed_slots(&state.file)? {
        return Ok(vec![0u8; CHUNK]);
    }
    let mut slot = vec![0u8; SLOT];
    state.file.seek(SeekFrom::Start(slot_offset(index)?))?;
    state.file.read_exact(&mut slot)?;
    let plain = pages.open(index, &slot)?;
    if plain.len() != CHUNK {
        return Err(StoreError::CorruptVolume);
    }
    Ok(plain)
}

fn store_chunk(
    state: &mut crate::volume::State,
    pages: &PageCodec,
    index: u64,
    plain: &[u8],
) -> Result<(), StoreError> {
    let have = sealed_slots(&state.file)?;
    if index > have {
        let zeros = vec![0u8; CHUNK];
        for gap in have..index {
            write_slot(&mut state.file, pages, gap, &zeros)?;
        }
    }
    write_slot(&mut state.file, pages, index, plain)
}

fn write_slot(
    file: &mut File,
    pages: &PageCodec,
    index: u64,
    plain: &[u8],
) -> Result<(), StoreError> {
    let sealed = pages.seal(index, plain)?;
    if sealed.len() != SLOT {
        return Err(StoreError::CorruptVolume);
    }
    file.seek(SeekFrom::Start(slot_offset(index)?))?;
    file.write_all(&sealed)?;
    Ok(())
}

fn sealed_slots(file: &File) -> Result<u64, StoreError> {
    let len = file.metadata()?.len();
    if len < HEADER_REGION as u64 {
        return Err(StoreError::CorruptVolume);
    }
    let body = len - HEADER_REGION as u64;
    if !body.is_multiple_of(SLOT as u64) {
        return Err(StoreError::CorruptVolume);
    }
    Ok(body / SLOT as u64)
}

fn slot_offset(index: u64) -> Result<u64, StoreError> {
    index
        .checked_mul(SLOT as u64)
        .and_then(|off| off.checked_add(HEADER_REGION as u64))
        .ok_or(StoreError::CorruptVolume)
}
