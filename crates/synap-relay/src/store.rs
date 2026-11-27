//! 盲存储：`SyncId -> 密文帧`，中继不解密、不校验内容。
//!
//! * `MemoryRelayStore`：开箱即用，重启丢集合（并集幂等，客户端下次对账重推）。
//! * `RedisRelayStore`：生产后端，键 `prefix:frame:{hex}` + `prefix:ids` SET +
//!   `prefix:bytes` 计数器；值仍是不透明帧。`put` 经 Lua 脚本一次原子提交。
//! * [`RelayBackend`]：HTTP/WS 层只认这个枚举，加后端不用改上层。

use std::collections::HashMap;
use synap_sync::{MerkleDigest, SyncId, SyncIdSet, SyncSession};

/// 中继存储后端。异步是为 Redis 留的；内存实现直接返回。
///
/// 只在进程内按枚举分发（见 [`RelayBackend`]），不做 `dyn RelayStore`，
/// 所以不需要给返回的 Future 加 `Send` 约束。
#[allow(async_fn_in_trait)]
pub trait RelayStore: Send + Sync + 'static {
    async fn put(&mut self, id: SyncId, frame: Vec<u8>);
    async fn get(&self, id: &SyncId) -> Option<Vec<u8>>;
    async fn sync_ids(&self) -> SyncIdSet;
    async fn total_bytes(&self) -> u64;
    async fn batch_get(&self, ids: &[SyncId]) -> Vec<(SyncId, Vec<u8>)> {
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(frame) = self.get(id).await {
                out.push((*id, frame));
            }
        }
        out
    }
    /// 存活探针：内存恒 ok，Redis 发 PING。
    async fn health(&self) -> Result<String, String> {
        Ok("ok".to_string())
    }
}

/// 内存实现：单测、本地开发、单机部署用。
#[derive(Debug, Default)]
pub struct MemoryRelayStore {
    frames: HashMap<SyncId, Vec<u8>>,
    ids: SyncIdSet,
    bytes: u64,
}

impl MemoryRelayStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }
}

impl RelayStore for MemoryRelayStore {
    async fn put(&mut self, id: SyncId, frame: Vec<u8>) {
        // 幂等覆盖：重复推送同一帧无副作用，无租约语义。
        if let Some(old) = self.frames.insert(id, frame) {
            self.bytes -= old.len() as u64;
        }
        self.bytes += self.frames.get(&id).map(Vec::len).unwrap_or(0) as u64;
        self.ids.insert(id);
    }

    async fn get(&self, id: &SyncId) -> Option<Vec<u8>> {
        self.frames.get(id).cloned()
    }

    async fn sync_ids(&self) -> SyncIdSet {
        self.ids.clone()
    }

    async fn total_bytes(&self) -> u64 {
        self.bytes
    }
}

/// Redis 实现：多实例共享同一后端时共用一套 Merkle 集合。
pub struct RedisRelayStore {
    mgr: redis::aio::ConnectionManager,
    prefix: String,
}

impl RedisRelayStore {
    pub async fn connect(url: &str, prefix: String) -> Result<Self, redis::RedisError> {
        let client = redis::Client::open(url)?;
        let mgr = client.get_connection_manager().await?;
        Ok(Self { mgr, prefix })
    }

    fn frame_key(&self, id: &SyncId) -> String {
        format!("{}:frame:{}", self.prefix, hex::encode(id.as_bytes()))
    }

    fn ids_key(&self) -> String {
        format!("{}:ids", self.prefix)
    }

    fn bytes_key(&self) -> String {
        format!("{}:bytes", self.prefix)
    }
}

impl RelayStore for RedisRelayStore {
    async fn put(&mut self, id: SyncId, frame: Vec<u8>) {
        // SET + SADD + 字节计数一次原子提交；重复推送只调计数差值。
        let script = redis::Script::new(
            r#"
            local old = redis.call('GET', KEYS[1])
            redis.call('SET', KEYS[1], ARGV[2])
            redis.call('SADD', KEYS[2], ARGV[1])
            local newlen = string.len(ARGV[2])
            if old then
                redis.call('INCRBY', KEYS[3], newlen - string.len(old))
            else
                redis.call('INCRBY', KEYS[3], newlen)
            end
            return 1
            "#,
        );
        let id_hex = hex::encode(id.as_bytes());
        let mut conn = self.mgr.clone();
        let _: () = script
            .key(self.frame_key(&id))
            .key(self.ids_key())
            .key(self.bytes_key())
            .arg(id_hex)
            .arg(frame)
            .invoke_async(&mut conn)
            .await
            .unwrap_or(());
    }

    async fn get(&self, id: &SyncId) -> Option<Vec<u8>> {
        let mut conn = self.mgr.clone();
        redis::cmd("GET")
            .arg(self.frame_key(id))
            .query_async(&mut conn)
            .await
            .unwrap_or(None)
    }

    async fn batch_get(&self, ids: &[SyncId]) -> Vec<(SyncId, Vec<u8>)> {
        if ids.is_empty() {
            return Vec::new();
        }
        let keys: Vec<String> = ids.iter().map(|id| self.frame_key(id)).collect();
        let mut conn = self.mgr.clone();
        let frames: Vec<Option<Vec<u8>>> = redis::cmd("MGET")
            .arg(keys)
            .query_async(&mut conn)
            .await
            .unwrap_or_default();
        ids.iter()
            .zip(frames)
            .filter_map(|(id, f)| f.map(|raw| (*id, raw)))
            .collect()
    }

