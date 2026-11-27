//! 数据目录里的库。只有 `serve` 长期握着打开的卷，`init` 建完就放手。

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use synap_store::{MasterCustody, Volume};

use crate::config::{ConfigError, VaultConfig};

pub const VOLUME_FILE: &str = "volume";

pub struct Opened {
    pub created: bool,
    pub config: VaultConfig,
    volume: Volume,
}

impl Opened {
    #[cfg(test)]
    pub fn volume(&self) -> &Volume {
        &self.volume
    }

    pub fn into_volume(self) -> Volume {
        self.volume
    }
}

/// 目录还是空的时候建库。已经有库就拒绝，避免把卷盖掉。
pub fn create(data_dir: &Path, server_name: &str) -> Result<VaultConfig, VaultError> {
    if config_exists(data_dir) || volume_path(data_dir).exists() {
        return Err(VaultError::AlreadyInitialized(data_dir.to_path_buf()));
    }
    let opened = create_new(data_dir, server_name)?;
    Ok(opened.config)
}

/// `serve` 用。没有库就建一个，有库就打开。
pub fn open_or_create(data_dir: &Path, server_name: &str) -> Result<Opened, VaultError> {
    let has_config = config_exists(data_dir);
    let has_volume = volume_path(data_dir).exists();
    match (has_config, has_volume) {
        (true, true) => open_existing(data_dir),
        (false, false) => create_new(data_dir, server_name),
        _ => Err(VaultError::Incomplete(data_dir.to_path_buf())),
    }
}

fn create_new(data_dir: &Path, server_name: &str) -> Result<Opened, VaultError> {
    std::fs::create_dir_all(data_dir).map_err(|err| VaultError::Io(err.to_string()))?;
    let path = volume_path(data_dir);
    let created = Volume::create(&path, MasterCustody::Embedded).map_err(|err| {
        let _ = std::fs::remove_file(&path);
        VaultError::Store(err.to_string())
    })?;
    let config = VaultConfig::new(server_name.to_owned(), now_ms());
    if let Err(err) = VaultConfig::init(data_dir, &config) {
        drop(created);
        let _ = std::fs::remove_file(&path);
        return Err(VaultError::Config(err));
    }
    Ok(Opened {
        created: true,
        config,
        volume: created.volume,
    })
}

fn open_existing(data_dir: &Path) -> Result<Opened, VaultError> {
    let config = VaultConfig::load(data_dir)?;
    let volume = Volume::open(&volume_path(data_dir), None)
        .map_err(|err| VaultError::Store(err.to_string()))?;
    Ok(Opened {
        created: false,
        config,
        volume,
    })
}

fn volume_path(data_dir: &Path) -> PathBuf {
    data_dir.join(VOLUME_FILE)
}

/// 这个目录里已经有一个完整的库。
pub fn exists(data_dir: &Path) -> bool {
    config_exists(data_dir) && volume_path(data_dir).exists()
}

fn config_exists(data_dir: &Path) -> bool {
    VaultConfig::path(data_dir).exists()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error("io: {0}")]
    Io(String),
    #[error("store: {0}")]
    Store(String),
    #[error("already initialized: {0}")]
    AlreadyInitialized(PathBuf),
    #[error("vault.json and volume must both exist in {0}")]
    Incomplete(PathBuf),
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "synap-node-{label}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ));
            Self { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn create_then_serve_opens_the_same_volume() {
        let dir = TempDir::new("vault");
        let config = create(&dir.path, "n1").unwrap();
        assert_eq!(config.server_name, "n1");
        let opened = open_or_create(&dir.path, "other").unwrap();
        assert!(!opened.created);
        assert_eq!(opened.config.server_name, "n1");
        let tip = opened.volume().snapshot().unwrap().tip().unwrap().get();
        assert_eq!(tip, 0);
        assert_eq!(opened.volume().custody().unwrap(), MasterCustody::Embedded);
        assert!(matches!(
            create(&dir.path, "n1"),
            Err(VaultError::AlreadyInitialized(_))
        ));
    }
}
