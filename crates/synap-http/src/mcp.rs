//! MCP 工具面：窄腰契约的机器入口，与 `synap-bridge`（FFI）对等。
//!
//! 设计规则（见 crate 根文档）：**MCP 不镜像 HTTP**。tool 面向上下文——
//! 组合、稠密、自描述，目标是智能体用最少的 round trip 从“理解”走到“动手”。
//! 工具按一次会话的顺序排：`describe`（定义、实体数、问题和写插件的说明一次带回）
//! → `get` / `query`（读一条、读一个投影）→ `construct` / `call`（经沙箱的写入）
//! → `plugin`（试一个插件对现有数据的影响，确认后装进库）。
//! 错误以 `is_error` 返回，正文是给模型看的原因。

use std::sync::Arc;

use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{ServerCapabilities, ServerConfig},
    tool, tool_handler, tool_router, ServerHandler,
};

use crate::{Library, LibraryError, NodeState};

#[derive(Debug, Clone)]
pub struct NodeMcp {
    state: Arc<NodeState>,
    tool_router: ToolRouter<Self>,
}

impl NodeMcp {
    pub fn new(state: Arc<NodeState>) -> Self {
        Self {
            tool_router: Self::tool_router(),
            state,
        }
    }
}

#[derive(Debug, serde::Deserialize, rmcp::schemars::JsonSchema)]
struct NoArgs {}

#[derive(Debug, serde::Deserialize, rmcp::schemars::JsonSchema)]
struct GetArgs {
    /// Entity id (uuid v7).
    entity: String,
}

#[derive(Debug, serde::Deserialize, rmcp::schemars::JsonSchema)]
struct QueryArgs {
    /// Projection name, as listed by `describe`.
    projection: String,
    /// Max rows, default 50, at most 1000.
    limit: Option<usize>,
}

#[derive(Debug, serde::Deserialize, rmcp::schemars::JsonSchema)]
struct ConstructArgs {
    /// Schema id with major, e.g. `std/context@1`.
    schema: String,
    /// Construct arguments as a JSON object.
    #[serde(default)]
    args: serde_json::Value,
}

#[derive(Debug, serde::Deserialize, rmcp::schemars::JsonSchema)]
struct CallArgs {
    /// Entity id (uuid v7).
    entity: String,
    /// Trait id with major, e.g. `std/Titled@1`.
    #[serde(rename = "trait")]
    trait_id: String,
    /// Write method declared by the trait.
    method: String,
    /// Method arguments as a JSON object.
    #[serde(default)]
    args: serde_json::Value,
}

#[derive(Debug, serde::Deserialize, rmcp::schemars::JsonSchema)]
struct PluginArgs {
    /// Full Lua source of one plugin. Same plugin id replaces the installed version.
    source: String,
    /// false (default): only check it against the vault. true: install if the check passes.
    #[serde(default)]
    install: bool,
}

impl NodeMcp {
    /// 库调用会阻塞到引擎线程做完，放到阻塞线程池里等。
    async fn with_library<T, F>(&self, run: F) -> Result<String, String>
    where
        T: serde::Serialize + Send + 'static,
        F: FnOnce(&dyn Library) -> Result<T, LibraryError> + Send + 'static,
    {
        let library = self
            .state
            .library
            .clone()
            .ok_or_else(|| "no vault is open on this node".to_owned())?;
        let result = tokio::task::spawn_blocking(move || run(library.as_ref()))
            .await
            .map_err(|err| err.to_string())?;
        match result {
            Ok(value) => serde_json::to_string(&value).map_err(|err| err.to_string()),
            Err(err) => Err(err.message),
        }
    }
}

#[tool_router]
impl NodeMcp {
    #[tool(
        description = "Start here. Everything the vault knows: plugins, schemas (fields, components, traits, entity counts), traits (read shape, write methods), projections, current issues, and a short guide to writing plugins."
    )]
    async fn describe(&self, Parameters(NoArgs {}): Parameters<NoArgs>) -> Result<String, String> {
        self.with_library(|library| library.describe()).await
    }

    #[tool(
        description = "One entity: schema, flat fields, the value of every trait it has, and recent history."
    )]
    async fn get(
        &self,
        Parameters(GetArgs { entity }): Parameters<GetArgs>,
    ) -> Result<String, String> {
        self.with_library(move |library| library.get_entity(&entity))
            .await
    }

    #[tool(
        description = "Rows of a projection, in key order. Each row is the projection's covering value."
    )]
    async fn query(
        &self,
        Parameters(QueryArgs { projection, limit }): Parameters<QueryArgs>,
    ) -> Result<String, String> {
        let limit = limit.unwrap_or(50).clamp(1, 1000);
        self.with_library(move |library| library.rows(&projection, limit))
            .await
    }

    #[tool(
        description = "Create an entity of a schema. Returns its id. See `describe` for the schema's fields."
    )]
    async fn construct(
        &self,
        Parameters(ConstructArgs { schema, args }): Parameters<ConstructArgs>,
    ) -> Result<String, String> {
        self.with_library(move |library| {
            library
                .create_entity(&schema, args)
                .map(|id| serde_json::json!({ "id": id }))
        })
        .await
    }

    #[tool(
        description = "Change an entity through a trait's write method. The only way to edit data."
    )]
    async fn call(
        &self,
        Parameters(CallArgs {
            entity,
            trait_id,
            method,
            args,
        }): Parameters<CallArgs>,
    ) -> Result<String, String> {
        self.with_library(move |library| {
            library
                .call_entity(&entity, &trait_id, &method, args)
                .map(|()| serde_json::json!({ "ok": true }))
        })
        .await
    }

    #[tool(
        description = "Check a plugin's Lua source against the vault: does it load alongside the others, and can existing entities of this plugin still be read with it. Set install=true to install it once the check passes; projections rebuild afterwards."
    )]
    async fn plugin(
        &self,
        Parameters(PluginArgs { source, install }): Parameters<PluginArgs>,
    ) -> Result<String, String> {
        self.with_library(move |library| library.plugin(&source, install))
            .await
    }

    #[tool(description = "Node status: server name, version, data dir, uptime, open vault")]
    async fn status(&self, Parameters(NoArgs {}): Parameters<NoArgs>) -> String {
        let mut body = serde_json::json!({
            "service": "synap-node",
            "version": env!("CARGO_PKG_VERSION"),
            "server_name": self.state.server_name,
            "data_dir": self.state.data_dir,
            "uptime_ms": crate::uptime_ms(&self.state),
        });
        if let Some(vault) = &self.state.library {
            match (vault.tip(), vault.custody()) {
                (Ok(tip), Ok(custody)) => {
                    body["tip"] = serde_json::json!(tip);
                    body["custody"] = serde_json::json!(custody);
                }
                (Err(err), _) | (_, Err(err)) => {
                    body["vault_error"] = serde_json::json!(err);
                }
            }
        }
        body.to_string()
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for NodeMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
    }
}
