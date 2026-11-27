//! 插件元数据与解释器。
//!
//! `Plugin` 只保存源码和声明，是 `Send + Sync` 的；Lua 状态在 [`Interpreter`] 里，
//! 只属于一个线程。任务跨线程只携带 `CanonicalValue`，不携带 Lua 指针。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use mlua::{
    debug::Debug, HookTriggers, IntoLuaMulti, Lua, LuaOptions, MultiValue, RegistryKey, StdLib,
    Table, Value, VmState,
};
use synap_core::{Block, CanonicalValue};

use crate::error::PluginError;
use crate::manifest::{self, Declarations, Parsed};
use crate::task::Budget;

/// 本运行时实现的宿主 API 版本。
pub const HOST_API: u32 = 1;

/// 标准库插件的 id，唯一可以声明 `std/` 名字的插件。
pub const STD_PLUGIN: &str = "std";

/// 沙箱限制。指令数由每次调用的 [`Budget`] 覆盖。
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// 默认指令数。
    pub instructions: u64,
    /// Lua 堆的上限。
    pub memory_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            instructions: Budget::default().instructions,
            memory_bytes: 64 * 1024 * 1024,
        }
    }
}

const INSTRUCTION_STEP: u32 = 10_000;

struct Sandbox {
    lua: Lua,
    env: Table,
    ticks: Arc<AtomicU64>,
    limit_ticks: Arc<AtomicU64>,
    preempted: Arc<AtomicBool>,
}

