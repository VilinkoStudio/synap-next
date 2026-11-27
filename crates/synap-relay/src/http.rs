//! HTTP + WebSocket 服务：运维端点、同步 REST、双向对账 WS、邮箱信令。
//!
//! 同步批量建议客户端按 ≤200 帧分片（默认 2MB body 上限），丢了下次补上。

use std::sync::Arc;
use std::time::SystemTime;

use axum::{
    body::Bytes,
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, State,
    },
    http::{HeaderMap, Request, StatusCode},
    middleware::{self, Next},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::Serialize;

use crate::auth::{self, RelayAuth};
use crate::mailbox::MailboxStore;
use crate::proto::*;
use crate::store::{self, RelayBackend, RelayStore};

#[derive(Clone)]
pub struct AppState {
    inner: Arc<Inner>,
}

struct Inner {
    server_name: String,
    started_at_ms: u64,
    auth: RelayAuth,
    relay: tokio::sync::RwLock<RelayBackend>,
    mailbox: tokio::sync::Mutex<MailboxStore>,
}

impl AppState {
    pub fn new(server_name: String, auth: RelayAuth, backend: RelayBackend) -> Self {
        Self {
            inner: Arc::new(Inner {
                server_name,
                started_at_ms: now_ms(),
                auth,
                relay: tokio::sync::RwLock::new(backend),
                mailbox: tokio::sync::Mutex::new(MailboxStore::new()),
            }),
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Debug)]
struct AppError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl AppError {
    fn bad_request(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code,
            message: message.into(),
        }
    }
    fn unauthorized(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            code: "unauthorized",
            message: message.into(),
        }
    }
    fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code: "not_found",
            message: message.into(),
        }
    }
    fn unavailable(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "backend_unavailable",
            message: message.into(),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        #[derive(Serialize)]
        struct Body<'a> {
            code: &'a str,
            error: String,
        }
        (
            self.status,
            Json(Body {
                code: self.code,
                error: self.message,
            }),
        )
            .into_response()
    }
}

fn auth_err(e: auth::AuthError) -> AppError {
    match &e {
        auth::AuthError::Missing(_) | auth::AuthError::Invalid(_, _) => {
            AppError::bad_request(e.code(), e.to_string())
        }
        auth::AuthError::Unauthorized(_) => AppError::unauthorized(e.to_string()),
    }
}

fn require_api_key(state: &AppState, headers: &HeaderMap) -> Result<(), AppError> {
    auth::check_api_key(&state.inner.auth, headers).map_err(auth_err)
}

async fn api_key_mw(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: Request<axum::body::Body>,
    next: Next,
) -> Result<impl IntoResponse, AppError> {
    require_api_key(&state, &headers)?;
    Ok(next.run(request).await)
}

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/", get(root))
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/status", get(status))
        .route("/v1/sync/digest", get(sync_digest))
        .route("/v1/sync/diff", post(sync_diff))
        .route("/v1/sync/offer", post(sync_offer))
        .route("/v1/sync/ids", post(sync_ids))
        .route("/v1/sync/put", post(sync_put))
        .route("/v1/sync/fetch", post(sync_fetch))
        .route("/v1/sync/ws", get(sync_ws))
        .route(
            "/v1/mailboxes/{mailbox}",
            post(post_mailbox).get(get_mailbox),
        )
        .route("/v1/mailboxes/{mailbox}/acks", post(post_ack))
        .layer(middleware::from_fn_with_state(state.clone(), api_key_mw))
        .with_state(state)
}

// ---- 运维 ----

#[derive(Serialize)]
struct ServiceInfo {
    service: &'static str,
    server_name: String,
    mode: &'static str,
}

async fn root(State(state): State<AppState>) -> Json<ServiceInfo> {
    Json(ServiceInfo {
        service: "synap-relay",
        server_name: state.inner.server_name.clone(),
        mode: "blind",
    })
}

#[derive(Serialize)]
struct Health {
    status: &'static str,
    server_name: String,
    backend: String,
    uptime_ms: u64,
}

async fn healthz(State(state): State<AppState>) -> Json<Health> {
    let relay = state.inner.relay.read().await;
    Json(Health {
        status: "ok",
        server_name: state.inner.server_name.clone(),
        backend: relay.backend_name().to_string(),
        uptime_ms: now_ms().saturating_sub(state.inner.started_at_ms),
    })
}

#[derive(Serialize)]
struct Ready {
    ready: bool,
    backend: String,
}

async fn readyz(State(state): State<AppState>) -> Result<Json<Ready>, AppError> {
    let relay = state.inner.relay.read().await;
    relay.health().await.map_err(AppError::unavailable)?;
    Ok(Json(Ready {
        ready: true,
        backend: relay.backend_name().to_string(),
    }))
}

