use thiserror::Error;

use synap_core::CoreError;

/// 插件加载与执行错误。消息里尽量带插件、Schema、Trait 和字段路径，
/// 让 AI 与作者能直接定位，而不是只看到一句 "lua error"。
#[derive(Debug, Error)]
pub enum PluginError {
    /// 插件源码不是 UTF-8。
    #[error("plugin source is not UTF-8")]
    SourceNotUtf8,
    /// Lua 语法或运行时错误。
    #[error("lua: {0}")]
    Lua(#[from] mlua::Error),
    /// 值转换或规范编码失败。
    #[error("value: {0}")]
    Value(#[from] CoreError),
    /// 声明形状不对。
    #[error("manifest: {0}")]
    Manifest(String),
    /// 插件没有 `plugin` 块。
    #[error("plugin declares no `plugin` block")]
    MissingPlugin,
    /// 插件声明了多个 `plugin` 块。
    #[error("plugin declares more than one `plugin` block")]
    DuplicatePlugin,
    /// 宿主 API 版本不匹配。
    #[error("plugin `{plugin}` targets host api {found}, this runtime is api {host}")]
    HostApi {
        /// 插件声明的版本。
        found: u32,
        /// 本运行时的版本。
        host: u32,
        /// 插件 id。
        plugin: String,
    },
    /// 单个插件内出现重复声明。
    #[error("plugin `{plugin}` declares {kind} `{name}` twice")]
    Duplicate {
        /// 插件 id。
        plugin: String,
        /// 声明种类。
        kind: &'static str,
        /// 声明名字。
        name: String,
    },
    /// 引用了一个不存在的组件。
    #[error("schema `{schema}` composes unknown component `{component}`")]
    UnknownComponent {
        /// Schema。
        schema: String,
        /// 组件。
        component: String,
    },
    /// 引用了一个不存在的 Schema。
    #[error("no loaded schema `{0}`")]
    UnknownSchema(String),
    /// 引用了一个不存在的 Trait。
    #[error("no loaded trait `{0}`")]
    UnknownTrait(String),
    /// 同一对 `(Trait, Schema)` 有多个显式 Impl，且没有归属优先者。
    #[error("trait `{trait_id}` has more than one impl for schema `{schema}`")]
    AmbiguousImpl {
        /// Trait。
        trait_id: String,
        /// Schema。
        schema: String,
    },
    /// 某个能力没有实现，也没有可委托的组件。
    #[error("schema `{schema}` implements no `{trait_id}`")]
    MissingImpl {
        /// Schema。
        schema: String,
        /// Trait。
        trait_id: String,
    },
    /// 能力有几个组件都能提供，但没有 `delegate` 消歧。
    #[error(
        "schema `{schema}` has several components for `{trait_id}` ({components}); add delegate"
    )]
    AmbiguousDelegate {
        /// Schema。
        schema: String,
        /// Trait。
        trait_id: String,
        /// 候选组件别名。
        components: String,
    },
    /// 两个组件或自有字段占用了同一个字段名。
    #[error("schema `{schema}` maps field `{field}` twice (`{left}` and `{right}`)")]
    DuplicateField {
        /// Schema。
        schema: String,
        /// 字段。
        field: String,
        /// 先声明者。
        left: String,
        /// 后声明者。
        right: String,
    },
    /// 平坦记录里出现未知字段。
    #[error("schema `{schema}` has no field `{field}`")]
    UnknownField {
        /// Schema。
        schema: String,
        /// 字段。
        field: String,
    },
    /// 必需字段没给。
    #[error("schema `{schema}` field `{field}` is missing")]
    MissingField {
        /// Schema。
        schema: String,
        /// 字段。
        field: String,
    },
    /// 可选组件当前不存在。
    #[error("schema `{schema}` has no component `{component}`")]
    ComponentAbsent {
        /// Schema。
        schema: String,
        /// 组件别名。
        component: String,
    },
    /// 必需组件在 payload 里没有分区。
    #[error("schema `{schema}` is missing required component partition `{component}`")]
    MissingComponent {
        /// Schema。
        schema: String,
        /// 组件别名。
        component: String,
    },
    /// 写入方法不存在。
    #[error("trait `{trait_id}` has no write method `{method}`")]
    UnknownMethod {
        /// Trait。
        trait_id: String,
        /// 方法。
        method: String,
    },
    /// 声明了本层尚未实现的机制。明确拒绝，不悄悄忽略。
    #[error("`{feature}` in plugin `{plugin}` is not implemented by this runtime layer yet")]
    Unsupported {
        /// 插件 id。
        plugin: String,
        /// 机制名。
        feature: &'static str,
    },
    /// 写过一次后会拒绝（例如构造时要求可选组件）。
    #[error("lua returned a non-table value where `{where_}` needs a table")]
    NotATable {
        /// 出错位置。
        where_: String,
    },
    /// 任务预算耗尽。纯函数没有副作用，调用方可以重试或拆小。
    #[error("plugin task was preempted")]
    Preempted,
}

impl PluginError {
    pub(crate) fn manifest(message: impl Into<String>) -> Self {
        Self::Manifest(message.into())
    }
}