impl Sandbox {
    fn new(limits: Limits) -> Result<Self, PluginError> {
        let libs = StdLib::TABLE | StdLib::STRING | StdLib::MATH | StdLib::BIT;
        let lua = Lua::new_with(libs, LuaOptions::new())?;

        let limit_ticks = Arc::new(AtomicU64::new(ticks_for(limits.instructions)));
        let ticks = Arc::new(AtomicU64::new(0));
        let preempted = Arc::new(AtomicBool::new(false));
        {
            let ticks = ticks.clone();
            let limit_ticks = limit_ticks.clone();
            let preempted = preempted.clone();
            lua.set_hook(
                HookTriggers::new().every_nth_instruction(INSTRUCTION_STEP),
                move |_, _: &Debug| {
                    let count = ticks.fetch_add(1, Ordering::Relaxed) + 1;
                    if count > limit_ticks.load(Ordering::Relaxed) {
                        preempted.store(true, Ordering::Relaxed);
                        return Err(mlua::Error::RuntimeError(
                            "instruction limit exceeded".to_owned(),
                        ));
                    }
                    Ok(VmState::Continue)
                },
            )?;
        }
        lua.set_memory_limit(limits.memory_bytes)?;

        {
            let globals = lua.globals();
            for name in [
                "os",
                "io",
                "package",
                "debug",
                "jit",
                "load",
                "loadstring",
                "loadfile",
                "dofile",
                "require",
                "module",
                "print",
                "collectgarbage",
                "rawset",
                "setfenv",
                "getfenv",
                // LuaJIT 的基础库自带 coroutine；指令钩子只挂在主线程上，协程里的死循环不受限。
                "coroutine",
                "gcinfo",
                "newproxy",
            ] {
                globals.raw_set(name, Value::Nil)?;
            }
            if let Ok(math) = globals.get::<mlua::Table>("math") {
                math.raw_set("random", Value::Nil)?;
                math.raw_set("randomseed", Value::Nil)?;
            }
            if let Ok(table) = globals.get::<mlua::Table>("table") {
                // 按哈希顺序遍历，跨进程不确定。
                table.raw_set("foreach", Value::Nil)?;
                table.raw_set("foreachi", Value::Nil)?;
            }
            if let Ok(string) = globals.get::<mlua::Table>("string") {
                // 字节码可以绕过文本加载的检查。
                string.raw_set("dump", Value::Nil)?;
            }
            // 预算一旦耗尽，pcall / xpcall 不能把它吞掉：返回后立即重新抛出。
            let exhausted = {
                let preempted = preempted.clone();
                lua.create_function(move |_, ()| Ok(preempted.load(Ordering::Relaxed)))?
            };
            globals.raw_set("__synap_exhausted", exhausted)?;
            lua.load(
                r#"
local raw_next = next
local exhausted = __synap_exhausted
local raw_pcall, raw_xpcall, raw_error, raw_tostring = pcall, xpcall, error, tostring
local function guard(...)
  if exhausted() then raw_error("instruction limit exceeded", 0) end
  return ...
end
function pcall(...) return guard(raw_pcall(...)) end
function xpcall(...) return guard(raw_xpcall(...)) end
function tostring(value)
  local kind = type(value)
  -- table / function 的默认 tostring 带内存地址，结果不确定。
  if kind == "table" or kind == "function" or kind == "userdata" or kind == "thread" then
    return kind
  end
  return raw_tostring(value)
end
local function rank(key)
  local kind = type(key)
  if kind == "number" then return 1 end
  if kind == "string" then return 2 end
  if kind == "boolean" then return 3 end
  error("pairs: unsupported key type " .. kind, 3)
end
local function less(left, right)
  local left_rank = rank(left)
  local right_rank = rank(right)
  if left_rank ~= right_rank then return left_rank < right_rank end
  return left < right
end
function pairs(t)
  local keys = {}
  for key in raw_next, t do
    keys[#keys + 1] = key
  end
  table.sort(keys, less)
  local index = 0
  return function()
    index = index + 1
    local key = keys[index]
    if key == nil then return nil end
    return key, t[key]
  end
end
local pipeline = {}
pipeline.__index = pipeline
function pipeline:filter(fn) self.filter = fn; return self end
function pipeline:map(fn) self.map = fn; return self end
function pipeline:index() self.indexed = true; return self end
function from(trait) return setmetatable({ trait = trait }, pipeline) end
"#,
            )
            .set_name("sandbox")
            .exec()?;
            globals.raw_set("next", Value::Nil)?;
            globals.raw_set("__synap_exhausted", Value::Nil)?;
        }

        freeze_std(&lua)?;

        // 插件 chunk 的环境：读真实全局，写未知全局报错。
        let env = lua.create_table()?;
        let metatable = lua.create_table()?;
        metatable.raw_set("__index", lua.globals())?;
        // 锁住元表，不让 getmetatable(_G) 拿到真实全局表。
        metatable.raw_set("__metatable", false)?;
        metatable.raw_set(
            "__newindex",
            lua.create_function(
                |_, (_, key, _): (Table, Value, Value)| -> mlua::Result<()> {
                    let name = match key {
                        Value::String(text) => text
                            .to_str()
                            .map(|value| value.to_owned())
                            .unwrap_or_default(),
                        other => other.type_name().to_owned(),
                    };
                    Err(mlua::Error::RuntimeError(format!(
                        "plugin cannot assign global `{name}`"
                    )))
                },
            )?,
        )?;
        env.set_metatable(Some(metatable))?;
        env.raw_set("_G", env.clone())?;

        Ok(Self {
            lua,
            env,
            ticks,
            limit_ticks,
            preempted,
        })
    }

    fn begin(&self, budget: Budget) {
        self.ticks.store(0, Ordering::Relaxed);
        self.preempted.store(false, Ordering::Relaxed);
        self.limit_ticks
            .store(ticks_for(budget.instructions), Ordering::Relaxed);
    }

    fn lua(&self) -> &Lua {
        &self.lua
    }

    fn environment(&self) -> Table {
        self.env.clone()
    }

    fn preempted(&self) -> bool {
        self.preempted.load(Ordering::Relaxed)
    }
}

fn ticks_for(instructions: u64) -> u64 {
    instructions / u64::from(INSTRUCTION_STEP) + 1
}

/// 标准库表只读。插件之间复制解释器时，不能让某个副本改掉 `string`/`table`。
fn freeze_std(lua: &Lua) -> mlua::Result<()> {
    let deny = lua.create_function(|_, _: (Table, Value, Value)| -> mlua::Result<()> {
        Err(mlua::Error::RuntimeError(
            "sandbox table is read-only".to_owned(),
        ))
    })?;
    for name in ["table", "string", "math", "bit"] {
        if let Ok(table) = lua.globals().get::<Table>(name) {
            let metatable = lua.create_table()?;
            metatable.raw_set("__newindex", deny.clone())?;
            metatable.raw_set("__metatable", false)?;
            table.set_metatable(Some(metatable))?;
        }
    }
    // 字符串共享同一个元表；锁住它，插件不能替换字符串方法。
    lua.load(r#"getmetatable("").__metatable = false"#)
        .set_name("sandbox")
        .exec()?;
    Ok(())
}

/// 解释器调用的结果。
pub(crate) enum Invoked {
    Value(Value),
    Preempted,
}

/// 一个插件源码的元数据。没有 Lua 状态。
pub struct Plugin {
    id: String,
    api: u32,
    source: Vec<u8>,
    declarations: Declarations,
}

impl std::fmt::Debug for Plugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Plugin")
            .field("id", &self.id)
            .field("api", &self.api)
            .field("traits", &self.declarations.traits.len())
            .field("components", &self.declarations.components.len())
            .field("schemas", &self.declarations.schemas.len())
            .field("impls", &self.declarations.impls.len())
            .field("projections", &self.declarations.projections.len())
            .finish_non_exhaustive()
    }
}

