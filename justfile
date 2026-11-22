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
    cargo check -p synap-bridge
