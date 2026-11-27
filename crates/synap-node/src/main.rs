//! synap-node：库住在 `serve` 里。命令行只跟这个进程说话。
//!
//! ```text
//! synap-node init   --data-dir <dir>
//! synap-node serve  --data-dir <dir>
//! synap-node create --schema <name@major> --args <json>
//! synap-node call   <id> --trait <name@major> --method <name> --args <json>
//! synap-node rows   --projection <name>
//! synap-node status
//! synap-node version
//! synap-node export --data-dir <dir> --out <dir>      # 节点停着时用
//! synap-node import --data-dir <dir> --from <dir> [--dry-run] [--delete-missing]
//! ```
//!
//! `synap-node --version` 仍是本机这个二进制的版本，不访问服务器。

mod client;
mod config;
mod library;
mod vault;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use synap_http::{build_router, NodeState};

#[derive(Debug, Parser)]
#[command(name = "synap-node", about = "Headless Synap node", version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// 在本机建库。节点还没起来时用，建完就关闭文件。
    Init(InitArgs),
    /// 打开库并听着。没有库时顺手建一个。
    Serve(ServeArgs),
    /// 向正在跑的节点要状态。
    Status(RemoteArgs),
    /// 向正在跑的节点要版本。
    Version(RemoteArgs),
    /// 让节点新建一条实体。
    Create(CreateArgs),
    /// 让节点调用一个 Trait 写方法。
    Call(CallArgs),
    /// 向节点要一页投影行。
    Rows(RowsArgs),
    /// 把库导出成明文目录：一个实体一个 Markdown 文件，加上插件源码。节点停着时用。
    Export(ExportArgs),
    /// 从明文目录按差异导入：没变的不写，改了的接新版本。节点停着时用。
    Import(ImportArgs),
}

#[derive(Debug, Clone, clap::Args)]
struct ExportArgs {
    #[arg(long, env = "SYNAP_NODE_DATA_DIR")]
    data_dir: PathBuf,
    /// 导出目录。
    #[arg(long)]
    out: PathBuf,
}

#[derive(Debug, Clone, clap::Args)]
struct ImportArgs {
    #[arg(long, env = "SYNAP_NODE_DATA_DIR")]
    data_dir: PathBuf,
    /// 之前导出（可能被改过）的目录。
    #[arg(long)]
    from: PathBuf,
    /// 只报告会发生什么，不写入。
    #[arg(long)]
    dry_run: bool,
    /// 目录里没有的实体写删除标记。默认关闭。
    #[arg(long)]
    delete_missing: bool,
}

#[derive(Debug, Clone, clap::Args)]
struct InitArgs {
    #[arg(long, env = "SYNAP_NODE_DATA_DIR")]
    data_dir: PathBuf,
    #[arg(long, env = "SYNAP_NODE_SERVER_NAME", default_value = "synap-node")]
    server_name: String,
}

#[derive(Debug, Clone, clap::Args)]
struct ServeArgs {
    #[arg(long, env = "SYNAP_NODE_DATA_DIR")]
    data_dir: PathBuf,
    #[arg(long, env = "SYNAP_NODE_SERVER_NAME", default_value = "synap-node")]
    server_name: String,
    #[arg(long, env = "SYNAP_NODE_LISTEN", default_value = "127.0.0.1:8790")]
    listen: SocketAddr,
}

#[derive(Debug, Clone, clap::Args)]
struct CreateArgs {
    #[clap(flatten)]
    remote: RemoteArgs,
    /// `namespace/name@major`。
    #[arg(long)]
    schema: String,
    /// JSON 对象。省略时是 `{}`。
    #[arg(long, default_value = "{}")]
    args: String,
}

#[derive(Debug, Clone, clap::Args)]
struct CallArgs {
    #[clap(flatten)]
    remote: RemoteArgs,
    /// 实体 id。
    id: String,
    /// `namespace/name@major`。
    #[arg(long = "trait")]
    trait_name: String,
    #[arg(long)]
    method: String,
    #[arg(long, default_value = "{}")]
    args: String,
}

#[derive(Debug, Clone, clap::Args)]
struct RowsArgs {
    #[clap(flatten)]
    remote: RemoteArgs,
    #[arg(long)]
    projection: String,
    #[arg(long, default_value_t = 50)]
    limit: usize,
}

#[derive(Debug, Clone, clap::Args)]
struct RemoteArgs {
    /// 正在跑的节点，例如 `http://127.0.0.1:8790`。
    #[arg(long, env = "SYNAP_NODE_URL", default_value = "http://127.0.0.1:8790")]
    url: String,
}

