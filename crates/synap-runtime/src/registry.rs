//! 定义与解释器分离。
//!
//! [`Definitions`] 只保存形状、实现表与函数 id，是 `Send + Sync` 的；
//! [`Registry`] 把定义和每个插件自己的 Lua 状态配在一起，执行纯函数。
//! 任务跨线程只携带 `CanonicalValue`，不携带 Lua 指针。

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use mlua::Value;
use synap_core::{CanonicalValue, EntityId, IndexKey, KeyComponent, KeyScalar, TraitId};

use crate::error::PluginError;
use crate::fnid::FnId;
use crate::lua_value;
use crate::manifest::{ComponentDef, ImplDef, SchemaDef, TraitDef};
use crate::plugin::{push_args, Interpreter, Invoked, Plugin};
use crate::task::{Budget, Task, TaskOutcome};
use crate::types::{TypeSpec, VersionedName};

/// Schema 自有字段在 payload 里的分区名。用户别名不能以 `$` 开头。
pub const SELF_PARTITION: &str = "$self";

type Site = (usize, usize);

#[derive(Debug, Clone, PartialEq, Eq)]
enum Owner {
    Component(String),
    SelfFields,
}

struct ResolvedSchema {
    site: Site,
    components: BTreeMap<String, Site>,
    owners: BTreeMap<String, Owner>,
    traits: BTreeSet<TraitId>,
}

/// 投影的静态位置。
#[derive(Debug, Clone)]
pub struct ProjectionMeta {
    /// 插件下标。
    pub plugin: usize,
    /// 插件内声明下标。
    pub index: usize,
    /// 投影名。
    pub name: String,
    /// 数据源 Trait。
    pub trait_id: TraitId,
    /// 插件 id。
    pub plugin_id: String,
}

enum ImplSite {
    Explicit(Site),
    Delegated { alias: String },
}

struct FlatRecord {
    fields: BTreeMap<String, CanonicalValue>,
    extras: BTreeMap<String, CanonicalValue>,
    present: BTreeSet<String>,
}

/// 跨线程共享的纯元数据。
pub struct Definitions {
    plugins: Vec<Plugin>,
    traits: BTreeMap<TraitId, Site>,
    components: BTreeMap<VersionedName, Site>,
    schemas: BTreeMap<VersionedName, ResolvedSchema>,
    impls: BTreeMap<(TraitId, VersionedName), ImplSite>,
}

impl std::fmt::Debug for Definitions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Definitions")
            .field("plugins", &self.plugins)
            .finish_non_exhaustive()
    }
}

