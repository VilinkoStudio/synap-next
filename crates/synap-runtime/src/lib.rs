//! synap-runtime：插件 schema 与运行时，仍在 Rust 里。
//!
//! Todo 的完成、Wiki 的版本都是 schema，不进 core。

mod runtime;
mod schema;

pub use runtime::{Action, ActionResult, Runtime};
pub use schema::{PluginSchema, SchemaError};
