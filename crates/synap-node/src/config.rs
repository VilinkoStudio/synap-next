//! 库配置：`init` 写入、`serve` 读出。只记事实，不玩花样。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const CONFIG_FILE: &str = "vault.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VaultConfig {
    pub server_name: String,
    pub created_at_ms: u64,
    pub format_v: u32,
}

impl VaultConfig {
    pub fn new(server_name: String, now_ms: u64) -> Self {
        Self {
            server_name,
            created_at_ms: now_ms,
            format_v: 1,
        }
    }

    pub fn path(data_dir: &Path) -> PathBuf {
        data_dir.join(CONFIG_FILE)
    }

    /// 已存在则拒绝，避免把已有库踩掉。
    pub fn init(data_dir: &Path, cfg: &VaultConfig) -> Result<PathBuf, ConfigError> {
        std::fs::create_dir_all(data_dir).map_err(|e| ConfigError::Io(e.to_string()))?;
        let path = Self::path(data_dir);
        if path.exists() {
            return Err(ConfigError::AlreadyInitialized(path));
        }
        let text =
            serde_json::to_string_pretty(cfg).map_err(|e| ConfigError::Encode(e.to_string()))?;
        std::fs::write(&path, text).map_err(|e| ConfigError::Io(e.to_string()))?;
        Ok(path)
    }

    pub fn load(data_dir: &Path) -> Result<Self, ConfigError> {
        let path = Self::path(data_dir);
        let text = std::fs::read_to_string(&path).map_err(|_| ConfigError::NotInitialized(path))?;
        serde_json::from_str(&text).map_err(|e| ConfigError::Decode(e.to_string()))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("io: {0}")]
    Io(String),
    #[error("encode: {0}")]
    Encode(String),
    #[error("decode: {0}")]
    Decode(String),
    #[error("already initialized: {0:?}")]
    AlreadyInitialized(PathBuf),
    #[error("not initialized: {0:?} (run `synap-node init` first)")]
    NotInitialized(PathBuf),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_then_load_round_trip() {
        let dir = std::env::temp_dir().join(format!("synap-node-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let cfg = VaultConfig::new("n1".to_string(), 42);
        let path = VaultConfig::init(&dir, &cfg).unwrap();
        assert_eq!(VaultConfig::load(&dir).unwrap(), cfg);
        assert!(path.ends_with(CONFIG_FILE));
        // 二次 init 拒绝。
        assert!(matches!(
            VaultConfig::init(&dir, &cfg),
            Err(ConfigError::AlreadyInitialized(_))
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_missing_hints_init() {
        let dir = std::env::temp_dir().join(format!("synap-node-missing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(matches!(
            VaultConfig::load(&dir),
            Err(ConfigError::NotInitialized(_))
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