impl Definitions {
    /// 解析源码，只保留元数据。Lua 状态在这里创建后立即丢弃。
    pub fn load<'a>(sources: impl IntoIterator<Item = &'a [u8]>) -> Result<Self, PluginError> {
        let mut plugins = Vec::new();
        for source in sources {
            plugins.push(Plugin::load(source)?);
        }
        Self::from_plugins(plugins)
    }

    /// 从已解析的插件构造定义。
    pub fn from_plugins(plugins: Vec<Plugin>) -> Result<Self, PluginError> {
        let mut definitions = Self {
            plugins,
            traits: BTreeMap::new(),
            components: BTreeMap::new(),
            schemas: BTreeMap::new(),
            impls: BTreeMap::new(),
        };
        definitions.index()?;
        definitions.validate_impls()?;
        definitions.validate_delegation()?;
        definitions.validate_projections()?;
        definitions.collect_traits()?;
        Ok(definitions)
    }

    /// 插件元数据。
    pub fn plugins(&self) -> &[Plugin] {
        &self.plugins
    }

    /// 所有 Trait 定义。
    pub fn traits(&self) -> impl Iterator<Item = (&Plugin, &TraitDef)> {
        self.plugins.iter().flat_map(|plugin| {
            plugin
                .declarations()
                .traits
                .iter()
                .map(move |def| (plugin, def))
        })
    }

    /// 所有组件定义。
    pub fn components(&self) -> impl Iterator<Item = (&Plugin, &ComponentDef)> {
        self.plugins.iter().flat_map(|plugin| {
            plugin
                .declarations()
                .components
                .iter()
                .map(move |def| (plugin, def))
        })
    }

    /// 所有 Schema 定义。
    pub fn schemas(&self) -> impl Iterator<Item = (&Plugin, &SchemaDef)> {
        self.plugins.iter().flat_map(|plugin| {
            plugin
                .declarations()
                .schemas
                .iter()
                .map(move |def| (plugin, def))
        })
    }

    /// 所有投影声明。
    pub fn projection_metas(&self) -> Vec<ProjectionMeta> {
        let mut out = Vec::new();
        for (plugin, owner) in self.plugins.iter().enumerate() {
            for (index, def) in owner.declarations().projections.iter().enumerate() {
                out.push(ProjectionMeta {
                    plugin,
                    index,
                    name: def.name.clone(),
                    trait_id: def.trait_id.clone(),
                    plugin_id: owner.id().to_owned(),
                });
            }
        }
        out
    }

    /// 一个 Schema 可能具备的全部 Trait（实现表里有的）。
    pub fn schema_traits(&self, schema: &VersionedName) -> Vec<TraitId> {
        self.schemas
            .get(schema)
            .map(|resolved| resolved.traits.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// 一个 Schema 所属插件的 id。
    pub fn schema_plugin_id(&self, schema: &VersionedName) -> Option<&str> {
        let resolved = self.schemas.get(schema)?;
        Some(self.plugins[resolved.site.0].id())
    }

    /// Schema 的构造函数与所属插件。测试与任务派发用。
    pub fn schema_construct(&self, schema: &VersionedName) -> Option<(String, FnId)> {
        let resolved = self.schemas.get(schema)?;
        let (_, def) = self.schema_def(resolved);
        let function = def.construct.clone()?;
        Some((self.plugins[resolved.site.0].id().to_owned(), function))
    }

    /// 一条 payload 的平坦字段视图。
    pub fn fields(
        &self,
        schema: &VersionedName,
        payload: &CanonicalValue,
    ) -> Result<BTreeMap<String, CanonicalValue>, PluginError> {
        let resolved = self.schema(schema)?;
        Ok(self.decode(schema, resolved, payload)?.fields)
    }

    // ---- 查表与校验 -----------------------------------------------------

    fn index(&mut self) -> Result<(), PluginError> {
        let mut traits: BTreeMap<TraitId, Site> = BTreeMap::new();
        let mut components: BTreeMap<VersionedName, Site> = BTreeMap::new();
        let mut schemas: BTreeMap<VersionedName, ResolvedSchema> = BTreeMap::new();
        for (plugin_index, plugin) in self.plugins.iter().enumerate() {
            for (index, def) in plugin.declarations().traits.iter().enumerate() {
                if traits
                    .insert(def.id.clone(), (plugin_index, index))
                    .is_some()
                {
                    return Err(PluginError::Duplicate {
                        plugin: plugin.id().to_owned(),
                        kind: "trait",
                        name: def.id.to_string(),
                    });
                }
            }
            for (index, def) in plugin.declarations().components.iter().enumerate() {
                if components
                    .insert(def.id.clone(), (plugin_index, index))
                    .is_some()
                {
                    return Err(PluginError::Duplicate {
                        plugin: plugin.id().to_owned(),
                        kind: "component",
                        name: def.id.to_string(),
                    });
                }
            }
            for (index, def) in plugin.declarations().schemas.iter().enumerate() {
                if schemas.contains_key(&def.id) {
                    return Err(PluginError::Duplicate {
                        plugin: plugin.id().to_owned(),
                        kind: "schema",
                        name: def.id.to_string(),
                    });
                }
                let resolved =
                    resolve_schema(&self.plugins, &components, (plugin_index, index), def)?;
                schemas.insert(def.id.clone(), resolved);
            }
        }
        self.traits = traits;
        self.components = components;
        self.schemas = schemas;
        Ok(())
    }

    fn validate_impls(&mut self) -> Result<(), PluginError> {
        let mut grouped: BTreeMap<(TraitId, VersionedName), Vec<Site>> = BTreeMap::new();
        for (plugin_index, plugin) in self.plugins.iter().enumerate() {
            for (index, def) in plugin.declarations().impls.iter().enumerate() {
                self.trait_def(&def.trait_id)?;
                if !self.schemas.contains_key(&def.schema) {
                    return Err(PluginError::UnknownSchema(def.schema.to_string()));
                }
                let trait_def = self.trait_def(&def.trait_id)?;
                for method in def.methods.keys() {
                    if !trait_def.write.contains_key(method) {
                        return Err(PluginError::UnknownMethod {
                            trait_id: def.trait_id.to_string(),
                            method: method.clone(),
                        });
                    }
                }
                let owns_schema =
                    self.schemas.get(&def.schema).expect("checked above").site.0 == plugin_index;
                // 扩展 Impl 只读，不要求实现写方法；作者自己的 Impl 要实现全部写方法。
                if owns_schema && !trait_def.write.is_empty() && def.methods.is_empty() {
                    return Err(PluginError::manifest(format!(
                        "impl `{}` for `{}` implements no write method",
                        def.trait_id, def.schema
                    )));
                }
                grouped
                    .entry((def.trait_id.clone(), def.schema.clone()))
                    .or_default()
                    .push((plugin_index, index));
            }
        }
        let mut impls = BTreeMap::new();
        for ((trait_id, schema), sites) in grouped {
            let resolved = self.schemas.get(&schema).expect("checked above");
            let owner = resolved.site.0;
            let owned: Vec<Site> = sites
                .iter()
                .copied()
                .filter(|site| site.0 == owner)
                .collect();
            // 扩展 Impl（别的插件写的）还没有自己的存储，写方法会改到原插件的字段。
            // 在扩展组件落地之前，扩展 Impl 只能读。
            for site in sites.iter().filter(|site| site.0 != owner) {
                let (plugin, def) = self.impl_def(site);
                if !def.methods.is_empty() {
                    return Err(PluginError::manifest(format!(
                        "plugin `{}` implements `{trait_id}` for `{schema}`, which it does not own; \
                         extension impls cannot have write methods yet",
                        plugin.id()
                    )));
                }
            }
            if owned.is_empty() {
                // Schema 作者自己用组件委托实现了这个 Trait 时，作者的选择优先。
                let (_, def) = self.schema_def(resolved);
                let delegated = def.delegates.contains_key(&trait_id)
                    || resolved
                        .components
                        .values()
                        .any(|site| self.component(*site).1.traits.contains(&trait_id));
                if delegated {
                    continue;
                }
            }
            let chosen = match owned.len() {
                1 => owned[0],
                _ if sites.len() == 1 => sites[0],
                _ => {
                    return Err(PluginError::AmbiguousImpl {
                        trait_id: trait_id.to_string(),
                        schema: schema.to_string(),
                    });
                }
            };
            impls.insert((trait_id, schema), ImplSite::Explicit(chosen));
        }
        self.impls = impls;
        Ok(())
    }

    fn validate_delegation(&self) -> Result<(), PluginError> {
        for (name, resolved) in &self.schemas {
            let (_, def) = self.schema_def(resolved);
            let mut candidates: BTreeMap<TraitId, Vec<String>> = BTreeMap::new();
            for (alias, site) in &resolved.components {
                let (_, component) = self.component(*site);
                for trait_id in &component.traits {
                    self.trait_def(trait_id)?;
                    candidates
                        .entry(trait_id.clone())
                        .or_default()
                        .push(alias.clone());
                }
            }
            for (trait_id, aliases) in &candidates {
                match def.delegates.get(trait_id) {
                    Some(alias) => {
                        if !resolved.components.contains_key(alias) {
                            return Err(PluginError::manifest(format!(
                                "schema `{name}` delegates `{trait_id}` to unknown alias `{alias}`"
                            )));
                        }
                        if !aliases.contains(alias) {
                            return Err(PluginError::manifest(format!(
                                "schema `{name}` delegates `{trait_id}` to `{alias}`, which does not implement it"
                            )));
                        }
                    }
                    None if aliases.len() > 1 => {
                        return Err(PluginError::AmbiguousDelegate {
                            schema: name.to_string(),
                            trait_id: trait_id.to_string(),
                            components: aliases.join(", "),
                        });
                    }
                    None => {}
                }
            }
            for trait_id in def.delegates.keys() {
                if !candidates.contains_key(trait_id) {
                    return Err(PluginError::manifest(format!(
                        "schema `{name}` delegates `{trait_id}`, but no component implements it"
                    )));
                }
            }
        }
        Ok(())
    }

    fn validate_projections(&self) -> Result<(), PluginError> {
        for plugin in &self.plugins {
            for def in &plugin.declarations().projections {
                self.trait_def(&def.trait_id)?;
            }
        }
        Ok(())
    }

    fn collect_traits(&mut self) -> Result<(), PluginError> {
        let schemas: Vec<VersionedName> = self.schemas.keys().cloned().collect();
        let traits: Vec<TraitId> = self.traits.keys().cloned().collect();
        let mut found_by_schema = BTreeMap::new();
        for schema in schemas {
            let resolved = self.schemas.get(&schema).expect("listed above");
            let mut found = BTreeSet::new();
            for trait_id in &traits {
                match self.impl_site(&schema, resolved, trait_id) {
                    Ok(_) => {
                        found.insert(trait_id.clone());
                    }
                    Err(PluginError::MissingImpl { .. }) => {}
                    Err(error) => return Err(error),
                }
            }
            found_by_schema.insert(schema, found);
        }
        for (schema, found) in found_by_schema {
            if let Some(resolved) = self.schemas.get_mut(&schema) {
                resolved.traits = found;
            }
        }
        Ok(())
    }

    fn schema(&self, name: &VersionedName) -> Result<&ResolvedSchema, PluginError> {
        self.schemas
            .get(name)
            .ok_or_else(|| PluginError::UnknownSchema(name.to_string()))
    }

    fn schema_def<'a>(&'a self, resolved: &ResolvedSchema) -> (&'a Plugin, &'a SchemaDef) {
        let plugin = &self.plugins[resolved.site.0];
        (plugin, &plugin.declarations().schemas[resolved.site.1])
    }

    fn trait_def(&self, id: &TraitId) -> Result<&TraitDef, PluginError> {
        let (plugin, index) = self
            .traits
            .get(id)
            .ok_or_else(|| PluginError::UnknownTrait(id.to_string()))?;
        Ok(&self.plugins[*plugin].declarations().traits[*index])
    }

    fn component(&self, site: Site) -> (&Plugin, &ComponentDef) {
        let plugin = &self.plugins[site.0];
        (plugin, &plugin.declarations().components[site.1])
    }

    fn impl_def(&self, site: &Site) -> (&Plugin, &ImplDef) {
        let plugin = &self.plugins[site.0];
        (plugin, &plugin.declarations().impls[site.1])
    }

    fn field_spec<'a>(
        &'a self,
        resolved: &'a ResolvedSchema,
        field: &str,
        owner: &Owner,
    ) -> &'a TypeSpec {
        match owner {
            Owner::Component(alias) => {
                let site = resolved
                    .components
                    .get(alias)
                    .expect("owner has a component");
                let (_, component) = self.component(*site);
                component.fields.get(field).expect("owner has the field")
            }
            Owner::SelfFields => {
                let (_, def) = self.schema_def(resolved);
                def.fields.get(field).expect("owner has the field")
            }
        }
    }

    fn impl_site(
        &self,
        schema: &VersionedName,
        resolved: &ResolvedSchema,
        trait_id: &TraitId,
    ) -> Result<ImplSite, PluginError> {
        if let Some(site) = self.impls.get(&(trait_id.clone(), schema.clone())) {
            return Ok(match site {
                ImplSite::Explicit(site) => ImplSite::Explicit(*site),
                ImplSite::Delegated { alias } => ImplSite::Delegated {
                    alias: alias.clone(),
                },
            });
        }
        let (_, def) = self.schema_def(resolved);
        if let Some(alias) = def.delegates.get(trait_id) {
            return Ok(ImplSite::Delegated {
                alias: alias.clone(),
            });
        }
        let mut candidates = Vec::new();
        for (alias, site) in &resolved.components {
            let (_, component) = self.component(*site);
            if component.traits.contains(trait_id) {
                candidates.push((alias, *site));
            }
        }
        match candidates.as_slice() {
            [(alias, _)] => Ok(ImplSite::Delegated {
                alias: (*alias).clone(),
            }),
            [] => Err(PluginError::MissingImpl {
                schema: schema.to_string(),
                trait_id: trait_id.to_string(),
            }),
            _ => Err(PluginError::AmbiguousDelegate {
                schema: schema.to_string(),
                trait_id: trait_id.to_string(),
                components: candidates
                    .iter()
                    .map(|(alias, _)| (*alias).clone())
                    .collect::<Vec<_>>()
                    .join(", "),
            }),
        }
    }

    // ---- 记录编解码 -----------------------------------------------------

    fn decode(
        &self,
        schema: &VersionedName,
        resolved: &ResolvedSchema,
        payload: &CanonicalValue,
    ) -> Result<FlatRecord, PluginError> {
        let mut record = FlatRecord {
            fields: BTreeMap::new(),
            extras: BTreeMap::new(),
            present: BTreeSet::new(),
        };
        let partitions = match payload {
            CanonicalValue::Null => return self.finish_decode(schema, resolved, record),
            CanonicalValue::Map(map) => map,
            _ => {
                return Err(PluginError::manifest(format!(
                    "payload of `{schema}` must be a map"
                )));
            }
        };
        for (partition, value) in partitions {
            let CanonicalValue::Map(fields) = value else {
                return Err(PluginError::manifest(format!(
                    "payload partition `{partition}` of `{schema}` must be a map"
                )));
            };
            if partition == SELF_PARTITION {
                for field in fields.keys() {
                    if !resolved
                        .owners
                        .get(field)
                        .is_some_and(|owner| *owner == Owner::SelfFields)
                    {
                        return Err(PluginError::UnknownField {
                            schema: schema.to_string(),
                            field: field.clone(),
                        });
                    }
                }
                for (field, value) in fields {
                    record.fields.insert(field.clone(), value.clone());
                }
                continue;
            }
            let Some(site) = resolved.components.get(partition) else {
                record.extras.insert(partition.clone(), value.clone());
                continue;
            };
            let (_, component) = self.component(*site);
            for field in fields.keys() {
                if !component.fields.contains_key(field) {
                    return Err(PluginError::UnknownField {
                        schema: schema.to_string(),
                        field: field.clone(),
                    });
                }
            }
            record.present.insert(partition.clone());
            for (field, value) in fields {
                record.fields.insert(field.clone(), value.clone());
            }
        }
        self.finish_decode(schema, resolved, record)
    }

    fn finish_decode(
        &self,
        schema: &VersionedName,
        resolved: &ResolvedSchema,
        record: FlatRecord,
    ) -> Result<FlatRecord, PluginError> {
        let (_, def) = self.schema_def(resolved);
        for (alias, reference) in &def.compose {
            if !reference.optional && !record.present.contains(alias) {
                return Err(PluginError::MissingComponent {
                    schema: schema.to_string(),
                    component: alias.clone(),
                });
            }
        }
        Ok(record)
    }

    fn encode(
        &self,
        schema: &VersionedName,
        resolved: &ResolvedSchema,
        flat: &BTreeMap<String, CanonicalValue>,
        extras: &BTreeMap<String, CanonicalValue>,
        present: &BTreeSet<String>,
    ) -> Result<CanonicalValue, PluginError> {
        for field in flat.keys() {
            if !resolved.owners.contains_key(field) {
                return Err(PluginError::UnknownField {
                    schema: schema.to_string(),
                    field: field.clone(),
                });
            }
        }
        let (_, def) = self.schema_def(resolved);
        let mut partitions = extras.clone();
        for (alias, site) in &resolved.components {
            let (_, component) = self.component(*site);
            let optional = def
                .compose
                .get(alias)
                .is_some_and(|reference| reference.optional);
            let mut fields = BTreeMap::new();
            let mut touched = false;
            for field in component.fields.keys() {
                if let Some(value) = flat.get(field) {
                    touched = true;
                    fields.insert(field.clone(), value.clone());
                }
            }
            if optional && !present.contains(alias) && !touched {
                continue;
            }
            partitions.insert(alias.clone(), CanonicalValue::Map(fields));
        }
        if !def.fields.is_empty() {
            let mut fields = BTreeMap::new();
            for field in def.fields.keys() {
                if let Some(value) = flat.get(field) {
                    fields.insert(field.clone(), value.clone());
                }
            }
            partitions.insert(SELF_PARTITION.to_owned(), CanonicalValue::Map(fields));
        }
        Ok(CanonicalValue::Map(partitions))
    }
}

