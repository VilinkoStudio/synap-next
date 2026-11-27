//! 插件清单：Lua 源码里的声明。
//!
//! 语法是真正的 Lua，不是伪代码：
//!
//! ```lua
//! plugin "vilinko.todo" { api = 1 }
//!
//! trait "std/Checkable@1" {
//!   read  = { done = "bool" },
//!   write = { set_done = { args = { done = "bool" } } },
//! }
//!
//! component "std/Check@1" {
//!   fields = { done = "bool" },
//!   merge = "lww",
//!   impl = { "std/Checkable@1" },
//!   construct = function(args) return { done = args.done == true } end,
//! }
//!
//! schema "vilinko.todo/item" {
//!   compose = { title = "std/Title@1", check = "std/Check@1" },
//!   fields = { priority = "u8" },
//! }
//!
//! impl("std/Checkable@1", "vilinko.todo/item@1", {
//!   extract = function(r) return { done = r.status == "done" } end,
//!   set_done = function(r, a) r.status = a.done and "done" or "open"; return r end,
//! })
//! ```
//!
//! `schema "x" { ... }` 是 `schema("x")({ ... })` 的糖；`impl` 需要三个参数。
//! 声明结果只保存 [`FnId`]；函数句柄留在解释器里，定义可以跨线程共享。

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use mlua::{Function, Lua, RegistryKey, Table, Value};
use synap_core::{SchemaId, TraitId};

use crate::error::PluginError;
use crate::fnid::FnId;
use crate::types::{parse_trait, TypeSpec, VersionedName};

/// Trait 的写方法签名。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MethodDef {
    /// 参数名到类型。
    pub args: BTreeMap<String, TypeSpec>,
    /// 文档。
    pub doc: String,
}

/// 能力契约。
#[derive(Debug)]
pub struct TraitDef {
    /// 身份。
    pub id: TraitId,
    /// 读取形状。
    pub read: BTreeMap<String, TypeSpec>,
    /// 写入方法。
    pub write: BTreeMap<String, MethodDef>,
    /// 文档。
    pub doc: String,
}

/// 组件引用；`?` 表示可选组件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentRef {
    /// 组件身份。
    pub name: VersionedName,
    /// 是否可选。
    pub optional: bool,
}

/// 可复用结构片段。
#[derive(Debug)]
pub struct ComponentDef {
    /// 身份。
    pub id: VersionedName,
    /// 自己带的字段。
    pub fields: BTreeMap<String, TypeSpec>,
    /// 内置合并策略名。
    pub merge: Option<String>,
    /// 自动委托的 Trait。
    pub traits: Vec<TraitId>,
    /// 构造纯函数。
    pub construct: FnId,
    /// 小版本迁移。
    pub upgrade: Option<FnId>,
    /// 文档。
    pub doc: String,
}

/// 领域 Schema。
#[derive(Debug)]
pub struct SchemaDef {
    /// 身份，含作者声明的大版本。
    pub id: VersionedName,
    /// 别名到组件的组合。
    pub compose: BTreeMap<String, ComponentRef>,
    /// 自有字段。
    pub fields: BTreeMap<String, TypeSpec>,
    /// 自有字段的合并策略。
    pub merge: BTreeMap<String, String>,
    /// `Trait -> 组件别名` 的消歧。
    pub delegates: BTreeMap<TraitId, String>,
    /// 构造补充函数。
    pub construct: Option<FnId>,
    /// 私有动作。
    pub actions: BTreeMap<String, FnId>,
    /// 升级函数或迁移表。
    pub upgrade: Option<FnId>,
    /// 删改冲突策略。
    pub on_delete_conflict: Option<String>,
    /// 文档。
    pub doc: String,
}

/// `(Trait, Schema)` 的显式实现。
#[derive(Debug)]
pub struct ImplDef {
    /// 能力。
    pub trait_id: TraitId,
    /// 结构。
    pub schema: VersionedName,
    /// 读取形状的纯函数。
    pub extract: FnId,
    /// 写方法名到纯函数。
    pub methods: BTreeMap<String, FnId>,
    /// 文档。
    pub doc: String,
}

