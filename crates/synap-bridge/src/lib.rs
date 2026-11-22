//! synap-bridge：唯一的 FFI 薄层。
//!
//! flutter_rust_bridge 的 codegen 只扫这里。业务不写在这里，
//! 只做类型翻译 + 转发到 `synap-runtime`，投影走 Stream。

mod api;
mod frb_generated; /* AUTO INJECTED BY flutter_rust_bridge. This line may not be accurate, and you can change it according to your needs. */

pub use api::{bridge_ping, bridge_put_record, bridge_version};
