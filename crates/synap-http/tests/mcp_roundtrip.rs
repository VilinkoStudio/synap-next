//! MCP 端到端：列工具、调 `status`、没开库时的错误，走真 HTTP + 真 rmcp 客户端。

use std::sync::Arc;

use rmcp::{
    model::{CallToolRequestParams, ClientConfig},
    transport::{
        streamable_http_client::StreamableHttpClientTransportConfig, StreamableHttpClientTransport,
    },
    ServiceExt,
};
use synap_http::{build_router, NodeState};

#[tokio::test]
async fn mcp_status_tool_round_trip() {
    let state = Arc::new(NodeState::new(
        "mcp-test".to_string(),
        "/tmp/synap-mcp-test".to_string(),
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, build_router(state)).await;
    });

    let transport = StreamableHttpClientTransport::from_config(
        StreamableHttpClientTransportConfig::with_uri(format!("http://{addr}/mcp")),
    );
    let client = ClientConfig::default().serve(transport).await.unwrap();

    let tools = client.list_tools(None).await.unwrap();
    for name in [
        "status",
        "describe",
        "get",
        "query",
        "construct",
        "call",
        "plugin",
    ] {
        assert!(
            tools.tools.iter().any(|t| t.name == name),
            "{name} tool listed"
        );
    }

    let closed = client
        .call_tool(CallToolRequestParams::new("describe"))
        .await
        .unwrap();
    assert_eq!(
        closed.is_error,
        Some(true),
        "no vault is an error, not a crash"
    );

    let out = client
        .call_tool(CallToolRequestParams::new("status"))
        .await
        .unwrap();
    let texts: Vec<&str> = out
        .content
        .iter()
        .filter_map(|b| match b {
            rmcp::model::ContentBlock::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(texts.len(), 1);
    let v: serde_json::Value = serde_json::from_str(texts[0]).unwrap();
    assert_eq!(v["service"], "synap-node");
    assert_eq!(v["server_name"], "mcp-test");
    assert_eq!(v["data_dir"], "/tmp/synap-mcp-test");

    server.abort();
}