/// 一条投影声明。源只能是 Trait。
#[derive(Debug)]
pub struct ProjectionDef {
    /// 投影名，形如 `namespace/name`。
    pub name: String,
    /// 数据源能力。
    pub trait_id: TraitId,
    /// 先过滤，可选。
    pub filter: Option<FnId>,
    /// 产出一行的 key 与 value。
    pub map: FnId,
}

/// 一个插件源码里的全部声明。不含 Lua 句柄。
#[derive(Debug)]
pub struct Declarations {
    /// 插件 id 与宿主 API。
    pub plugin: Option<(String, u32)>,
    /// Trait。
    pub traits: Vec<TraitDef>,
    /// 组件。
    pub components: Vec<ComponentDef>,
    /// Schema。
    pub schemas: Vec<SchemaDef>,
    /// 显式 Impl。
    pub impls: Vec<ImplDef>,
    /// 投影。
    pub projections: Vec<ProjectionDef>,
}

/// 声明解析的产物：元数据 + 本 Lua 状态里的函数句柄。
pub(crate) struct Parsed {
    pub(crate) declarations: Declarations,
    pub(crate) functions: BTreeMap<FnId, RegistryKey>,
}

struct Collected {
    declarations: Declarations,
    functions: BTreeMap<FnId, RegistryKey>,
}

type Sink = Rc<RefCell<Collected>>;

/// 在 `env` 里执行源码。函数体此时不执行，只收集声明。
pub(crate) fn collect(lua: &Lua, env: Table, source: &str) -> Result<Parsed, PluginError> {
    let sink = Rc::new(RefCell::new(Collected {
        declarations: Declarations {
            plugin: None,
            traits: Vec::new(),
            components: Vec::new(),
            schemas: Vec::new(),
            impls: Vec::new(),
            projections: Vec::new(),
        },
        functions: BTreeMap::new(),
    }));
    install(lua, &sink)?;
    lua.load(source)
        .set_name("plugin")
        .set_environment(env)
        .exec()
        .map_err(PluginError::from)?;

    for name in [
        "plugin",
        "trait",
        "component",
        "schema",
        "impl",
        "extend",
        "projection",
        "from",
    ] {
        lua.globals().raw_set(name, Value::Nil)?;
    }
    lua.gc_collect()?;

    let collected = Rc::try_unwrap(sink)
        .map_err(|_| PluginError::manifest("declaration sink is still borrowed"))?
        .into_inner();
    Ok(Parsed {
        declarations: collected.declarations,
        functions: collected.functions,
    })
}

fn install(lua: &Lua, sink: &Sink) -> mlua::Result<()> {
    let globals = lua.globals();

    {
        let sink = sink.clone();
        globals.set(
            "plugin",
            lua.create_function(move |lua, name: String| {
                let sink = sink.clone();
                lua.create_function(move |_, spec: Table| {
                    let sink = sink.clone();
                    declare_plugin(&sink, &name, &spec)
                })
            })?,
        )?;
    }
    install_curried(lua, &globals, sink, "trait", declare_trait)?;
    install_curried(lua, &globals, sink, "component", declare_component)?;
    install_curried(lua, &globals, sink, "schema", declare_schema)?;
    install_curried(lua, &globals, sink, "projection", declare_projection)?;

    {
        let sink = sink.clone();
        globals.set(
            "impl",
            lua.create_function(
                move |lua, (trait_id, schema, spec): (String, String, Table)| {
                    declare_impl(lua, &sink, &trait_id, &schema, &spec)
                },
            )?,
        )?;
    }

    globals.set(
        "extend",
        lua.create_function(|_, (): ()| -> mlua::Result<()> {
            Err(mlua::Error::RuntimeError(
                "`extend` is not implemented by this runtime layer yet".to_owned(),
            ))
        })?,
    )?;

    let synap = lua.create_table()?;
    synap.set(
        "ref",
        lua.create_function(|lua, id: String| {
            let table = lua.create_table()?;
            table.raw_set("__synap", "ref")?;
            table.raw_set("id", id)?;
            Ok(table)
        })?,
    )?;
    synap.set(
        "blob",
        lua.create_function(|lua, hash: String| {
            let table = lua.create_table()?;
            table.raw_set("__synap", "blob")?;
            table.raw_set("hash", hash)?;
            Ok(table)
        })?,
    )?;
    synap.set("api", crate::HOST_API)?;
    globals.set("synap", synap)?;

    Ok(())
}

