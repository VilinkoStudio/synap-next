//! 给 AI 的自省：库里有什么类型、能力、方法、投影，各有多少条；
//! 以及在不落盘的前提下试一个新插件（006 §6.2 的试运行）。

use std::collections::BTreeMap;

use serde_json::{json, Value};
use synap_core::{Block, CanonicalValue, EntityId, LocalSeq};

use crate::engine::{Engine, EngineError};
use crate::plugin::Plugin;
use crate::registry::{Definitions, Registry};
use crate::text::to_json;
use crate::types::TypeSpec;

/// 写插件的最小说明，随 `describe` 一起给 AI。它本身就是一个能加载的插件（测试保证）。
pub const AUTHORING_GUIDE: &str = r#"-- A plugin is one Lua file. Every function is a pure function of one record.
plugin "me.course" { api = 1 }                       -- id; names you declare must not use std/ or sys/
schema "me.course/grade" {                            -- entity type, default major @1
  compose = { title = "std/Title@1", tags = "std/Tags@1?" },  -- reuse std components; `?` = optional
  fields  = { score = "float", credits = "float", note = "text?" },
}
-- Without `construct`, own fields come from args of the same name; required ones must be present.
-- Types: bool u8 u16 u32 u64 i8 i16 i32 i64 int float text bytes ref blob time(hlc) any, [T] list, T? optional.
-- Integers must stay within ±2^53. Construct args for components: title=..., text=..., tags=[...], due=...
trait "me.course/Graded@1" { read = { score = "float" }, write = { rescore = { args = { score = "float" } } } }
impl("me.course/Graded@1", "me.course/grade@1", {
  extract = function(r) return { score = r.score } end,          -- r is the flat record
  rescore = function(r, a) r.score = a.score; return r end,      -- return the record; r.x = nil clears x
})
projection "me.course/failing" {
  from("me.course/Graded@1")                                     -- source is a trait, never a schema
    :filter(function(t) return t.Graded.score < 60 end)          -- t.<TraitShortName>, t.id
    :map(function(t) return { t.Graded.score }, { id = t.id, title = t.Titled and t.Titled.title } end)
    :index()                                                     -- key tuple (scalars only), covering value
}
-- No os/io/time/random/coroutine; pairs() is sorted. Errors in one entity never block the vault."#;

impl Engine {
    /// 库里的全部定义和每个 Schema 的实体数。
    pub fn describe(&self) -> Result<Value, EngineError> {
        let defs = self.registry().definitions_arc();
        let counts = self.schema_counts()?;
        let mut plugins = Vec::new();
        for plugin in defs.plugins() {
            plugins.push(json!({
                "id": plugin.id(),
                "version": self.plugin_version(plugin.id()).map(|hash| hash.to_string()),
            }));
        }
        let traits: Vec<Value> = defs
            .traits()
            .map(|(plugin, def)| {
                json!({
                    "id": def.id.to_string(),
                    "plugin": plugin.id(),
                    "doc": def.doc,
                    "read": types(&def.read),
                    "write": def.write.iter().map(|(name, method)| {
                        (name.clone(), json!({ "args": types(&method.args), "doc": method.doc }))
                    }).collect::<serde_json::Map<_, _>>(),
                })
            })
            .collect();
        let components: Vec<Value> = defs
            .components()
            .map(|(plugin, def)| {
                json!({
                    "id": def.id.to_string(),
                    "plugin": plugin.id(),
                    "doc": def.doc,
                    "fields": types(&def.fields),
                    "traits": def.traits.iter().map(ToString::to_string).collect::<Vec<_>>(),
                })
            })
            .collect();
        let schemas: Vec<Value> = defs
            .schemas()
            .map(|(plugin, def)| {
                let id = def.id.clone();
                json!({
                    "id": id.to_string(),
                    "plugin": plugin.id(),
                    "doc": def.doc,
                    "components": def.compose.iter().map(|(alias, component)| {
                        (alias.clone(), json!({
                            "component": component.name.to_string(),
                            "optional": component.optional,
                        }))
                    }).collect::<serde_json::Map<_, _>>(),
                    "fields": types(&def.fields),
                    "traits": self.registry().schema_traits(&id)
                        .into_iter().map(|t| t.to_string()).collect::<Vec<_>>(),
                    "entities": counts.get(&id.name.to_string()).copied().unwrap_or(0),
                })
            })
            .collect();
        let projections: Vec<Value> = self
            .registry()
            .projection_metas()
            .into_iter()
            .map(|meta| json!({ "name": meta.name, "plugin": meta.plugin_id, "from": meta.trait_id.to_string() }))
            .collect();
        let issues: Vec<Value> = self
            .issues()
            .into_iter()
            .map(|issue| json!({ "subject": issue.subject, "message": issue.message }))
            .collect();
        Ok(json!({
            "plugins": plugins,
            "traits": traits,
            "components": components,
            "schemas": schemas,
            "projections": projections,
            "issues": issues,
            "authoring_guide": AUTHORING_GUIDE,
        }))
    }