impl Plugin {
    /// 解析插件源码，得到可跨线程的元数据。函数体不执行。
    pub fn load(source: &[u8]) -> Result<Self, PluginError> {
        let text = std::str::from_utf8(source).map_err(|_| PluginError::SourceNotUtf8)?;
        let sandbox = Sandbox::new(Limits::default())?;
        let parsed: Parsed = manifest::collect(sandbox.lua(), sandbox.environment(), text)?;
        let Some((id, api)) = parsed.declarations.plugin.clone() else {
            return Err(PluginError::MissingPlugin);
        };
        if api != HOST_API {
            return Err(PluginError::HostApi {
                plugin: id,
                found: api,
                host: HOST_API,
            });
        }
        let plugin = Self {
            id,
            api,
            source: source.to_vec(),
            declarations: parsed.declarations,
        };
        plugin.validate_local()?;
        Ok(plugin)
    }

    /// 从创世约定的插件块解析。
    pub fn from_block(block: &Block) -> Result<Self, PluginError> {
        if block.header.schema_id.as_str() != "sys/plugin" || !block.header.schema_version.is_zero()
        {
            return Err(PluginError::manifest(
                "plugin block must use schema `sys/plugin` and schema_version = 0",
            ));
        }
        Self::load(&block.payload)
    }

    /// 插件 id。
    pub fn id(&self) -> &str {
        &self.id
    }

    /// 插件声明的宿主 API。
    pub const fn api(&self) -> u32 {
        self.api
    }

    /// 原始源码。
    pub fn source(&self) -> &[u8] {
        &self.source
    }

    pub(crate) fn declarations(&self) -> &Declarations {
        &self.declarations
    }

    fn validate_local(&self) -> Result<(), PluginError> {
        // `std/` 只属于标准库插件，`sys/` 只属于内核。其余命名空间先到先得，
        // 重名由加载时的隔离处理（后来的插件被停用，不会让库打不开）。
        let names = self
            .declarations
            .traits
            .iter()
            .map(|item| item.id.name().to_owned())
            .chain(
                self.declarations
                    .components
                    .iter()
                    .map(|item| item.id.name.to_string()),
            )
            .chain(
                self.declarations
                    .schemas
                    .iter()
                    .map(|item| item.id.name.to_string()),
            )
            .chain(
                self.declarations
                    .projections
                    .iter()
                    .map(|item| item.name.clone()),
            );
        for name in names {
            let namespace = name.split('/').next().unwrap_or_default();
            let reserved = match namespace {
                "sys" => true,
                "std" => self.id != STD_PLUGIN,
                _ => false,
            };
            if reserved {
                return Err(PluginError::manifest(format!(
                    "plugin `{}` cannot declare `{name}`: the `{namespace}/` namespace is reserved",
                    self.id
                )));
            }
        }
        let mut traits = std::collections::BTreeSet::new();
        for item in &self.declarations.traits {
            if !traits.insert(item.id.clone()) {
                return Err(duplicate(&self.id, "trait", item.id.to_string()));
            }
        }
        let mut components = std::collections::BTreeSet::new();
        for item in &self.declarations.components {
            if !components.insert(item.id.clone()) {
                return Err(duplicate(&self.id, "component", item.id.to_string()));
            }
        }
        let mut schemas = std::collections::BTreeSet::new();
        for item in &self.declarations.schemas {
            if !schemas.insert(item.id.clone()) {
                return Err(duplicate(&self.id, "schema", item.id.to_string()));
            }
        }
        let mut impls = std::collections::BTreeSet::new();
        for item in &self.declarations.impls {
            if !impls.insert((item.trait_id.clone(), item.schema.clone())) {
                return Err(duplicate(
                    &self.id,
                    "impl",
                    format!("{} for {}", item.trait_id, item.schema),
                ));
            }
        }
        let mut projections = std::collections::BTreeSet::new();
        for item in &self.declarations.projections {
            if !projections.insert(item.name.clone()) {
                return Err(duplicate(&self.id, "projection", item.name.clone()));
            }
        }
        Ok(())
    }
}