type Curried = fn(&Lua, &Sink, &str, &Table) -> Result<(), PluginError>;

fn install_curried(
    lua: &Lua,
    globals: &Table,
    sink: &Sink,
    name: &'static str,
    declare: Curried,
) -> mlua::Result<()> {
    let sink = sink.clone();
    globals.set(
        name,
        lua.create_function(move |lua, id: String| {
            let sink = sink.clone();
            lua.create_function(move |lua, spec: Table| {
                declare(lua, &sink, &id, &spec).map_err(PluginError::into_lua)
            })
        })?,
    )?;
    Ok(())
}

impl PluginError {
    fn into_lua(self) -> mlua::Error {
        mlua::Error::RuntimeError(self.to_string())
    }
}

fn declare_plugin(sink: &Sink, name: &str, spec: &Table) -> mlua::Result<()> {
    if name.is_empty()
        || name.contains('@')
        || name.contains('/')
        || name.chars().any(char::is_whitespace)
    {
        return Err(mlua::Error::RuntimeError(format!(
            "plugin id `{name}` must be a bare namespace (no `/`, `@` or whitespace)"
        )));
    }
    let api: u32 = spec.raw_get::<Option<u32>>("api")?.ok_or_else(|| {
        mlua::Error::RuntimeError("plugin spec needs `api = <number>`".to_owned())
    })?;
    let mut collected = sink.borrow_mut();
    if collected.declarations.plugin.is_some() {
        return Err(mlua::Error::RuntimeError(
            "more than one `plugin` block".to_owned(),
        ));
    }
    collected.declarations.plugin = Some((name.to_owned(), api));
    Ok(())
}

fn declare_trait(_lua: &Lua, sink: &Sink, id: &str, spec: &Table) -> Result<(), PluginError> {
    let id = parse_trait(id)?;
    let read = string_map(spec, "read")?
        .into_iter()
        .map(|(key, value)| {
            value
                .parse()
                .map(|spec| (key.clone(), spec))
                .map_err(|err| PluginError::manifest(format!("trait {id} field `{key}`: {err}")))
        })
        .collect::<Result<_, _>>()?;
    let mut write = BTreeMap::new();
    for (name, table) in table_map(spec, "write")? {
        let args = match opt_value(&table, "args")? {
            Some(Value::Table(args)) => strings_to_types(&args)?,
            Some(other) => {
                return Err(PluginError::manifest(format!(
                    "trait `{id}` method `{name}`: `args` must be a table, got `{}`",
                    other.type_name()
                )));
            }
            None => BTreeMap::new(),
        };
        write.insert(
            name,
            MethodDef {
                args,
                doc: doc_of(&table)?,
            },
        );
    }
    sink.borrow_mut().declarations.traits.push(TraitDef {
        id,
        read,
        write,
        doc: doc_of(spec)?,
    });
    Ok(())
}

