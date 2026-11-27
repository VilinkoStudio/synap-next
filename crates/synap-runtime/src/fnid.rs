//! 稳定的函数身份。`Definitions` 只持它，不持 Lua 句柄。

/// 插件里一个纯函数的位置。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FnId {
    /// 组件的 `construct`。
    ComponentConstruct {
        /// 组件在插件声明里的下标。
        component: usize,
    },
    /// 组件的 `upgrade`。
    ComponentUpgrade {
        /// 组件在插件声明里的下标。
        component: usize,
    },
    /// Schema 的 `construct`。
    SchemaConstruct {
        /// Schema 在插件声明里的下标。
        schema: usize,
    },
    /// Schema 的私有 action。
    SchemaAction {
        /// Schema 在插件声明里的下标。
        schema: usize,
        /// action 名。
        action: String,
    },
    /// Schema 的 `upgrade`。
    SchemaUpgrade {
        /// Schema 在插件声明里的下标。
        schema: usize,
    },
    /// Impl 的 `extract`。
    ImplExtract {
        /// Impl 在插件声明里的下标。
        imp: usize,
    },
    /// Impl 的写方法。
    ImplMethod {
        /// Impl 在插件声明里的下标。
        imp: usize,
        /// 方法名。
        method: String,
    },
    /// 投影的 `filter`。
    ProjectionFilter {
        /// 投影在插件声明里的下标。
        projection: usize,
    },
    /// 投影的 `map`。
    ProjectionMap {
        /// 投影在插件声明里的下标。
        projection: usize,
    },
}