/// 定义 + 每个插件自己的 Lua 状态。
pub struct Registry {
    defs: Arc<Definitions>,
    interpreters: Vec<Interpreter>,
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registry")
            .field("plugins", &self.defs.plugins)
            .finish_non_exhaustive()
    }
}

impl Registry {
    /// 解析并加载所有插件。
    pub fn load<'a>(sources: impl IntoIterator<Item = &'a [u8]>) -> Result<Self, PluginError> {
        let defs = Definitions::load(sources)?;
        Self::from_definitions(defs)
    }

    /// 从已解析的插件构造。
    pub fn from_plugins(plugins: Vec<Plugin>) -> Result<Self, PluginError> {
        let defs = Definitions::from_plugins(plugins)?;
        Self::from_definitions(defs)
    }

    /// 在调用线程里为每个插件创建解释器。
    pub fn from_definitions(defs: Definitions) -> Result<Self, PluginError> {
        Self::from_shared(Arc::new(defs))
    }

    /// 用一份共享定义建一套解释器。多个 worker 可以各建一套，定义只解析一次。
    pub fn from_shared(defs: Arc<Definitions>) -> Result<Self, PluginError> {
        let mut interpreters = Vec::with_capacity(defs.plugins.len());
        for plugin in &defs.plugins {
            interpreters.push(Interpreter::load(plugin)?);
        }
        Ok(Self { defs, interpreters })
    }

    /// 纯元数据。
    pub fn definitions(&self) -> &Definitions {
        &self.defs
    }

    /// 共享定义。worker 用它各建一套解释器。
    pub fn definitions_arc(&self) -> Arc<Definitions> {
        self.defs.clone()
    }

    /// 所有 Trait 定义。
    pub fn traits(&self) -> impl Iterator<Item = (&Plugin, &TraitDef)> {
        self.defs.traits()
    }

    /// 所有组件定义。
    pub fn components(&self) -> impl Iterator<Item = (&Plugin, &ComponentDef)> {
        self.defs.components()
    }

    /// 所有 Schema 定义。
    pub fn schemas(&self) -> impl Iterator<Item = (&Plugin, &SchemaDef)> {
        self.defs.schemas()
    }

    /// 所有投影声明。
    pub fn projection_metas(&self) -> Vec<ProjectionMeta> {
        self.defs.projection_metas()
    }

    /// 一个 Schema 可能具备的全部 Trait。
    pub fn schema_traits(&self, schema: &VersionedName) -> Vec<TraitId> {
        self.defs.schema_traits(schema)
    }

    /// 一个 Schema 所属插件的 id。
    pub fn schema_plugin_id(&self, schema: &VersionedName) -> Option<&str> {
        self.defs.schema_plugin_id(schema)
    }

    /// Schema 构造函数，任务派发用。
    pub fn schema_construct(&self, schema: &VersionedName) -> Option<(String, FnId)> {
        self.defs.schema_construct(schema)
    }

    /// 一条 payload 的平坦字段视图。
    pub fn fields(
        &self,
        schema: &VersionedName,
        payload: &CanonicalValue,
    ) -> Result<BTreeMap<String, CanonicalValue>, PluginError> {
        self.defs.fields(schema, payload)
    }

    /// 按 `Construct` 契约构造一条新记录。
    pub fn construct(
        &self,
        schema: &VersionedName,
        args: &CanonicalValue,
    ) -> Result<CanonicalValue, PluginError> {
        self.construct_with_budget(schema, args, Budget::default())
    }

    /// 带预算的构造。
    pub fn construct_with_budget(
        &self,
        schema: &VersionedName,
        args: &CanonicalValue,
        budget: Budget,
    ) -> Result<CanonicalValue, PluginError> {
        let resolved = self.defs.schema(schema)?;
        let (_, def) = self.defs.schema_def(resolved);
        let args = normalize_args(args)?;

        let mut flat = BTreeMap::new();
        let mut present = BTreeSet::new();
        for alias in resolved.components.keys() {
            let component_ref = def
                .compose
                .get(alias)
                .expect("resolved component comes from compose");
            if component_ref.optional && !args.contains_key(alias) {
                continue;
            }
            let site = *resolved.components.get(alias).expect("resolved site");
            let (_, component) = self.defs.component(site);
            let lua = self.interpreters[site.0].lua();
            let args_table = push_args(lua, &CanonicalValue::Map(args.clone()))?;
            let value = self.invoke(site.0, &component.construct, (args_table,), budget)?;
            let Value::Table(table) = value else {
                return Err(PluginError::NotATable {
                    where_: format!("component `{}` construct", component.id),
                });
            };
            for (field, spec) in &component.fields {
                let value: Value = table.raw_get(field.as_str())?;
                let canonical = if matches!(value, Value::Nil) {
                    CanonicalValue::Null
                } else {
                    lua_value::take(value, spec, &format!("{}.{field}", component.id))?
                };
                flat.insert(field.clone(), canonical);
            }
            present.insert(alias.clone());
        }

        if def.construct.is_none() {
            // 没写 construct 时，自有字段直接从同名参数里取，按声明的类型校验。
            // 必填字段缺了要报错，不能悄悄写成空。
            for (field, spec) in &def.fields {
                match args.get(field) {
                    Some(value) if !matches!(value, CanonicalValue::Null) => {
                        let value = coerce(value.clone(), spec);
                        lua_value::check(&value, spec, &format!("{schema}.{field}"))?;
                        flat.insert(field.clone(), value);
                    }
                    _ => {
                        if !matches!(spec, TypeSpec::Optional(_) | TypeSpec::Any) {
                            return Err(PluginError::MissingField {
                                schema: schema.to_string(),
                                field: field.clone(),
                            });
                        }
                        flat.insert(field.clone(), CanonicalValue::Null);
                    }
                }
            }
        }
        if let Some(function) = &def.construct {
            let index = resolved.site.0;
            let lua = self.interpreters[index].lua();
            let args_table = push_args(lua, &CanonicalValue::Map(args.clone()))?;
            let base_table = push_args(lua, &CanonicalValue::Map(flat.clone()))?;
            let value = self.invoke(index, function, (args_table, base_table), budget)?;
            let Value::Table(table) = value else {
                return Err(PluginError::NotATable {
                    where_: format!("schema `{schema}` construct"),
                });
            };
            for pair in table.pairs::<String, Value>() {
                let (field, value) = pair?;
                let Some(owner) = resolved.owners.get(&field) else {
                    return Err(PluginError::UnknownField {
                        schema: schema.to_string(),
                        field,
                    });
                };
                let spec = self.defs.field_spec(resolved, &field, owner);
                let canonical = if matches!(value, Value::Nil) {
                    CanonicalValue::Null
                } else {
                    lua_value::take(value, spec, &format!("{schema}.{field}"))?
                };
                flat.insert(field, canonical);
            }
        }

        self.defs
            .encode(schema, resolved, &flat, &BTreeMap::new(), &present)
    }

    /// 取一个实体当前具备的 Trait 值。`None` 表示此刻不具备。
    pub fn extract(
        &self,
        schema: &VersionedName,
        trait_id: &TraitId,
        payload: &CanonicalValue,
    ) -> Result<Option<CanonicalValue>, PluginError> {
        self.extract_with_budget(schema, trait_id, payload, Budget::default())
    }

    /// 带预算的 extract。
    pub fn extract_with_budget(
        &self,
        schema: &VersionedName,
        trait_id: &TraitId,
        payload: &CanonicalValue,
        budget: Budget,
    ) -> Result<Option<CanonicalValue>, PluginError> {
        let resolved = self.defs.schema(schema)?;
        let trait_def = self.defs.trait_def(trait_id)?;
        let record = self.defs.decode(schema, resolved, payload)?;
        match self.defs.impl_site(schema, resolved, trait_id)? {
            ImplSite::Explicit(site) => {
                let (_, def) = self.defs.impl_def(&site);
                let lua = self.interpreters[site.0].lua();
                let table = push_args(lua, &CanonicalValue::Map(record.fields.clone()))?;
                let value = self.invoke(site.0, &def.extract, (table,), budget)?;
                let table = match value {
                    Value::Table(table) => table,
                    Value::Nil => return Ok(None),
                    other => {
                        return Err(PluginError::NotATable {
                            where_: format!("{trait_id}.extract (got {})", other.type_name()),
                        });
                    }
                };
                let mut out = BTreeMap::new();
                for pair in table.pairs::<String, Value>() {
                    let (field, value) = pair?;
                    let Some(spec) = trait_def.read.get(&field) else {
                        return Err(PluginError::manifest(format!(
                            "impl `{trait_id}` for `{schema}` returned unknown field `{field}`"
                        )));
                    };
                    out.insert(
                        field.clone(),
                        if matches!(value, Value::Nil) {
                            CanonicalValue::Null
                        } else {
                            lua_value::take(value, spec, &format!("{trait_id}.{field}"))?
                        },
                    );
                }
                Ok(Some(CanonicalValue::Map(out)))
            }
            ImplSite::Delegated { alias } => {
                if !record.present.contains(&alias) {
                    return Ok(None);
                }
                let (_, component) = self.defs.component(
                    *resolved
                        .components
                        .get(&alias)
                        .expect("delegated alias has a component"),
                );
                let mut out = BTreeMap::new();
                for field in trait_def.read.keys() {
                    if !component.fields.contains_key(field) {
                        return Err(PluginError::manifest(format!(
                            "`{trait_id}` is delegated to `{}`, which has no field `{field}`",
                            component.id
                        )));
                    }
                    out.insert(
                        field.clone(),
                        record
                            .fields
                            .get(field)
                            .cloned()
                            .unwrap_or(CanonicalValue::Null),
                    );
                }
                Ok(Some(CanonicalValue::Map(out)))
            }
        }
    }

    /// 调用 Trait 写方法，返回新的 payload。
    pub fn call(
        &self,
        schema: &VersionedName,
        trait_id: &TraitId,
        method: &str,
        payload: &CanonicalValue,
        args: &CanonicalValue,
    ) -> Result<CanonicalValue, PluginError> {
        self.call_with_budget(schema, trait_id, method, payload, args, Budget::default())
    }

    /// 带预算的写方法调用。
    pub fn call_with_budget(
        &self,
        schema: &VersionedName,
        trait_id: &TraitId,
        method: &str,
        payload: &CanonicalValue,
        args: &CanonicalValue,
        budget: Budget,
    ) -> Result<CanonicalValue, PluginError> {
        let resolved = self.defs.schema(schema)?;
        let trait_def = self.defs.trait_def(trait_id)?;
        let Some(signature) = trait_def.write.get(method) else {
            return Err(PluginError::UnknownMethod {
                trait_id: trait_id.to_string(),
                method: method.to_owned(),
            });
        };
        let args = normalize_args(args)?;
        for (name, spec) in &signature.args {
            match args.get(name) {
                Some(CanonicalValue::Null) | None => {
                    if !matches!(spec, TypeSpec::Optional(_)) {
                        return Err(PluginError::MissingField {
                            schema: trait_id.to_string(),
                            field: name.clone(),
                        });
                    }
                }
                Some(_) => {}
            }
        }
        for name in args.keys() {
            if !signature.args.contains_key(name) {
                return Err(PluginError::UnknownField {
                    schema: trait_id.to_string(),
                    field: name.clone(),
                });
            }
        }

        let record = self.defs.decode(schema, resolved, payload)?;
        let site = self.defs.impl_site(schema, resolved, trait_id)?;
        let mut flat = record.fields;
        let mut present = record.present;
        match site {
            ImplSite::Explicit(site) => {
                let (_, def) = self.defs.impl_def(&site);
                let Some(function) = def.methods.get(method) else {
                    return Err(PluginError::UnknownMethod {
                        trait_id: trait_id.to_string(),
                        method: method.to_owned(),
                    });
                };
                let lua = self.interpreters[site.0].lua();
                let record_table = push_args(lua, &CanonicalValue::Map(flat.clone()))?;
                let args_table = push_args(lua, &CanonicalValue::Map(args))?;
                let value = self.invoke(site.0, function, (record_table, args_table), budget)?;
                let Value::Table(table) = value else {
                    return Err(PluginError::NotATable {
                        where_: format!("{trait_id}.{method}"),
                    });
                };
                // `r.x = nil` 表示清空字段：返回的表里没有的字段一律视为清空。
                let returned: BTreeSet<String> = table
                    .pairs::<String, Value>()
                    .filter_map(|pair| pair.ok().map(|(field, _)| field))
                    .collect();
                for (field, value) in flat.iter_mut() {
                    if !returned.contains(field) {
                        *value = CanonicalValue::Null;
                    }
                }
                for pair in table.pairs::<String, Value>() {
                    let (field, value) = pair?;
                    let Some(owner) = resolved.owners.get(&field) else {
                        return Err(PluginError::UnknownField {
                            schema: schema.to_string(),
                            field,
                        });
                    };
                    let spec = self.defs.field_spec(resolved, &field, owner);
                    let canonical = if matches!(value, Value::Nil) {
                        CanonicalValue::Null
                    } else {
                        lua_value::take(value, spec, &format!("{schema}.{field}"))?
                    };
                    flat.insert(field, canonical);
                }
            }
            ImplSite::Delegated { alias } => {
                let (_, component) =
                    self.defs
                        .component(*resolved.components.get(&alias).ok_or_else(|| {
                            PluginError::ComponentAbsent {
                                schema: schema.to_string(),
                                component: alias.clone(),
                            }
                        })?);
                if !present.contains(&alias) {
                    return Err(PluginError::ComponentAbsent {
                        schema: schema.to_string(),
                        component: alias.clone(),
                    });
                }
                for (name, value) in args {
                    let Some(field_spec) = component.fields.get(&name) else {
                        return Err(PluginError::manifest(format!(
                            "`{trait_id}.{method}` is delegated to `{}`, which has no field `{name}`",
                            component.id
                        )));
                    };
                    lua_value::check(&value, field_spec, &format!("{trait_id}.{method}.{name}"))?;
                    flat.insert(name, value);
                }
                present.insert(alias);
            }
        }

        self.defs
            .encode(schema, resolved, &flat, &record.extras, &present)
    }

    /// 一个实体当前具备的 Trait 值，按短名分组。
    pub fn trait_table(
        &self,
        schema: &VersionedName,
        payload: &CanonicalValue,
    ) -> Result<BTreeMap<String, CanonicalValue>, PluginError> {
        let resolved = self.defs.schema(schema)?;
        let traits: Vec<TraitId> = resolved.traits.iter().cloned().collect();
        let mut out = BTreeMap::new();
        for trait_id in traits {
            if let Some(value) = self.extract(schema, &trait_id, payload)? {
                let short = trait_id
                    .name()
                    .rsplit('/')
                    .next()
                    .unwrap_or(trait_id.name())
                    .to_owned();
                if out.insert(short.clone(), value).is_some() {
                    return Err(PluginError::manifest(format!(
                        "schema `{schema}` has two traits with the short name `{short}`"
                    )));
                }
            }
        }
        Ok(out)
    }

    /// 跑一条投影的一行。
    pub fn projection_row(
        &self,
        meta: &ProjectionMeta,
        entity: EntityId,
        schema: &VersionedName,
        payload: &CanonicalValue,
    ) -> Result<Option<(IndexKey, Vec<u8>)>, PluginError> {
        // 数据源按完整的 TraitId（含命名空间和大版本）匹配，不按短名。
        let resolved = self.defs.schema(schema)?;
        if !resolved.traits.contains(&meta.trait_id) {
            return Ok(None);
        }
        let Some(source) = self.extract(schema, &meta.trait_id, payload)? else {
            return Ok(None);
        };
        // 闭包里用短名读能力（`t.Checkable`）。数据源占它自己的短名；
        // 其余 Trait 短名冲突时 std/ 优先，再按 TraitId 顺序；
        // 某个无关 Trait 的 extract 出错只让它缺席，不连累这一行。
        let mut others: Vec<TraitId> = resolved
            .traits
            .iter()
            .filter(|id| **id != meta.trait_id)
            .cloned()
            .collect();
        others.sort_by_key(|id| (!id.name().starts_with("std/"), id.clone()));
        let mut traits: BTreeMap<String, CanonicalValue> = BTreeMap::new();
        traits.insert(short_name(&meta.trait_id), source);
        for trait_id in others {
            let short = short_name(&trait_id);
            if traits.contains_key(&short) {
                continue;
            }
            if let Ok(Some(value)) = self.extract(schema, &trait_id, payload) {
                traits.insert(short, value);
            }
        }
        let def = &self.defs.plugins[meta.plugin].declarations().projections[meta.index];
        let lua = self.interpreters[meta.plugin].lua();
        let table = lua.create_table()?;
        for (name, value) in &traits {
            table.raw_set(name.as_str(), lua_value::push(lua, value)?)?;
        }
        table.raw_set("id", lua_value::push(lua, &CanonicalValue::Ref(entity))?)?;

        if let Some(filter) = &def.filter {
            let verdict = self.invoke(meta.plugin, filter, (table.clone(),), Budget::default())?;
            match verdict {
                Value::Nil | Value::Boolean(false) => return Ok(None),
                _ => {}
            }
        }
        let values =
            self.interpreters[meta.plugin].invoke_multi(&def.map, (table,), Budget::default())?;
        let mut values = values.into_iter();
        let key = values.next().unwrap_or(Value::Nil);
        let value = values.next().unwrap_or(Value::Nil);
        let Value::Table(key) = key else {
            return Ok(None);
        };
        let mut parts = Vec::new();
        for (index, item) in key.sequence_values::<Value>().enumerate() {
            let item = item?;
            let canonical = lua_value::take_any(item, &format!("{} key[{index}]", meta.name))?;
            parts.push(KeyComponent::asc(key_scalar(canonical, &meta.name)?));
        }
        let value = lua_value::take_any(value, &format!("{} value", meta.name))?;
        Ok(Some((IndexKey::new(parts, entity), value.encode())))
    }

    /// 跑一条跨线程可描述的单函数任务。
    pub fn run_task(&self, task: &Task) -> Result<TaskOutcome, PluginError> {
        let index = self
            .defs
            .plugins
            .iter()
            .position(|plugin| plugin.id() == task.plugin)
            .ok_or_else(|| PluginError::manifest(format!("no plugin `{}`", task.plugin)))?;
        let lua = self.interpreters[index].lua();
        let invoked = match task.args.as_slice() {
            [] => self.interpreters[index].invoke(&task.function, (), task.budget),
            [one] => self.interpreters[index].invoke(
                &task.function,
                (push_args(lua, one)?,),
                task.budget,
            ),
            [one, two] => self.interpreters[index].invoke(
                &task.function,
                (push_args(lua, one)?, push_args(lua, two)?),
                task.budget,
            ),
            _ => {
                return Err(PluginError::manifest(
                    "task supports at most two arguments".to_owned(),
                ));
            }
        };
        match invoked? {
            Invoked::Preempted => Ok(TaskOutcome::Preempted),
            Invoked::Value(value) => Ok(TaskOutcome::Value(lua_value::take_any(value, "task")?)),
        }
    }

    fn invoke(
        &self,
        plugin: usize,
        function: &FnId,
        args: impl mlua::IntoLuaMulti,
        budget: Budget,
    ) -> Result<Value, PluginError> {
        match self.interpreters[plugin].invoke(function, args, budget)? {
            Invoked::Value(value) => Ok(value),
            Invoked::Preempted => Err(PluginError::Preempted),
        }
    }
}