fn duplicate(plugin: &str, kind: &'static str, name: String) -> PluginError {
    PluginError::Duplicate {
        plugin: plugin.to_owned(),
        kind,
        name,
    }
}

/// 一个插件源码在某一个线程里的 Lua 状态。
pub struct Interpreter {
    plugin_id: String,
    sandbox: Sandbox,
    functions: std::collections::BTreeMap<crate::fnid::FnId, RegistryKey>,
}

impl std::fmt::Debug for Interpreter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Interpreter")
            .field("plugin_id", &self.plugin_id)
            .field("functions", &self.functions.len())
            .finish_non_exhaustive()
    }
}

impl Interpreter {
    /// 在一个线程里加载插件。必须在该线程创建，不允许把 Lua 搬过来。
    pub fn load(plugin: &Plugin) -> Result<Self, PluginError> {
        let text = std::str::from_utf8(&plugin.source).map_err(|_| PluginError::SourceNotUtf8)?;
        let sandbox = Sandbox::new(Limits::default())?;
        let parsed = manifest::collect(sandbox.lua(), sandbox.environment(), text)?;
        if parsed
            .declarations
            .plugin
            .as_ref()
            .map(|(id, _)| id.as_str())
            != Some(plugin.id())
        {
            return Err(PluginError::manifest(
                "plugin source changed between parses",
            ));
        }
        Ok(Self {
            plugin_id: plugin.id().to_owned(),
            sandbox,
            functions: parsed.functions,
        })
    }

    /// 插件 id。
    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    pub(crate) fn lua(&self) -> &Lua {
        self.sandbox.lua()
    }

    pub(crate) fn invoke(
        &self,
        function: &crate::fnid::FnId,
        args: impl IntoLuaMulti,
        budget: Budget,
    ) -> Result<Invoked, PluginError> {
        let key = self.functions.get(function).ok_or_else(|| {
            PluginError::manifest(format!(
                "plugin `{}` has no function {function:?}",
                self.plugin_id
            ))
        })?;
        self.sandbox.begin(budget);
        let lua = self.sandbox.lua();
        let function = lua.registry_value::<mlua::Function>(key)?;
        match function.call::<Value>(args) {
            Ok(value) => Ok(Invoked::Value(value)),
            Err(error) => {
                if self.sandbox.preempted() {
                    Ok(Invoked::Preempted)
                } else {
                    Err(PluginError::Lua(error))
                }
            }
        }
    }

    pub(crate) fn invoke_multi(
        &self,
        function: &crate::fnid::FnId,
        args: impl IntoLuaMulti,
        budget: Budget,
    ) -> Result<MultiValue, PluginError> {
        let key = self.functions.get(function).ok_or_else(|| {
            PluginError::manifest(format!(
                "plugin `{}` has no function {function:?}",
                self.plugin_id
            ))
        })?;
        self.sandbox.begin(budget);
        let lua = self.sandbox.lua();
        let function = lua.registry_value::<mlua::Function>(key)?;
        match function.call::<MultiValue>(args) {
            Ok(values) => Ok(values),
            Err(error) => {
                if self.sandbox.preempted() {
                    Err(PluginError::Preempted)
                } else {
                    Err(PluginError::Lua(error))
                }
            }
        }
    }
}

/// 把 `CanonicalValue` 的映射推成 Lua 表，供调用方构造参数。
pub(crate) fn push_args(lua: &Lua, args: &CanonicalValue) -> Result<Table, PluginError> {
    match crate::lua_value::push(lua, args)? {
        Value::Table(table) => Ok(table),
        Value::Nil => Ok(lua.create_table()?),
        _ => Err(PluginError::manifest("arguments must be a map")),
    }
}