#[derive(Serialize)]
struct StatusResponse {
    service: &'static str,
    server_name: String,
    backend: String,
    frames: usize,
    bytes: u64,
    mailboxes: usize,
    buffered_slots: usize,
    total_post: u64,
    total_ack: u64,
    uptime_ms: u64,
}

async fn status(State(state): State<AppState>) -> Json<StatusResponse> {
    let relay = state.inner.relay.read().await;
    let mailbox = state.inner.mailbox.lock().await;
    Json(StatusResponse {
        service: "synap-relay",
        server_name: state.inner.server_name.clone(),
        backend: relay.backend_name().to_string(),
        frames: relay.frame_count().await,
        bytes: relay.total_bytes().await,
        mailboxes: mailbox.mailbox_count(),
        buffered_slots: mailbox.buffered_slots(),
        total_post: mailbox.total_post,
        total_ack: mailbox.total_ack,
        uptime_ms: now_ms().saturating_sub(state.inner.started_at_ms),
    })
}

// ---- 同步 REST ----

async fn sync_digest(State(state): State<AppState>) -> Json<DigestResponse> {
    let relay = state.inner.relay.read().await;
    let d = store::local_digest(&*relay).await;
    Json(DigestResponse::from(&d))
}

async fn sync_diff(
    State(state): State<AppState>,
    Json(body): Json<DigestBody>,
) -> Result<Json<DiffResponse>, AppError> {
    let remote = body
        .to_digest()
        .map_err(|e| AppError::bad_request("bad_digest", e))?;
    let relay = state.inner.relay.read().await;
    let buckets = relay.sync_ids().await.digest().diff_buckets(&remote);
    Ok(Json(DiffResponse { buckets }))
}

async fn sync_offer(
    State(state): State<AppState>,
    Json(body): Json<OfferBody>,
) -> Result<Json<WantResponse>, AppError> {
    let mut offered = Vec::with_capacity(body.ids.len());
    for s in &body.ids {
        offered.push(id_from_hex(s).map_err(|e| AppError::bad_request("bad_id", e))?);
    }
    let relay = state.inner.relay.read().await;
    let want = store::missing_from_offer(&*relay, &offered).await;
    Ok(Json(WantResponse {
        want: want.iter().map(id_to_hex).collect(),
    }))
}

async fn sync_ids(State(state): State<AppState>, Json(body): Json<IdsBody>) -> Json<IdsResponse> {
    let relay = state.inner.relay.read().await;
    let ids = store::ids_in_buckets(&*relay, &body.buckets).await;
    Json(IdsResponse {
        ids: ids.iter().map(id_to_hex).collect(),
    })
}

async fn sync_put(
    State(state): State<AppState>,
    Json(body): Json<PutBody>,
) -> Result<Json<PutResponse>, AppError> {
    let mut relay = state.inner.relay.write().await;
    let mut accepted = 0;
    for f in &body.frames {
        let (id, raw) = f
            .decode()
            .map_err(|e| AppError::bad_request("bad_frame", e))?;
        relay.put(id, raw).await;
        accepted += 1;
    }
    Ok(Json(PutResponse { accepted }))
}

async fn sync_fetch(
    State(state): State<AppState>,
    Json(body): Json<FetchBody>,
) -> Result<Json<FetchResponse>, AppError> {
    let mut ids = Vec::with_capacity(body.ids.len());
    for s in &body.ids {
        ids.push(id_from_hex(s).map_err(|e| AppError::bad_request("bad_id", e))?);
    }
    let relay = state.inner.relay.read().await;
    let frames = relay
        .batch_get(&ids)
        .await
        .iter()
        .map(|(id, raw)| FrameJson::encode(id, raw))
        .collect();
    Ok(Json(FetchResponse { frames }))
}

// ---- 双向对账 WS：任一端先 Hello，对称状态机 ----

async fn sync_ws(State(state): State<AppState>, upgrade: WebSocketUpgrade) -> impl IntoResponse {
    upgrade.on_upgrade(move |socket| ws_loop(state, socket))
}