fn resolve_schema(
    plugins: &[Plugin],
    components: &BTreeMap<VersionedName, Site>,
    site: Site,
    def: &SchemaDef,
) -> Result<ResolvedSchema, PluginError> {
    let mut resolved_components = BTreeMap::new();
    let mut owners = BTreeMap::new();
    for (alias, reference) in &def.compose {
        let Some(component_site) = components.get(&reference.name).copied() else {
            return Err(PluginError::UnknownComponent {
                schema: def.id.to_string(),
                component: reference.name.to_string(),
            });
        };
        let component = &plugins[component_site.0].declarations().components[component_site.1];
        for field in component.fields.keys() {
            if let Some(previous) = owners.insert(field.clone(), Owner::Component(alias.clone())) {
                return Err(duplicate_field(def, field, &previous, alias));
            }
        }
        resolved_components.insert(alias.clone(), component_site);
    }
    for field in def.fields.keys() {
        if let Some(previous) = owners.insert(field.clone(), Owner::SelfFields) {
            return Err(duplicate_field(def, field, &previous, SELF_PARTITION));
        }
    }
    Ok(ResolvedSchema {
        site,
        components: resolved_components,
        owners,
        traits: BTreeSet::new(),
    })
}

fn normalize_args(args: &CanonicalValue) -> Result<BTreeMap<String, CanonicalValue>, PluginError> {
    match args {
        CanonicalValue::Null => Ok(BTreeMap::new()),
        CanonicalValue::Map(map) => Ok(map.clone()),
        _ => Err(PluginError::manifest("arguments must be a map")),
    }
}

