//! 给界面用的三条资源。形状稳定，一次只做一件事。

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;

use crate::{NodeState, VaultFacts};

/// 节点进程里那把库。HTTP 不关心它是不是 `Engine`。
pub trait Library: VaultFacts {
    /// 按 Schema 造一条实体，返回它的 id。
    fn create_entity(&self, schema: &str, args: serde_json::Value) -> Result<String, LibraryError>;

    /// 对一条实体调用 Trait 写方法。
    fn call_entity(
        &self,
        entity: &str,
        trait_name: &str,
        method: &str,
        args: serde_json::Value,
    ) -> Result<(), LibraryError>;

    /// 按投影名字读一页覆盖值。
    fn rows(&self, projection: &str, limit: usize) -> Result<Vec<serde_json::Value>, LibraryError>;

    /// 库里的全部定义、实体数、问题和写插件的说明。
    fn describe(&self) -> Result<serde_json::Value, LibraryError>;

    /// 一条实体的字段、能力值和版本历史。
    fn get_entity(&self, entity: &str) -> Result<serde_json::Value, LibraryError>;

    /// 试一个插件源码；`install` 为真且试过了就装进库里并重开。
    fn plugin(&self, source: &str, install: bool) -> Result<serde_json::Value, LibraryError>;
}

/// 库拒绝了这次调用。`status` 是要回给客户端的 HTTP 状态。
#[derive(Debug)]
pub struct LibraryError {
    /// HTTP 状态。
    pub status: StatusCode,
    /// 给人看的原因。
    pub message: String,
}

pub(crate) fn library_routes() -> Router<Arc<NodeState>> {
    Router::new()
        .route("/v1/entities", post(create_entity))
        .route("/v1/entities/{id}/calls", post(call_entity))
        .route("/v1/rows", get(rows))
}

#[derive(Debug, Deserialize)]
struct CreateBody {
    schema: String,
    #[serde(default)]
    args: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct CallBody {
    #[serde(rename = "trait")]
    trait_name: String,
    method: String,
    #[serde(default)]
    args: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct RowsQuery {
    projection: String,
    limit: Option<usize>,
}

async fn create_entity(
    State(state): State<Arc<NodeState>>,
    Json(body): Json<CreateBody>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, String)> {
    let library = state.library.as_ref().ok_or_else(closed)?;
    let id = library
        .create_entity(&body.schema, body.args)
        .map_err(fail)?;
    Ok((StatusCode::CREATED, Json(serde_json::json!({ "id": id }))))
}

async fn call_entity(
    State(state): State<Arc<NodeState>>,
    Path(id): Path<String>,
    Json(body): Json<CallBody>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let library = state.library.as_ref().ok_or_else(closed)?;
    library
        .call_entity(&id, &body.trait_name, &body.method, body.args)
        .map_err(fail)?;
    Ok(Json(serde_json::json!({ "id": id })))
}

async fn rows(
    State(state): State<Arc<NodeState>>,
    Query(query): Query<RowsQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let limit = query.limit.unwrap_or(50);
    if limit > 200 {
        return Err((
            StatusCode::BAD_REQUEST,
            "limit must be at most 200".to_owned(),
        ));
    }
    let library = state.library.as_ref().ok_or_else(closed)?;
    let rows = library.rows(&query.projection, limit).map_err(fail)?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

fn closed() -> (StatusCode, String) {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "library is not open".to_owned(),
    )
}

fn fail(err: LibraryError) -> (StatusCode, String) {
    (err.status, err.message)
}
