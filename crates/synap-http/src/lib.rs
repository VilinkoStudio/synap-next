//! synap-http：节点的网络翻译层，与 `synap-bridge`（FFI）对等。
//!
//! 一个 crate，两套接口（同端口、同进程、同认证，共享 `NodeState`）：
//! - HTTP REST：给通用 App 用的。面向界面，按屏取数：稳定、分页、可 `watch`
//!   流。回答“这块界面要什么数据”。
//! - MCP tools（`/mcp`，见 `mcp` 模块）：给 AI 用的。面向上下文，组合、稠密、
//!   自描述：一次调用带回“能做什么 + 怎么做”。回答“智能体要理解什么、能干什么”。
//!
//! 两套接口**不要求一一对应**。镜像式 tool（和某条路由干同一件事）是坏味道；
//! 每个 MCP tool 必须靠“省 round trip 或编码一段工作流”证明自己。
//! 读写路由只做翻译：创建实体、调用 Trait、按投影读一页。
//! 命令行是这个进程的客户端，不另开一份库。MCP 不镜像这些路由。

mod api;
pub mod mcp;

pub use api::{Library, LibraryError};

pub use mcp::NodeMcp;

use std::sync::Arc;
use std::time::SystemTime;

use axum::{extract::State, http::StatusCode, routing::get, Json, Router};

use crate::api::library_routes;
use serde::Serialize;

/// 已经打开的库，给控制面看一眼。具体读写仍由节点进程持有。
pub trait VaultFacts: Send + Sync {
    /// 本机到达序号的末端。还没有块时是 0。
    fn tip(&self) -> Result<u64, String>;
    /// 主密钥放在哪：`embedded` 或 `external`。
    fn custody(&self) -> Result<String, String>;
}

/// 节点运行态。库在 `serve` 里打开，挂到这里给 HTTP 和 MCP 共用。
#[derive(Clone)]
pub struct NodeState {
    pub server_name: String,
    pub data_dir: String,
    pub started_at_ms: u64,
    pub library: Option<Arc<dyn Library>>,
}

impl std::fmt::Debug for NodeState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeState")
            .field("server_name", &self.server_name)
            .field("data_dir", &self.data_dir)
            .field("started_at_ms", &self.started_at_ms)
            .field("library", &self.library.is_some())
            .finish()
    }
}

impl NodeState {
    pub fn new(server_name: String, data_dir: String) -> Self {
        Self {
            server_name,
            data_dir,
            started_at_ms: now_ms(),
            library: None,
        }
    }

    /// 挂上已经打开的库。请求在库自己的锁后面串行执行。
    pub fn with_library(mut self, library: Arc<dyn Library>) -> Self {
        self.library = Some(library);
        self
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub(crate) fn uptime_ms(state: &NodeState) -> u64 {
    now_ms().saturating_sub(state.started_at_ms)
}

pub fn build_router(state: Arc<NodeState>) -> Router {
    let mcp: rmcp::transport::streamable_http_server::StreamableHttpService<
        NodeMcp,
        rmcp::transport::streamable_http_server::session::local::LocalSessionManager,
    > = rmcp::transport::streamable_http_server::StreamableHttpService::new(
        {
            let state = state.clone();
            move || Ok(NodeMcp::new(state.clone()))
        },
        Default::default(),
        rmcp::transport::streamable_http_server::StreamableHttpServerConfig::default(),
    );
    Router::new()
        .route("/healthz", get(healthz))
        .route("/version", get(version))
        .route("/v1/status", get(status))
        .merge(library_routes())
        .nest_service("/mcp", mcp)
        .with_state(state)
}

#[derive(Serialize)]
struct Health {
    status: &'static str,
}

async fn healthz() -> Json<Health> {
    Json(Health { status: "ok" })
}

#[derive(Serialize)]
struct Version {
    service: &'static str,
    version: &'static str,
    server_name: String,
}

async fn version(State(state): State<Arc<NodeState>>) -> Json<Version> {
    Json(Version {
        service: "synap-node",
        version: env!("CARGO_PKG_VERSION"),
        server_name: state.server_name.clone(),
    })
}

#[derive(Serialize)]
struct Status {
    service: &'static str,
    server_name: String,
    data_dir: String,
    uptime_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    tip: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    custody: Option<String>,
}

async fn status(State(state): State<Arc<NodeState>>) -> Result<Json<Status>, (StatusCode, String)> {
    let (tip, custody) = match &state.library {
        None => (None, None),
        Some(vault) => (
            Some(vault.tip().map_err(internal)?),
            Some(vault.custody().map_err(internal)?),
        ),
    };
    Ok(Json(Status {
        service: "synap-node",
        server_name: state.server_name.clone(),
        data_dir: state.data_dir.clone(),
        uptime_ms: now_ms().saturating_sub(state.started_at_ms),
        tip,
        custody,
    }))
}

fn internal(err: String) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    fn router() -> Router {
        build_router(Arc::new(NodeState::new(
            "test-node".to_string(),
            "/tmp/synap-test".to_string(),
        )))
    }

    async fn get(path: &str) -> (StatusCode, serde_json::Value) {
        let req = Request::builder().uri(path).body(Body::empty()).unwrap();
        let resp = router().oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn control_plane_answers() {
        let (st, body) = get("/healthz").await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(body["status"], "ok");

        let (st, body) = get("/version").await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(body["service"], "synap-node");
        assert_eq!(body["server_name"], "test-node");

        let (st, body) = get("/v1/status").await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(body["data_dir"], "/tmp/synap-test");
    }
}