fn key_scalar(value: CanonicalValue, projection: &str) -> Result<KeyScalar, PluginError> {
    Ok(match value {
        CanonicalValue::Null => KeyScalar::Null,
        CanonicalValue::Bool(value) => KeyScalar::Bool(value),
        CanonicalValue::Int(value) => KeyScalar::Int(value),
        CanonicalValue::String(value) => KeyScalar::Text(value),
        CanonicalValue::Bytes(value) => KeyScalar::Bytes(value),
        CanonicalValue::Ref(entity) => KeyScalar::Ref(entity),
        CanonicalValue::Hlc(hlc) => {
            KeyScalar::Int(i64::try_from(hlc.physical_ms).map_err(|_| {
                PluginError::manifest(format!(
                    "projection `{projection}` key time is out of range"
                ))
            })?)
        }
        CanonicalValue::Float(_) => {
            return Err(PluginError::manifest(format!(
                "projection `{projection}` key cannot be a float"
            )));
        }
        CanonicalValue::List(_) | CanonicalValue::Map(_) | CanonicalValue::Blob(_) => {
            return Err(PluginError::manifest(format!(
                "projection `{projection}` key must be a scalar"
            )));
        }
    })
}

fn duplicate_field(def: &SchemaDef, field: &str, previous: &Owner, next: &str) -> PluginError {
    PluginError::DuplicateField {
        schema: def.id.to_string(),
        field: field.to_owned(),
        left: match previous {
            Owner::Component(alias) => alias.clone(),
            Owner::SelfFields => SELF_PARTITION.to_owned(),
        },
        right: next.to_owned(),
    }
}

fn short_name(trait_id: &TraitId) -> String {
    trait_id
        .name()
        .rsplit('/')
        .next()
        .unwrap_or(trait_id.name())
        .to_owned()
}

/// 整数写进浮点字段时提升为浮点，和 Lua 侧 `take` 的规则一致。
fn coerce(value: CanonicalValue, spec: &TypeSpec) -> CanonicalValue {
    match (spec, value) {
        (TypeSpec::Optional(inner), value) => coerce(value, inner),
        (TypeSpec::Float, CanonicalValue::Int(number)) => CanonicalValue::Float(number as f64),
        (_, value) => value,
    }
}