#[allow(non_camel_case_types)]
mod anyhow {
    pub type Result<T, E = Box<dyn std::error::Error + Send + Sync>> = std::result::Result<T, E>;
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Commands::Init(args) => {
            let config = vault::create(&args.data_dir, &args.server_name)?;
            println!(
                "initialized {} ({})",
                args.data_dir.display(),
                config.server_name
            );
            Ok(())
        }
        Commands::Serve(args) => serve(args).await,
        Commands::Status(args) => {
            let body = client::get_json(&args.url, "/v1/status")?;
            print_status(&body);
            Ok(())
        }
        Commands::Version(args) => {
            let body = client::get_json(&args.url, "/version")?;
            let version = body["version"].as_str().unwrap_or("unknown");
            let name = body["server_name"].as_str().unwrap_or("synap-node");
            println!("synap-node {version} ({name})");
            Ok(())
        }
        Commands::Create(args) => {
            let payload = serde_json::json!({
                "schema": args.schema,
                "args": parse_args(&args.args)?,
            });
            let body = client::send_json(&args.remote.url, "POST", "/v1/entities", Some(&payload))?;
            println!("{}", body["id"].as_str().unwrap_or(""));
            Ok(())
        }
        Commands::Call(args) => {
            let payload = serde_json::json!({
                "trait": args.trait_name,
                "method": args.method,
                "args": parse_args(&args.args)?,
            });
            let path = format!("/v1/entities/{}/calls", args.id);
            let body = client::send_json(&args.remote.url, "POST", &path, Some(&payload))?;
            println!("{}", body["id"].as_str().unwrap_or(""));
            Ok(())
        }
        Commands::Rows(args) => {
            let path = format!(
                "/v1/rows?projection={}&limit={}",
                encode_query(&args.projection),
                args.limit
            );
            let body = client::get_json(&args.remote.url, &path)?;
            println!("{}", serde_json::to_string_pretty(&body["rows"])?);
            Ok(())
        }
        Commands::Export(args) => {
            let out = args.out.clone();
            let report = library::with_engine(&args.data_dir, move |engine| {
                engine.export_text(&out).map_err(|err| err.to_string())
            })?;
            println!(
                "exported {} entities and {} plugins to {}",
                report.entities,
                report.plugins,
                args.out.display()
            );
            for (entity, reason) in report.skipped {
                eprintln!("skipped {entity}: {reason}");
            }
            Ok(())
        }
        Commands::Import(args) => {
            let from = args.from.clone();
            let options = synap_runtime::ImportOptions {
                delete_missing: args.delete_missing,
                dry_run: args.dry_run,
            };
            let report = library::with_engine(&args.data_dir, move |engine| {
                engine
                    .import_text(&from, options)
                    .map_err(|err| err.to_string())
            })?;
            let verb = if args.dry_run { "would write" } else { "wrote" };
            println!(
                "{verb}: {} updated, {} created, {} deleted; {} unchanged",
                report.updated, report.created, report.deleted, report.unchanged
            );
            for (path, reason) in &report.errors {
                eprintln!("error {}: {reason}", path.display());
            }
            if !report.errors.is_empty() {
                return Err(format!("{} files could not be imported", report.errors.len()).into());
            }
            Ok(())
        }
    }
}

fn parse_args(text: &str) -> anyhow::Result<serde_json::Value> {
    let value: serde_json::Value = serde_json::from_str(text).map_err(|err| err.to_string())?;
    if !value.is_object() {
        return Err("args must be a JSON object".into());
    }
    Ok(value)
}