    /// 一个实体：Schema、平坦字段、当前具备的能力值、版本历史。
    pub fn get_entity(&self, entity: EntityId) -> Result<Value, EngineError> {
        let snapshot = self.volume().snapshot()?;
        let mut heads = Vec::new();
        for hash in snapshot.heads(&entity)? {
            if let Some(block) = snapshot.block(&hash)? {
                heads.push(block);
            }
        }
        let latest = heads
            .iter()
            .max_by_key(|block| block.header.hlc)
            .ok_or(EngineError::EntityNotFound(entity))?
            .clone();
        let mut history = Vec::new();
        let mut cursor = Some(latest.clone());
        while let Some(block) = cursor.take() {
            if history.len() >= 50 {
                break;
            }
            history.push(json!({
                "block": block.hash()?.to_string(),
                "at_ms": block.header.hlc.physical_ms,
                "deleted": block.header.is_tombstone,
            }));
            if let Some(parent) = block.header.parents.first() {
                cursor = snapshot.block(parent)?;
            }
        }
        let schema = self.schema_of(&latest)?;
        let mut out = json!({
            "id": entity.to_string(),
            "schema_id": latest.header.schema_id.to_string(),
            "deleted": latest.header.is_tombstone,
            "heads": heads.len(),
            "history": history,
        });
        if latest.header.is_tombstone {
            return Ok(out);
        }
        let Some(schema) = schema else {
            out["error"] = json!("its plugin is not loaded");
            return Ok(out);
        };
        out["schema"] = json!(schema.to_string());
        let payload = CanonicalValue::decode(&latest.payload)?;
        let fields = self.registry().fields(&schema, &payload)?;
        out["fields"] = to_json(&CanonicalValue::Map(fields));
        let mut traits = serde_json::Map::new();
        for trait_id in self.registry().schema_traits(&schema) {
            match self.registry().extract(&schema, &trait_id, &payload) {
                Ok(Some(value)) => {
                    traits.insert(trait_id.to_string(), to_json(&value));
                }
                Ok(None) => {}
                Err(error) => {
                    traits.insert(trait_id.to_string(), json!({ "error": error.to_string() }));
                }
            }
        }
        out["traits"] = Value::Object(traits);
        Ok(out)
    }

    /// 不落盘地试一个插件源码：能不能和现有插件一起加载，现有数据用它还读不读得出来。
    pub fn check_plugin(&self, source: &[u8]) -> Result<Value, EngineError> {
        let candidate = match Plugin::load(source) {
            Ok(plugin) => plugin,
            Err(error) => return Ok(json!({ "ok": false, "error": error.to_string() })),
        };
        let id = candidate.id().to_owned();
        let mut sources: Vec<Vec<u8>> = self
            .registry()
            .definitions_arc()
            .plugins()
            .iter()
            .filter(|plugin| plugin.id() != id)
            .map(|plugin| plugin.source().to_vec())
            .collect();
        sources.push(source.to_vec());
        let plugins = sources
            .iter()
            .map(|source| Plugin::load(source))
            .collect::<Result<Vec<_>, _>>()?;
        if let Err(error) = Definitions::from_plugins(plugins) {
            return Ok(json!({ "ok": false, "plugin": id, "error": error.to_string() }));
        }
        let registry = match Registry::load(sources.iter().map(Vec::as_slice)) {
            Ok(registry) => registry,
            Err(error) => {
                return Ok(json!({ "ok": false, "plugin": id, "error": error.to_string() }))
            }
        };
        // 用新定义重读这个插件名下已有的实体。
        let snapshot = self.volume().snapshot()?;
        let (mut readable, mut failures) = (0usize, Vec::new());
        for block in self.latest_blocks(&snapshot)? {
            let Some(schema) = self.schema_of(&block)? else {
                continue;
            };
            if self.registry().schema_plugin_id(&schema) != Some(id.as_str()) {
                continue;
            }
            let payload = CanonicalValue::decode(&block.payload)?;
            let result = registry.fields(&schema, &payload).and_then(|_| {
                for trait_id in registry.schema_traits(&schema) {
                    registry.extract(&schema, &trait_id, &payload)?;
                }
                Ok(())
            });
            match result {
                Ok(()) => readable += 1,
                Err(error) => {
                    if failures.len() < 20 {
                        failures.push(json!({
                            "entity": block.header.entity_id.to_string(),
                            "error": error.to_string(),
                        }));
                    }
                }
            }
        }
        let failed = failures.len();
        Ok(json!({
            "ok": failed == 0,
            "plugin": id,
            "existing_entities_readable": readable,
            "existing_entities_failing": failures,
            "note": "projections are rebuilt in the background after install",
        }))
    }

    fn latest_blocks(&self, snapshot: &synap_store::Snapshot) -> Result<Vec<Block>, EngineError> {
        let mut entities = std::collections::BTreeSet::new();
        for arrival in snapshot.since(LocalSeq::ZERO)? {
            let (_, hash) = arrival?;
            if let Some(block) = snapshot.block(&hash)? {
                if block.header.schema_id.as_str() != "sys/plugin" {
                    entities.insert(block.header.entity_id);
                }
            }
        }
        let mut out = Vec::new();
        for entity in entities {
            let mut latest: Option<Block> = None;
            for hash in snapshot.heads(&entity)? {
                if let Some(block) = snapshot.block(&hash)? {
                    if latest
                        .as_ref()
                        .is_none_or(|current| block.header.hlc > current.header.hlc)
                    {
                        latest = Some(block);
                    }
                }
            }
            if let Some(block) = latest.filter(|block| !block.header.is_tombstone) {
                out.push(block);
            }
        }
        Ok(out)
    }

    fn schema_counts(&self) -> Result<BTreeMap<String, usize>, EngineError> {
        let snapshot = self.volume().snapshot()?;
        let mut counts = BTreeMap::new();
        for block in self.latest_blocks(&snapshot)? {
            *counts
                .entry(block.header.schema_id.to_string())
                .or_insert(0) += 1;
        }
        Ok(counts)
    }
}

fn types(map: &BTreeMap<String, TypeSpec>) -> Value {
    Value::Object(
        map.iter()
            .map(|(name, spec)| (name.clone(), json!(spec.describe())))
            .collect(),
    )
}
