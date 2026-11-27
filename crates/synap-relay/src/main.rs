//! synap-relay 二进制：盲中继独立服务。
//!
//! 两种启动方式：`--store memory` 开箱即用；`--store redis --redis-url …`
//! 接生产后端（多实例共享同一套 Merkle 集合）。

use std::net::SocketAddr;

use clap::{ArgGroup, Parser, Subcommand, ValueEnum};
use synap_relay::{build_router, AppState, RelayAuth, RelayBackend, RelayStore};

#[derive(Debug, Parser)]
#[command(
    name = "synap-relay",
    about = "Blind sync relay: opaque frames only",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    Serve(ServeArgs),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum StoreMode {
    Memory,
    Redis,
}

#[derive(Debug, Clone, clap::Args)]
#[command(group(
    ArgGroup::new("relay_auth_mode")
        .args(["api_key", "no_key"])
        .required(true)
))]
struct ServeArgs {
    #[arg(long, env = "SYNAP_RELAY_LISTEN", default_value = "127.0.0.1:8787")]
    listen: SocketAddr,
    #[arg(long, env = "SYNAP_RELAY_SERVER_NAME", default_value = "synap-relay")]
    server_name: String,
    #[arg(long, env = "SYNAP_RELAY_API_KEY")]
    api_key: Option<String>,
    #[arg(long, env = "SYNAP_RELAY_NO_KEY", default_value_t = false)]
    no_key: bool,
    /// 存储后端：memory 开箱即用，redis 供生产多实例共享。
    #[arg(long, env = "SYNAP_RELAY_STORE", value_enum, default_value_t = StoreMode::Memory)]
    store: StoreMode,
    /// Redis 连接地址（store=redis 时必填）。
    #[arg(
        long,
        env = "SYNAP_RELAY_REDIS_URL",
        default_value = "redis://127.0.0.1:6379/"
    )]
    redis_url: String,
    /// Redis 键前缀，多库隔离靠它区分。
    #[arg(long, env = "SYNAP_RELAY_KEY_PREFIX", default_value = "synap:relay")]
    key_prefix: String,
}

#[allow(non_camel_case_types)]
mod anyhow {
    pub type Result<T, E = Box<dyn std::error::Error + Send + Sync>> = std::result::Result<T, E>;
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Serve(args) => serve(args).await,
    }
}

async fn serve(args: ServeArgs) -> anyhow::Result<()> {
    let auth = if args.no_key {
        RelayAuth::Disabled
    } else {
        RelayAuth::ApiKey(args.api_key.expect("api key or --no-key"))
    };
    let backend = match args.store {
        StoreMode::Memory => RelayBackend::memory(),
        StoreMode::Redis => RelayBackend::redis(&args.redis_url, args.key_prefix)
            .await
            .map_err(|e| format!("redis connect {}: {e}", args.redis_url))?,
    };
    // 启动即探活：redis 连不上直接退出，不带病服务。
    backend
        .health()
        .await
        .map_err(|e| format!("backend {} unhealthy: {e}", backend.backend_name()))?;
    let state = AppState::new(args.server_name, auth, backend);
    let listener = tokio::net::TcpListener::bind(args.listen).await?;
    println!("synap-relay blind mode on {}", listener.local_addr()?);
    axum::serve(listener, build_router(state))
        .with_graceful_shutdown(shutdown())
        .await?;
    Ok(())
}

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
}