fn encode_query(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

async fn serve(args: ServeArgs) -> anyhow::Result<()> {
    let session = library::open_session(&args.data_dir, &args.server_name)?;
    let data_dir = args
        .data_dir
        .to_str()
        .ok_or("data-dir is not valid UTF-8")?
        .to_owned();
    let state =
        Arc::new(NodeState::new(session.server_name, data_dir).with_library(session.library));
    let listener = tokio::net::TcpListener::bind(args.listen)
        .await
        .map_err(|err| err.to_string())?;
    let local = listener.local_addr().map_err(|err| err.to_string())?;
    if session.created {
        println!("created vault {}", args.data_dir.display());
    } else {
        println!("opened vault {}", args.data_dir.display());
    }
    println!("synap-node on {local}");
    axum::serve(listener, build_router(state))
        .with_graceful_shutdown(shutdown())
        .await
        .map_err(|err| err.to_string())?;
    Ok(())
}

fn print_status(body: &serde_json::Value) {
    println!("server: {}", body["server_name"].as_str().unwrap_or(""));
    println!("data: {}", body["data_dir"].as_str().unwrap_or(""));
    if let Some(tip) = body["tip"].as_u64() {
        println!("tip: {tip}");
    }
    if let Some(custody) = body["custody"].as_str() {
        println!("custody: {custody}");
    }
    if let Some(uptime) = body["uptime_ms"].as_u64() {
        println!("uptime_ms: {uptime}");
    }
}

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn status_comes_from_the_running_node() {
        let dir = std::env::temp_dir().join(format!(
            "synap-node-serve-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let session = library::open_session(&dir, "n1").unwrap();
        let data_dir = dir.to_str().unwrap().to_owned();
        let state =
            Arc::new(NodeState::new(session.server_name, data_dir).with_library(session.library));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, build_router(state)).await;
        });

        let url = format!("http://{addr}");
        let mut body = None;
        for _ in 0..50 {
            let probe = url.clone();
            match tokio::task::spawn_blocking(move || client::get_json(&probe, "/v1/status")).await
            {
                Ok(Ok(value)) => {
                    body = Some(value);
                    break;
                }
                _ => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        }
        let body = body.expect("status");
        assert_eq!(body["server_name"], "n1");
        // 标准库作为插件块写入，空库打开后 tip 是 1。
        assert_eq!(body["tip"], 1);
        assert_eq!(body["custody"], "embedded");
        let version_url = url.clone();
        let version =
            tokio::task::spawn_blocking(move || client::get_json(&version_url, "/version"))
                .await
                .unwrap()
                .unwrap();
        assert_eq!(version["server_name"], "n1");
        let _ = std::fs::remove_dir_all(&dir);
    }

    const TODO: &str = r#"
plugin "vilinko.todo" { api = 1 }

schema "vilinko.todo/item" {
  compose = {
    title = "std/Title@1",
    check = "std/Check@1",
    due   = "std/Time@1?",
    note  = "std/Text@1?",
  },
  fields = { priority = "u8", status = "text" },
  merge = { priority = "lww", status = "lww" },
  construct = function(args, base)
    base.priority = args.priority or 0
    base.status = args.status or "open"
    return base
  end,
}

impl("std/Checkable@1", "vilinko.todo/item@1", {
  extract = function(r) return { done = r.status == "done" } end,
  set_done = function(r, a) r.status = a.done and "done" or "open"; return r end,
})

projection "vilinko.todo/open_by_due" {
  from("std/Checkable@1")
    :filter(function(t) return not t.Checkable.done end)
    :map(function(t)
      local due = t.Temporal and t.Temporal.due
      local title = t.Titled and t.Titled.title or ""
      return { due and due.physical_ms or 0, title },
             { id = t.id, title = title, due = due and due.physical_ms or nil }
    end)
    :index()
}
"#;

    #[tokio::test]
    async fn create_call_and_rows_go_through_the_node() {
        let dir = std::env::temp_dir().join(format!(
            "synap-node-rows-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("plugins")).unwrap();
        std::fs::write(dir.join("plugins/todo.lua"), TODO).unwrap();
        let session = library::open_session(&dir, "n1").unwrap();
        let data_dir = dir.to_str().unwrap().to_owned();
        let state =
            Arc::new(NodeState::new(session.server_name, data_dir).with_library(session.library));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, build_router(state)).await;
        });
        let url = format!("http://{addr}");

        let create_url = url.clone();
        let created = tokio::task::spawn_blocking(move || {
            let payload = serde_json::json!({
                "schema": "vilinko.todo/item@1",
                "args": { "title": "写投影", "status": "open", "priority": 1 }
            });
            client::send_json(&create_url, "POST", "/v1/entities", Some(&payload))
        })
        .await
        .unwrap()
        .unwrap();
        let id = created["id"].as_str().unwrap().to_owned();

        let rows_url = url.clone();
        let page = tokio::task::spawn_blocking(move || {
            client::get_json(
                &rows_url,
                "/v1/rows?projection=vilinko.todo/open_by_due&limit=50",
            )
        })
        .await
        .unwrap()
        .unwrap();
        assert_eq!(page["rows"].as_array().unwrap().len(), 1);
        assert_eq!(page["rows"][0]["title"], "写投影");

        let call_url = url.clone();
        let call_id = id.clone();
        tokio::task::spawn_blocking(move || {
            let payload = serde_json::json!({
                "trait": "std/Checkable@1",
                "method": "set_done",
                "args": { "done": true }
            });
            client::send_json(
                &call_url,
                "POST",
                &format!("/v1/entities/{call_id}/calls"),
                Some(&payload),
            )
        })
        .await
        .unwrap()
        .unwrap();

        let after_url = url.clone();
        let after = tokio::task::spawn_blocking(move || {
            client::get_json(
                &after_url,
                "/v1/rows?projection=vilinko.todo/open_by_due&limit=50",
            )
        })
        .await
        .unwrap()
        .unwrap();
        assert_eq!(after["rows"].as_array().unwrap().len(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