fn declare_component(lua: &Lua, sink: &Sink, id: &str, spec: &Table) -> Result<(), PluginError> {
    let id = VersionedName::parse(id)?;
    let index = sink.borrow().declarations.components.len();
    let fields = strings_to_types(&required_table(spec, "fields")?)?;
    let merge = opt_string(spec, "merge")?;
    let traits = string_list(spec, "impl")?
        .iter()
        .map(|text| parse_trait(text))
        .collect::<Result<Vec<_>, _>>()?;
    let construct = required_function(
        lua,
        sink,
        spec,
        "construct",
        FnId::ComponentConstruct { component: index },
    )?;
    let upgrade = optional_function(
        lua,
        sink,
        spec,
        "upgrade",
        FnId::ComponentUpgrade { component: index },
    )?;
    sink.borrow_mut()
        .declarations
        .components
        .push(ComponentDef {
            id,
            fields,
            merge,
            traits,
            construct,
            upgrade,
            doc: doc_of(spec)?,
        });
    Ok(())
}

fn declare_schema(lua: &Lua, sink: &Sink, id: &str, spec: &Table) -> Result<(), PluginError> {
    let id = VersionedName::parse_decl(id)?;
    let index = sink.borrow().declarations.schemas.len();
    let mut compose = BTreeMap::new();
    for (alias, reference) in string_map(spec, "compose")? {
        if alias.is_empty() || alias.starts_with('$') {
            return Err(PluginError::manifest(format!(
                "schema `{id}`: alias `{alias}` is reserved"
            )));
        }
        let (reference, optional) = match reference.strip_suffix('?') {
            Some(reference) => (reference, true),
            None => (reference.as_str(), false),
        };
        compose.insert(
            alias,
            ComponentRef {
                name: VersionedName::parse(reference)?,
                optional,
            },
        );
    }
    let fields = match opt_value(spec, "fields")? {
        Some(Value::Table(table)) => strings_to_types(&table)?,
        Some(other) => {
            return Err(PluginError::manifest(format!(
                "schema `{id}`: `fields` must be a table, got `{}`",
                other.type_name()
            )));
        }
        None => BTreeMap::new(),
    };
    let merge = string_map(spec, "merge")?;
    let mut delegates = BTreeMap::new();
    for (trait_id, alias) in string_map(spec, "delegate")? {
        delegates.insert(parse_trait(&trait_id)?, alias);
    }
    let construct = optional_function(
        lua,
        sink,
        spec,
        "construct",
        FnId::SchemaConstruct { schema: index },
    )?;
    let mut actions = BTreeMap::new();
    if let Some(value) = opt_value(spec, "actions")? {
        let Value::Table(table) = value else {
            return Err(PluginError::manifest(format!(
                "schema `{id}`: `actions` must be a table"
            )));
        };
        for pair in table.pairs::<String, Value>() {
            let (name, value) = pair?;
            let Value::Function(function) = value else {
                return Err(PluginError::manifest(format!(
                    "schema `{id}` action `{name}` must be a function"
                )));
            };
            let id = FnId::SchemaAction {
                schema: index,
                action: name.clone(),
            };
            register(lua, sink, id.clone(), Value::Function(function))?;
            actions.insert(name, id);
        }
    }
    let upgrade = optional_function(
        lua,
        sink,
        spec,
        "upgrade",
        FnId::SchemaUpgrade { schema: index },
    )?;
    let on_delete_conflict = opt_string(spec, "on_delete_conflict")?;
    if let Some(policy) = &on_delete_conflict {
        if policy != "keep" && policy != "delete" {
            return Err(PluginError::manifest(format!(
                "schema `{id}`: on_delete_conflict must be `keep` or `delete`"
            )));
        }
    }
    sink.borrow_mut().declarations.schemas.push(SchemaDef {
        id,
        compose,
        fields,
        merge,
        delegates,
        construct,
        actions,
        upgrade,
        on_delete_conflict,
        doc: doc_of(spec)?,
    });
    Ok(())
}

