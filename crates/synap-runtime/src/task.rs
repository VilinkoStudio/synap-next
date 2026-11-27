//! 任务与预算。执行单位是“一条记录的一次纯函数”。

use synap_core::CanonicalValue;

use crate::fnid::FnId;

/// 一次调用的资源上限。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    /// 允许执行的 Lua 指令数。近似值，每 10k 条检查一次。
    pub instructions: u64,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            instructions: 50_000_000,
        }
    }
}

impl Budget {
    /// 小预算，测试与短任务用。
    pub const fn small() -> Self {
        Self {
            instructions: 20_000,
        }
    }
}

/// 一个可跨线程描述的任务。Lua 状态不在这里。
#[derive(Debug, Clone)]
pub struct Task {
    /// 插件 id。
    pub plugin: String,
    /// 函数身份。
    pub function: FnId,
    /// 参数表，按函数签名顺序。目前支持 0..=2 个。
    pub args: Vec<CanonicalValue>,
    /// 预算。
    pub budget: Budget,
}

/// 任务结果。
#[derive(Debug, Clone)]
pub enum TaskOutcome {
    /// 正常返回。
    Value(CanonicalValue),
    /// 预算耗尽。调用方决定重试、拆小或报错。
    Preempted,
}
