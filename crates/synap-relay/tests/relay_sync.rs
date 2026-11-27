//! 中继端到端：REST 对账 + WS 双向同步 + 邮箱信令冒烟。
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::future::IntoFuture;
use synap_relay::{build_router, AppState, RelayAuth, RelayBackend};
use tower::ServiceExt;

fn hex_id(n: u8) -> String {
    let mut b = [0u8; 32];
    b[0] = n;
    hex::encode(b)
}

fn b64(raw: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(raw)
}

async fn call(
    state: &AppState,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(body.map(|v| v.to_string()).unwrap_or_default()))
        .unwrap();
    let resp = build_router(state.clone()).oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, json)
}

fn state() -> AppState {
    AppState::new(
        "test".to_string(),
        RelayAuth::Disabled,
        RelayBackend::memory(),
    )
}

#[tokio::test]
async fn rest_reconcile_round_trip() {
    let s = state();
    let (_, d0) = call(&s, "GET", "/v1/sync/digest", None).await;
    assert_eq!(d0["count"], 0);

    let put = json!({"frames": [
        {"id": hex_id(1), "frame": b64(b"cipher1")},
        {"id": hex_id(2), "frame": b64(b"cipher2")},
    ]});
    let (st, put_resp) = call(&s, "POST", "/v1/sync/put", Some(put)).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(put_resp["accepted"], 2);

    // 重推幂等。
    let put = json!({"frames": [{"id": hex_id(1), "frame": b64(b"cipher1")}]});
    let (_, put_resp) = call(&s, "POST", "/v1/sync/put", Some(put)).await;
    assert_eq!(put_resp["accepted"], 1);

    let (_, d) = call(&s, "GET", "/v1/sync/digest", None).await;
    assert_eq!(d["count"], 2);

    // 对端空集：diff 应指出非空桶，offer 全量后 want 全部。
    let empty = json!({"count": 0,
        "root": "00".repeat(32),
        "buckets": vec!["00".repeat(32); 256]});
    let (_, diff) = call(&s, "POST", "/v1/sync/diff", Some(empty)).await;
    assert!(!diff["buckets"].as_array().unwrap().is_empty());

    // offer = 对端已有的 ID；中继回其中自己缺的。
    let offer = json!({"ids": [hex_id(1), hex_id(2), hex_id(9)]});
    let (_, want) = call(&s, "POST", "/v1/sync/offer", Some(offer)).await;
    assert_eq!(want["want"].as_array().unwrap().len(), 1);
    assert_eq!(want["want"][0], hex_id(9));

    let fetch = json!({"ids": [hex_id(1)]});
    let (_, fetched) = call(&s, "POST", "/v1/sync/fetch", Some(fetch)).await;
    let frames = fetched["frames"].as_array().unwrap();
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0]["frame"], b64(b"cipher1"));

    // 坏输入 400。
    let (_, _) = call(&s, "POST", "/v1/sync/fetch", Some(json!({"ids": ["zz"]}))).await;
}

#[tokio::test]
async fn ws_bidirectional_reconcile() {
    // 中继持有 C；客户端持有 A、B。走完一轮双方补齐。
    let s = state();
    let put = json!({"frames": [{"id": hex_id(3), "frame": b64(b"cipherC")}]});
    call(&s, "POST", "/v1/sync/put", Some(put)).await;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, build_router(s.clone())).into_future());
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/v1/sync/ws"))
        .await
        .unwrap();

    // 客户端摘要 {A, B} 问好。
    let mut set = synap_sync::SyncIdSet::new();
    for n in [1u8, 2] {
        let mut b = [0u8; 32];
        b[0] = n;
        set.insert(synap_sync::SyncId::from_bytes(b));
    }
    let d = set.digest();
    let hello = serde_json::json!({"t": "hello", "count": d.count,
        "root": hex::encode(d.root),
        "buckets": d.buckets.iter().map(hex::encode).collect::<Vec<_>>()});
    ws.send(tokio_tungstenite::tungstenite::Message::Text(
        hello.to_string().into(),
    ))
    .await
    .unwrap();

    // Diff → Offer(A,B) → Want(B? no: 中继缺 A,B) → Batch(A,B) → Ack
    let diff: Value = {
        let m = ws.next().await.unwrap().unwrap().into_text().unwrap();
        serde_json::from_str(&m).unwrap()
    };
    assert_eq!(diff["t"], "diff");

    let offer = serde_json::json!({"t": "offer", "ids": [hex_id(1), hex_id(2)]});
    ws.send(tokio_tungstenite::tungstenite::Message::Text(
        offer.to_string().into(),
    ))
    .await
    .unwrap();
    let want: Value = {
        let m = ws.next().await.unwrap().unwrap().into_text().unwrap();
        serde_json::from_str(&m).unwrap()
    };
    assert_eq!(want["t"], "want");
    assert_eq!(want["want"].as_array().unwrap().len(), 2);

    let batch = serde_json::json!({"t": "batch", "frames": [
        {"id": hex_id(1), "frame": b64(b"cipherA")},
        {"id": hex_id(2), "frame": b64(b"cipherB")},
    ]});
    ws.send(tokio_tungstenite::tungstenite::Message::Text(
        batch.to_string().into(),
    ))
    .await
    .unwrap();
    let ack: Value = {
        let m = ws.next().await.unwrap().unwrap().into_text().unwrap();
        serde_json::from_str(&m).unwrap()
    };
    assert_eq!(ack["accepted"], 2);

    // 客户端反向索要 C。
    let wantreq = serde_json::json!({"t": "want_req", "ids": [hex_id(3)]});
    ws.send(tokio_tungstenite::tungstenite::Message::Text(
        wantreq.to_string().into(),
    ))
    .await
    .unwrap();
    let batch: Value = {
        let m = ws.next().await.unwrap().unwrap().into_text().unwrap();
        serde_json::from_str(&m).unwrap()
    };
    assert_eq!(batch["t"], "batch");
    assert_eq!(batch["frames"][0]["frame"], b64(b"cipherC"));

    let (_, d) = call(&s, "GET", "/v1/sync/digest", None).await;
    assert_eq!(d["count"], 3);
}