fn declare_impl(
    lua: &Lua,
    sink: &Sink,
    trait_id: &str,
    schema: &str,
    spec: &Table,
) -> mlua::Result<()> {
    let trait_id = parse_trait(trait_id).map_err(PluginError::into_lua)?;
    let schema = VersionedName::parse(schema).map_err(PluginError::into_lua)?;
    let index = sink.borrow().declarations.impls.len();
    let extract = required_function(lua, sink, spec, "extract", FnId::ImplExtract { imp: index })
        .map_err(PluginError::into_lua)?;
    let mut methods = BTreeMap::new();
    for pair in spec.clone().pairs::<String, Value>() {
        let (name, value) = pair?;
        if name == "extract" || name == "doc" {
            continue;
        }
        let Value::Function(function) = value else {
            return Err(mlua::Error::RuntimeError(format!(
                "impl `{trait_id}` for `{schema}`: `{name}` must be a function"
            )));
        };
        let id = FnId::ImplMethod {
            imp: index,
            method: name.clone(),
        };
        register(lua, sink, id.clone(), Value::Function(function))
            .map_err(PluginError::into_lua)?;
        methods.insert(name, id);
    }
    sink.borrow_mut().declarations.impls.push(ImplDef {
        trait_id,
        schema,
        extract,
        methods,
        doc: doc_of(spec).map_err(PluginError::into_lua)?,
    });
    Ok(())
}

fn declare_projection(lua: &Lua, sink: &Sink, name: &str, spec: &Table) -> Result<(), PluginError> {
    if name.is_empty() {
        return Err(PluginError::manifest("projection name is empty"));
    }
    if SchemaId::parse(name).is_err() {
        return Err(PluginError::manifest(format!(
            "projection `{name}` must be namespace/name"
        )));
    }
    let descriptor = match spec.raw_get::<Value>(1)? {
        Value::Table(descriptor) => descriptor,
        other => {
            return Err(PluginError::manifest(format!(
                "projection `{name}` needs a `from(...)` pipeline, got `{}`",
                other.type_name()
            )));
        }
    };
    let trait_id = match descriptor.raw_get::<Option<String>>("trait")? {
        Some(text) => parse_trait(&text)?,
        None => {
            return Err(PluginError::manifest(format!(
                "projection `{name}` has no `from(trait)`"
            )));
        }
    };
    if descriptor.raw_get::<Option<bool>>("indexed")? != Some(true) {
        return Err(PluginError::manifest(format!(
            "projection `{name}` must end with `:index()`"
        )));
    }
    let index = sink.borrow().declarations.projections.len();
    let filter = match descriptor.raw_get::<Option<Function>>("filter")? {
        Some(function) => Some(required_registered(
            lua,
            sink,
            Value::Function(function),
            FnId::ProjectionFilter { projection: index },
        )?),
        None => None,
    };
    let map = match descriptor.raw_get::<Option<Function>>("map")? {
        Some(function) => required_registered(
            lua,
            sink,
            Value::Function(function),
            FnId::ProjectionMap { projection: index },
        )?,
        None => {
            return Err(PluginError::manifest(format!(
                "projection `{name}` needs `:map(function ... end)`"
            )));
        }
    };
    sink.borrow_mut()
        .declarations
        .projections
        .push(ProjectionDef {
            name: name.to_owned(),
            trait_id,
            filter,
            map,
        });
    Ok(())
}

fn register(lua: &Lua, sink: &Sink, id: FnId, value: Value) -> Result<(), PluginError> {
    let key = match value {
        Value::Function(function) => lua.create_registry_value(function)?,
        Value::Table(table) => lua.create_registry_value(table)?,
        other => {
            return Err(PluginError::manifest(format!(
                "expected a function, got `{}`",
                other.type_name()
            )));
        }
    };
    if sink.borrow_mut().functions.insert(id, key).is_some() {
        return Err(PluginError::manifest("duplicate function id"));
    }
    Ok(())
}

fn required_registered(
    lua: &Lua,
    sink: &Sink,
    value: Value,
    id: FnId,
) -> Result<FnId, PluginError> {
    register(lua, sink, id.clone(), value)?;
    Ok(id)
}

