use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use synap_core::{CoreError, Kv, KvPairs};

const KV_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("kv");

/// 本机文件引擎：redb 单表 `kv`，key/value 都是字节。
/// 写入路径短，按键覆盖；版本和同步按此之上的字节语义走。
pub struct RedbKv {
    db: Database,
}

impl RedbKv {
    pub fn open(path: &std::path::Path) -> Result<Self, CoreError> {
        let db = Database::create(path).map_err(|e| CoreError::Storage(e.to_string()))?;
        {
            let txn = db
                .begin_write()
                .map_err(|e| CoreError::Storage(e.to_string()))?;
            txn.open_table(KV_TABLE)
                .map_err(|e| CoreError::Storage(e.to_string()))?;
            txn.commit()
                .map_err(|e| CoreError::Storage(e.to_string()))?;
        }
        Ok(Self { db })
    }
}

impl Kv for RedbKv {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, CoreError> {
        let txn = self
            .db
            .begin_read()
            .map_err(|e| CoreError::Storage(e.to_string()))?;
        let table = txn
            .open_table(KV_TABLE)
            .map_err(|e| CoreError::Storage(e.to_string()))?;
        let guard = table
            .get(key)
            .map_err(|e| CoreError::Storage(e.to_string()))?;
        Ok(guard.map(|g| g.value().to_vec()))
    }

    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), CoreError> {
        let txn = self
            .db
            .begin_write()
            .map_err(|e| CoreError::Storage(e.to_string()))?;
        {
            let mut table = txn
                .open_table(KV_TABLE)
                .map_err(|e| CoreError::Storage(e.to_string()))?;
            table
                .insert(key, value)
                .map_err(|e| CoreError::Storage(e.to_string()))?;
        }
        txn.commit()
            .map_err(|e| CoreError::Storage(e.to_string()))?;
        Ok(())
    }

    fn delete(&self, key: &[u8]) -> Result<(), CoreError> {
        let txn = self
            .db
            .begin_write()
            .map_err(|e| CoreError::Storage(e.to_string()))?;
        {
            let mut table = txn
                .open_table(KV_TABLE)
                .map_err(|e| CoreError::Storage(e.to_string()))?;
            table
                .remove(key)
                .map_err(|e| CoreError::Storage(e.to_string()))?;
        }
        txn.commit()
            .map_err(|e| CoreError::Storage(e.to_string()))?;
        Ok(())
    }

    fn scan_prefix(&self, prefix: &[u8]) -> Result<KvPairs, CoreError> {
        let txn = self
            .db
            .begin_read()
            .map_err(|e| CoreError::Storage(e.to_string()))?;
        let table = txn
            .open_table(KV_TABLE)
            .map_err(|e| CoreError::Storage(e.to_string()))?;
        // redb 无前缀扫描，用全表过滤；键量大了再换引擎，不在桥里特判。
        let mut out = Vec::new();
        let iter = table
            .iter()
            .map_err(|e| CoreError::Storage(e.to_string()))?;
        for item in iter {
            let (k, v) = item.map_err(|e| CoreError::Storage(e.to_string()))?;
            if k.value().starts_with(prefix) {
                out.push((k.value().to_vec(), v.value().to_vec()));
            }
        }
        Ok(out)
    }
}
