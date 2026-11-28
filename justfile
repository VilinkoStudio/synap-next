# 代替旧仓 xtask：codegen 只有 flutter_rust_bridge 一条。
# 用法：just test / just fmt / just gen / just bridge-check
# Rust 工具链钉在 crates/synap-bridge/rust-toolchain.toml（native-assets 构建要求），
# 这里再 export 一份，保证 just 跑的 cargo 和 hook 编的是同一个工具链。

export RUSTUP_TOOLCHAIN := "nightly-2026-02-28"

test:
    cargo test --workspace --all-targets

fmt:
    cargo fmt --all

lint:
    cargo clippy --workspace --all-targets -- -D warnings

# FRB codegen：读 app/flutter_rust_bridge.yaml（rust_root 指向 workspace 的 bridge），
# Dart 只落到 app/lib/bridge_generated/，Rust 侧只扫 crates/synap-bridge/src/api.rs。
gen:
    cd app && flutter_rust_bridge_codegen generate

bridge-check:
    cargo check --manifest-path crates/synap-bridge/Cargo.toml

# 把 synap-node 装到 ~/.cargo/bin。需要本机 C 编译器（LuaJIT 随源码编）。
install-node:
    cargo install --path crates/synap-node --locked --force

# 用 musl 静态编译 synap-node，生成较小的 scratch 镜像。
# 没有 docker-buildx 时退回旧版 builder。
docker-node:
    #!/usr/bin/env bash
    set -euo pipefail
    if docker buildx version >/dev/null 2>&1; then
        docker build -t synap-node:dev .
    else
        DOCKER_BUILDKIT=0 docker build -t synap-node:dev .
    fi

docker-node-run *args:
    docker run --rm -p 8790:8790 -v "$PWD/data:/data" {{args}} synap-node:dev

# 用 compose 构建并前台启动，数据在 ./data。serve 会在空目录里建库。
docker-node-up *args:
    docker compose up --build {{args}}