async fn ws_loop(state: AppState, mut socket: WebSocket) {
    while let Some(msg) = socket.recv().await {
        let msg = match msg {
            Ok(Message::Text(text)) => text.to_string(),
            Ok(Message::Close(_)) | Err(_) => break,
            _ => continue,
        };
        let incoming: WsMsg = match serde_json::from_str(&msg) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let reply: Option<WsMsg> = match incoming {
            WsMsg::Hello {
                count,
                root,
                buckets,
            } => {
                let remote = DigestBody {
                    count,
                    root,
                    buckets,
                }
                .to_digest();
                match remote {
                    Ok(d) => {
                        let relay = state.inner.relay.read().await;
                        let buckets = relay.sync_ids().await.digest().diff_buckets(&d);
                        Some(WsMsg::Diff { buckets })
                    }
                    Err(_) => None,
                }
            }
            WsMsg::Offer { ids } => {
                let mut offered = Vec::with_capacity(ids.len());
                let mut ok = true;
                for s in &ids {
                    match id_from_hex(s) {
                        Ok(id) => offered.push(id),
                        Err(_) => {
                            ok = false;
                            break;
                        }
                    }
                }
                if !ok {
                    None
                } else {
                    let relay = state.inner.relay.read().await;
                    let want = store::missing_from_offer(&*relay, &offered).await;
                    Some(WsMsg::Want {
                        want: want.iter().map(id_to_hex).collect(),
                    })
                }
            }
            WsMsg::WantReq { ids } => {
                let mut wanted = Vec::with_capacity(ids.len());
                let mut ok = true;
                for s in &ids {
                    match id_from_hex(s) {
                        Ok(id) => wanted.push(id),
                        Err(_) => {
                            ok = false;
                            break;
                        }
                    }
                }
                if !ok {
                    None
                } else {
                    let relay = state.inner.relay.read().await;
                    let frames = relay
                        .batch_get(&wanted)
                        .await
                        .iter()
                        .map(|(id, raw)| FrameJson::encode(id, raw))
                        .collect();
                    Some(WsMsg::Batch { frames })
                }
            }
            WsMsg::Batch { frames } => {
                let mut relay = state.inner.relay.write().await;
                let mut accepted = 0;
                for f in &frames {
                    if let Ok((id, raw)) = f.decode() {
                        relay.put(id, raw).await;
                        accepted += 1;
                    }
                }
                Some(WsMsg::Ack { accepted })
            }
            WsMsg::Want { .. } | WsMsg::Diff { .. } | WsMsg::Ack { .. } | WsMsg::Done {} => None,
        };
        if let Some(reply) = reply {
            let text = serde_json::to_string(&reply).unwrap_or_default();
            if socket.send(Message::Text(text.into())).await.is_err() {
                break;
            }
            if matches!(reply, WsMsg::Done {}) {
                break;
            }
        }
    }
}

// ---- 邮箱信令（一次性消息，保留老语义） ----

async fn post_mailbox(
    State(state): State<AppState>,
    Path(mailbox): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, AppError> {
    let mailbox = auth::normalize_mailbox_key(&mailbox).map_err(auth_err)?;
    if body.is_empty() {
        return Err(AppError::bad_request(
            "empty_body",
            "envelope body required",
        ));
    }
    let path = format!("/v1/mailboxes/{mailbox}");
    auth::verify_mailbox_request(&mailbox, &headers, "POST", &path, auth::now_ms())
        .map_err(auth_err)?;
    let sender = headers
        .get(auth::MESSAGE_SENDER_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let mut m = state.inner.mailbox.lock().await;
    m.post(&mailbox, &sender, body.to_vec(), auth::now_ms());
    m.cleanup(auth::now_ms());
    Ok(Json(serde_json::json!({"stored": true})))
}

async fn get_mailbox(
    State(state): State<AppState>,
    Path(mailbox): Path<String>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AppError> {
    let mailbox = auth::normalize_mailbox_key(&mailbox).map_err(auth_err)?;
    let path = format!("/v1/mailboxes/{mailbox}");
    auth::verify_mailbox_request(&mailbox, &headers, "GET", &path, auth::now_ms())
        .map_err(auth_err)?;
    let mut m = state.inner.mailbox.lock().await;
    m.cleanup(auth::now_ms());
    match m.take(&mailbox, auth::now_ms()) {
        None => Ok((StatusCode::NO_CONTENT, HeaderMap::new(), Bytes::new()).into_response()),
        Some(leased) => {
            let mut h = HeaderMap::new();
            h.insert("x-synap-lease-id", leased.lease_id.parse().unwrap());
            h.insert(
                "x-synap-leased-until",
                leased.leased_until_ms.to_string().parse().unwrap(),
            );
            h.insert("x-synap-sender-ed25519", leased.sender.parse().unwrap());
            Ok((StatusCode::OK, h, Bytes::from(leased.body)).into_response())
        }
    }
}

#[derive(serde::Deserialize)]
struct AckBody {
    lease_id: String,
}

async fn post_ack(
    State(state): State<AppState>,
    Path(mailbox): Path<String>,
    headers: HeaderMap,
    Json(body): Json<AckBody>,
) -> Result<impl IntoResponse, AppError> {
    let mailbox = auth::normalize_mailbox_key(&mailbox).map_err(auth_err)?;
    let path = format!("/v1/mailboxes/{mailbox}/acks");
    auth::verify_mailbox_request(&mailbox, &headers, "POST", &path, auth::now_ms())
        .map_err(auth_err)?;
    let mut m = state.inner.mailbox.lock().await;
    if m.ack(&mailbox, &body.lease_id, auth::now_ms()) {
        Ok(Json(serde_json::json!({"acked": true})))
    } else {
        Err(AppError::not_found("unknown lease"))
    }
}