    async fn sync_ids(&self) -> SyncIdSet {
        let mut conn = self.mgr.clone();
        let members: Vec<String> = redis::cmd("SMEMBERS")
            .arg(self.ids_key())
            .query_async(&mut conn)
            .await
            .unwrap_or_default();
        let mut set = SyncIdSet::new();
        for m in members {
            if let Ok(raw) = hex::decode(&m) {
                if let Ok(arr) = <[u8; 32]>::try_from(raw) {
                    set.insert(SyncId::from_bytes(arr));
                }
            }
        }
        set
    }

    async fn total_bytes(&self) -> u64 {
        let mut conn = self.mgr.clone();
        redis::cmd("GET")
            .arg(self.bytes_key())
            .query_async(&mut conn)
            .await
            .unwrap_or(None)
            .unwrap_or(0)
    }

    async fn health(&self) -> Result<String, String> {
        let mut conn = self.mgr.clone();
        redis::cmd("PING")
            .query_async::<String>(&mut conn)
            .await
            .map_err(|e| e.to_string())
    }
}

/// HTTP/WS 层只认这个枚举：加新后端只添变体。
pub enum RelayBackend {
    Memory(MemoryRelayStore),
    Redis(RedisRelayStore),
}

impl RelayBackend {
    pub fn memory() -> Self {
        Self::Memory(MemoryRelayStore::new())
    }

    pub async fn redis(url: &str, prefix: String) -> Result<Self, redis::RedisError> {
        Ok(Self::Redis(RedisRelayStore::connect(url, prefix).await?))
    }

    pub fn backend_name(&self) -> &'static str {
        match self {
            Self::Memory(_) => "memory",
            Self::Redis(_) => "redis",
        }
    }

    pub async fn frame_count(&self) -> usize {
        self.sync_ids().await.len()
    }
}

impl RelayStore for RelayBackend {
    async fn put(&mut self, id: SyncId, frame: Vec<u8>) {
        match self {
            Self::Memory(s) => s.put(id, frame).await,
            Self::Redis(s) => s.put(id, frame).await,
        }
    }

    async fn get(&self, id: &SyncId) -> Option<Vec<u8>> {
        match self {
            Self::Memory(s) => s.get(id).await,
            Self::Redis(s) => s.get(id).await,
        }
    }

    async fn sync_ids(&self) -> SyncIdSet {
        match self {
            Self::Memory(s) => s.sync_ids().await,
            Self::Redis(s) => s.sync_ids().await,
        }
    }

    async fn total_bytes(&self) -> u64 {
        match self {
            Self::Memory(s) => s.total_bytes().await,
            Self::Redis(s) => s.total_bytes().await,
        }
    }

    async fn batch_get(&self, ids: &[SyncId]) -> Vec<(SyncId, Vec<u8>)> {
        match self {
            Self::Memory(s) => s.batch_get(ids).await,
            Self::Redis(s) => s.batch_get(ids).await,
        }
    }

    async fn health(&self) -> Result<String, String> {
        match self {
            Self::Memory(s) => s.health().await,
            Self::Redis(s) => s.health().await,
        }
    }
}

/// 对账入口：本端摘要发给对端；对端回桶差异或全量 offer，再回缺块。
pub async fn local_digest<S: RelayStore>(store: &S) -> MerkleDigest {
    store.sync_ids().await.digest()
}

/// 对端 offer 全量 ID 时算出本端缺的（小库直连）。
pub async fn missing_from_offer<S: RelayStore>(store: &S, offered: &[SyncId]) -> Vec<SyncId> {
    let session = SyncSession::new(store.sync_ids().await);
    session
        .missing_from_offer(offered)
        .map(|w| w.ids)
        .unwrap_or_default()
}

/// 对端按桶索要 ID 列表时过滤。
pub async fn ids_in_buckets<S: RelayStore>(store: &S, buckets: &[u8]) -> Vec<SyncId> {
    let session = SyncSession::new(store.sync_ids().await);
    session.ids_in_buckets(buckets)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u8) -> SyncId {
        SyncId::from_bytes({
            let mut b = [0u8; 32];
            b[0] = n;
            b
        })
    }

    #[tokio::test]
    async fn put_is_idempotent_and_digest_tracks() {
        let mut s = MemoryRelayStore::new();
        s.put(id(1), b"frame1".to_vec()).await;
        s.put(id(1), b"frame1".to_vec()).await;
        assert_eq!(s.len(), 1);
        assert_eq!(s.total_bytes().await, 6);
        assert_eq!(s.get(&id(1)).await, Some(b"frame1".to_vec()));
        assert_eq!(local_digest(&s).await.count, 1);
    }

    #[tokio::test]
    async fn reconcile_finds_missing() {
        let mut relay = MemoryRelayStore::new();
        relay.put(id(1), b"a".to_vec()).await;
        assert_eq!(
            missing_from_offer(&relay, &[id(1), id(2)]).await,
            vec![id(2)]
        );
        assert_eq!(ids_in_buckets(&relay, &[1]).await, vec![id(1)]);
        assert!(ids_in_buckets(&relay, &[2]).await.is_empty());
    }
}
