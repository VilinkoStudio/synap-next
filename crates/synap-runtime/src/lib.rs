//! synap-runtime：类型系统与 LuaJIT 解释层。
//!
//! 这一层把插件源码里的 `trait / component / schema / impl` 声明变成 Rust 侧
//! 的实现表，并在 LuaJIT 沙箱里执行作者写的纯函数：`construct`、`extract`
//! 与 Trait 写方法。payload 的分区编码、合并、投影与 Block 写入留给后续层。
//!
//! 每个插件一个 Lua 状态；跨插件调用只经过 [`synap_core::CanonicalValue`]。

mod describe;
mod engine;
mod error;
mod fnid;
mod lua_value;
mod manifest;
mod plugin;
mod pool;
mod registry;
mod task;
mod text;
mod types;

pub use describe::AUTHORING_GUIDE;
pub use engine::{Engine, EngineError, Issue};
pub use error::PluginError;
pub use fnid::FnId;
pub use manifest::{
    ComponentDef, ComponentRef, Declarations, ImplDef, MethodDef, ProjectionDef, SchemaDef,
    TraitDef,
};
pub use plugin::{Interpreter, Limits, Plugin, HOST_API};
pub use pool::{Pool, PoolError};
pub use registry::{Definitions, ProjectionMeta, Registry, SELF_PARTITION};
pub use task::{Budget, Task, TaskOutcome};
pub use text::{ExportReport, ImportOptions, ImportReport};
pub use types::{IntType, TypeSpec, VersionedName};

/// 随运行时发行的标准库源码。它和第三方插件走同一条加载路径。
pub const STDLIB: &str = include_str!("../stdlib/std.lua");
