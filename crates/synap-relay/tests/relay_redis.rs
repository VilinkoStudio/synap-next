//! Redis 后端集成测试：需要真实 redis（`SYNAP_RELAY_TEST_REDIS`
//! 未设置时默认连 127.0.0.1:6399，连不上则跳过）。
//!
//! 跑法：先起一个本地 redis，再 `cargo test -p synap-relay`。

use synap_relay::{RedisRelayStore, RelayBackend, RelayStore};
use synap_sync::SyncId;

fn redis_url() -> String {
    std::env::var("SYNAP_RELAY_TEST_REDIS").unwrap_or("redis://127.0.0.1:6399/".to_string())
}

fn id(n: u8) -> SyncId {
    SyncId::from_bytes({
        let mut b = [0u8; 32];
        b[0] = n;
        b
    })
}

async fn try_connect(prefix: &str) -> Option<RedisRelayStore> {
    let url = redis_url();
    match RedisRelayStore::connect(&url, prefix.to_string()).await {
        Ok(s) => Some(s),
        Err(e) => {
            eprintln!("skip redis test (no server at {url}: {e})");
            None
        }
    }
}

#[tokio::test]
async fn redis_put_get_digest_round_trip() {
    let Some(mut s) = try_connect("test:rt").await else {
        return;
    };
    s.put(id(11), b"redframe".to_vec()).await;
    s.put(id(11), b"redframe".to_vec()).await; // 幂等
    assert_eq!(s.get(&id(11)).await, Some(b"redframe".to_vec()));
    assert_eq!(s.get(&id(12)).await, None);
    assert!(s.sync_ids().await.contains(&id(11)));
    assert!(s.total_bytes().await >= 8);
}

#[tokio::test]
async fn redis_batch_and_overwrite_accounting() {
    let Some(mut s) = try_connect("test:batch").await else {
        return;
    };
    s.put(id(21), b"aaaa".to_vec()).await;
    s.put(id(22), b"bb".to_vec()).await;
    s.put(id(21), b"aaaaaa".to_vec()).await; // 覆盖：4→6，+2
    let got = s.batch_get(&[id(21), id(22), id(23)]).await;
    assert_eq!(got.len(), 2);
    assert_eq!(s.total_bytes().await, 8);
    // 覆盖后摘要仍一致可算。
    assert_eq!(s.sync_ids().await.len(), 2);
}

#[tokio::test]
async fn redis_backend_enum_smoke() {
    let Some(inner) = try_connect("test:enum").await else {
        return;
    };
    let _url = redis_url();
    let mut backend = RelayBackend::Redis(inner);
    assert_eq!(backend.backend_name(), "redis");
    backend.put(id(31), b"z".to_vec()).await;
    assert_eq!(backend.frame_count().await, 1);
    assert!(backend.health().await.is_ok());
}