fn required_function(
    lua: &Lua,
    sink: &Sink,
    table: &Table,
    key: &str,
    id: FnId,
) -> Result<FnId, PluginError> {
    match opt_value(table, key)? {
        Some(Value::Function(function)) => {
            required_registered(lua, sink, Value::Function(function), id)
        }
        Some(other) => Err(PluginError::manifest(format!(
            "`{key}` must be a function, got `{}`",
            other.type_name()
        ))),
        None => Err(PluginError::manifest(format!("missing `{key}` function"))),
    }
}

fn optional_function(
    lua: &Lua,
    sink: &Sink,
    table: &Table,
    key: &str,
    id: FnId,
) -> Result<Option<FnId>, PluginError> {
    match opt_value(table, key)? {
        Some(Value::Function(function)) => Ok(Some(required_registered(
            lua,
            sink,
            Value::Function(function),
            id,
        )?)),
        Some(Value::Table(value)) => Ok(Some(required_registered(
            lua,
            sink,
            Value::Table(value),
            id,
        )?)),
        Some(other) => Err(PluginError::manifest(format!(
            "`{key}` must be a function or a table, got `{}`",
            other.type_name()
        ))),
        None => Ok(None),
    }
}

fn opt_value(table: &Table, key: &str) -> Result<Option<Value>, PluginError> {
    Ok(table.raw_get::<Option<Value>>(key)?)
}

fn required_table(table: &Table, key: &str) -> Result<Table, PluginError> {
    match opt_value(table, key)? {
        Some(Value::Table(inner)) => Ok(inner),
        Some(other) => Err(PluginError::manifest(format!(
            "`{key}` must be a table, got `{}`",
            other.type_name()
        ))),
        None => Err(PluginError::manifest(format!("missing `{key}` table"))),
    }
}

fn table_map(table: &Table, key: &str) -> Result<BTreeMap<String, Table>, PluginError> {
    let mut out = BTreeMap::new();
    if let Some(value) = opt_value(table, key)? {
        let Value::Table(inner) = value else {
            return Err(PluginError::manifest(format!(
                "`{key}` must be a table, got `{}`",
                value.type_name()
            )));
        };
        for pair in inner.pairs::<String, Table>() {
            let (name, value) = pair?;
            out.insert(name, value);
        }
    }
    Ok(out)
}

fn string_map(table: &Table, key: &str) -> Result<BTreeMap<String, String>, PluginError> {
    let mut out = BTreeMap::new();
    if let Some(value) = opt_value(table, key)? {
        let Value::Table(inner) = value else {
            return Err(PluginError::manifest(format!(
                "`{key}` must be a table of strings, got `{}`",
                value.type_name()
            )));
        };
        for pair in inner.pairs::<String, String>() {
            let (name, value) = pair?;
            out.insert(name, value);
        }
    }
    Ok(out)
}

fn string_list(table: &Table, key: &str) -> Result<Vec<String>, PluginError> {
    let mut out = Vec::new();
    if let Some(value) = opt_value(table, key)? {
        let Value::Table(inner) = value else {
            return Err(PluginError::manifest(format!(
                "`{key}` must be a list of strings, got `{}`",
                value.type_name()
            )));
        };
        for item in inner.sequence_values::<String>() {
            out.push(item?);
        }
    }
    Ok(out)
}

fn strings_to_types(table: &Table) -> Result<BTreeMap<String, TypeSpec>, PluginError> {
    let mut out = BTreeMap::new();
    for pair in table.pairs::<String, String>() {
        let (name, value) = pair?;
        let spec = value
            .parse()
            .map_err(|err| PluginError::manifest(format!("field `{name}`: {err}")))?;
        out.insert(name, spec);
    }
    Ok(out)
}

fn opt_string(table: &Table, key: &str) -> Result<Option<String>, PluginError> {
    Ok(table.raw_get::<Option<String>>(key)?)
}

fn doc_of(table: &Table) -> Result<String, PluginError> {
    Ok(opt_string(table, "doc")?.unwrap_or_default())
}
