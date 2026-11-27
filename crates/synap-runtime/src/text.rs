//! 明文导出与按差异导入（006 §6.7）。
//!
//! 一个实体一个 `.md` 文件，放在 `<Schema 名>/<EntityId>.md`：
//!
//! ```text
//! ---
//! { "id": "…", "schema": "me.course/grade@1", "body": "note.body", "payload": { … } }
//! ---
//! 正文
//! ```
//!
//! frontmatter 是 payload 的原样 JSON（按组件分区），类型用单键标签保住：
//! `{"$ref": id}`、`{"$hlc": [ms, logical, node]}`、`{"$bytes": hex}`、`{"$blob": hex}`、
//! `{"$float": "NaN"}`；恰好只有一个保留键的普通 map 包成 `{"$map": {…}}`。
//! 某个分区里叫 `body` 的文本字段挪到 Markdown 正文，`"body"` 记下它的位置。
//!
//! 导入逐个实体比较：没变的不写，变了的在当前 Head 后面接一个新块，新出现的按原 id 新建，
//! 可选地给导出目录里缺席的实体写 Tombstone。原样导入不写任何块。

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Number, Value};
use synap_core::{BlobHash, Block, BlockHeader, CanonicalValue, EntityId, Hlc, LocalSeq};

use crate::engine::{Engine, EngineError};
use crate::types::VersionedName;

const TAGS: [&str; 6] = ["$ref", "$hlc", "$bytes", "$blob", "$float", "$map"];
const PLUGIN_DIR: &str = "plugins";

/// 导入选项。
#[derive(Debug, Clone, Copy, Default)]
pub struct ImportOptions {
    /// 库里存在、导出目录里没有的实体写 Tombstone。默认关闭，防止拿一个不完整的目录误删。
    pub delete_missing: bool,
    /// 只比较，不写块。
    pub dry_run: bool,
}

/// 导出的结果。
#[derive(Debug, Default, Clone)]
pub struct ExportReport {
    /// 写出的实体文件数。
    pub entities: usize,
    /// 写出的插件源码数。
    pub plugins: usize,
    /// 没能导出的实体：读不出来、或者它的插件不在本机。
    pub skipped: Vec<(EntityId, String)>,
}

/// 导入的结果。
#[derive(Debug, Default, Clone)]
pub struct ImportReport {
    /// 内容没变，没有写块。
    pub unchanged: usize,
    /// 接了一个新块。
    pub updated: usize,
    /// 新建的实体。
    pub created: usize,
    /// 写了 Tombstone 的实体。
    pub deleted: usize,
    /// 没能导入的文件和原因。其余文件照常导入。
    pub errors: Vec<(PathBuf, String)>,
}

impl ImportReport {
    /// 一共会写多少个块。
    pub fn writes(&self) -> usize {
        self.updated + self.created + self.deleted
    }
}

struct Current {
    schema: VersionedName,
    heads: Vec<Block>,
    payload: Option<CanonicalValue>,
}

impl Engine {
    /// 把整个库导出成明文目录。目录不存在就创建；已有的同名文件被覆盖。
    pub fn export_text(&self, dir: &Path) -> Result<ExportReport, EngineError> {
        let mut report = ExportReport::default();
        std::fs::create_dir_all(dir.join(PLUGIN_DIR)).map_err(io)?;
        for plugin in self.registry().definitions_arc().plugins() {
            let path = dir.join(PLUGIN_DIR).join(format!("{}.lua", plugin.id()));
            std::fs::write(path, plugin.source()).map_err(io)?;
            report.plugins += 1;
        }
        for (entity, current) in self.current_entities()? {
            let Some(payload) = current.payload else {
                continue;
            };
            let file = render(entity, &current.schema, &payload);
            let path = entity_path(dir, &current.schema, entity);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(io)?;
            }
            std::fs::write(&path, file).map_err(io)?;
            report.entities += 1;
        }
        for (entity, message) in self.unreadable_entities()? {
            report.skipped.push((entity, message));
        }
        Ok(report)
    }

    /// 从明文目录按差异导入。一个文件出错只跳过它自己。
    pub fn import_text(
        &mut self,
        dir: &Path,
        options: ImportOptions,
    ) -> Result<ImportReport, EngineError> {
        let mut report = ImportReport::default();
        let mut files = Vec::new();
        collect_md(dir, &dir.join(PLUGIN_DIR), &mut files)?;
        files.sort();
        let current = self.current_entities()?;
        let mut seen = BTreeSet::new();
        let mut blocks = Vec::new();

        for path in files {
            let parsed = std::fs::read_to_string(&path)
                .map_err(|error| error.to_string())
                .and_then(|text| parse(&text));
            let (entity, schema, payload) = match parsed {
                Ok(parsed) => parsed,
                Err(message) => {
                    report.errors.push((path, message));
                    continue;
                }
            };
            if !seen.insert(entity) {
                report
                    .errors
                    .push((path, format!("entity {entity} appears twice")));
                continue;
            }
            if let Err(error) = self.registry().fields(&schema, &payload) {
                report
                    .errors
                    .push((path, format!("does not fit {schema}: {error}")));
                continue;
            }
            match current.get(&entity) {
                Some(existing) => {
                    if existing.schema.name != schema.name {
                        report.errors.push((
                            path,
                            format!("entity {entity} is a {}, not a {schema}", existing.schema),
                        ));
                        continue;
                    }
                    if existing.payload.as_ref() == Some(&payload) {
                        report.unchanged += 1;
                        continue;
                    }
                    blocks.push((entity, schema, Some(payload), existing.heads.clone()));
                    report.updated += 1;
                }
                None => {
                    blocks.push((entity, schema, Some(payload), Vec::new()));
                    report.created += 1;
                }
            }
        }
        if options.delete_missing {
            for (entity, existing) in &current {
                if existing.payload.is_some() && !seen.contains(entity) {
                    blocks.push((
                        *entity,
                        existing.schema.clone(),
                        None,
                        existing.heads.clone(),
                    ));
                    report.deleted += 1;
                }
            }
        }
        if options.dry_run {
            return Ok(report);
        }
        for (entity, schema, payload, heads) in blocks {
            self.append_raw(entity, &schema, payload, &heads)?;
        }
        if report.writes() > 0 {
            self.settle_public();
        }
        Ok(report)
    }

    /// 每个数据实体当前的状态。多 Head 时取 Hlc 最新的一个作为内容。
    fn current_entities(&self) -> Result<BTreeMap<EntityId, Current>, EngineError> {
        let snapshot = self.volume().snapshot()?;
        let mut entities = BTreeSet::new();
        for arrival in snapshot.since(LocalSeq::ZERO)? {
            let (_, hash) = arrival?;
            if let Some(block) = snapshot.block(&hash)? {
                if block.header.schema_id.as_str() != "sys/plugin" {
                    entities.insert(block.header.entity_id);
                }
            }
        }
        let mut out = BTreeMap::new();
        for entity in entities {
            let mut heads = Vec::new();
            for hash in snapshot.heads(&entity)? {
                if let Some(block) = snapshot.block(&hash)? {
                    heads.push(block);
                }
            }
            let Some(latest) = heads.iter().max_by_key(|block| block.header.hlc) else {
                continue;
            };
            let Some(schema) = self.schema_of(latest)? else {
                continue;
            };
            let payload = if latest.header.is_tombstone {
                None
            } else {
                Some(CanonicalValue::decode(&latest.payload)?)
            };
            out.insert(
                entity,
                Current {
                    schema,
                    heads,
                    payload,
                },
            );
        }
        Ok(out)
    }

    fn unreadable_entities(&self) -> Result<Vec<(EntityId, String)>, EngineError> {
        let readable = self.current_entities()?;
        let snapshot = self.volume().snapshot()?;
        let mut out = Vec::new();
        let mut seen = BTreeSet::new();
        for arrival in snapshot.since(LocalSeq::ZERO)? {
            let (_, hash) = arrival?;
            let Some(block) = snapshot.block(&hash)? else {
                continue;
            };
            let entity = block.header.entity_id;
            if block.header.schema_id.as_str() == "sys/plugin"
                || readable.contains_key(&entity)
                || !seen.insert(entity)
            {
                continue;
            }
            out.push((
                entity,
                format!("schema `{}` has no loaded plugin", block.header.schema_id),
            ));
        }
        Ok(out)
    }

    fn append_raw(
        &mut self,
        entity: EntityId,
        schema: &VersionedName,
        payload: Option<CanonicalValue>,
        heads: &[Block],
    ) -> Result<(), EngineError> {
        let version = self
            .registry()
            .schema_plugin_id(schema)
            .and_then(|owner| self.plugin_version(owner))
            .ok_or_else(|| EngineError::UnknownSchema(schema.to_string()))?;
        let parent_hlcs: Vec<Hlc> = heads.iter().map(|block| block.header.hlc).collect();
        let block = Block {
            header: BlockHeader {
                entity_id: entity,
                parents: heads.iter().map(Block::hash).collect::<Result<_, _>>()?,
                schema_id: schema.name.clone(),
                schema_version: version,
                hlc: self.next_hlc(&parent_hlcs),
                is_tombstone: payload.is_none(),
            },
            payload: payload.map(|payload| payload.encode()).unwrap_or_default(),
        };
        self.volume().ingest(&block)?;
        Ok(())
    }
}

fn io(error: std::io::Error) -> EngineError {
    EngineError::Io(error.to_string())
}

fn entity_path(dir: &Path, schema: &VersionedName, entity: EntityId) -> PathBuf {
    let mut path = dir.to_path_buf();
    for part in schema.name.as_str().split('/') {
        path.push(part);
    }
    path.push(format!("{entity}.md"));
    path
}

fn collect_md(dir: &Path, skip: &Path, out: &mut Vec<PathBuf>) -> Result<(), EngineError> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir).map_err(io)? {
        let path = entry.map_err(io)?.path();
        if path == skip {
            continue;
        }
        if path.is_dir() {
            collect_md(&path, skip, out)?;
        } else if path.extension().is_some_and(|ext| ext == "md") {
            out.push(path);
        }
    }
    Ok(())
}

// ---- 渲染 ---------------------------------------------------------------

fn render(entity: EntityId, schema: &VersionedName, payload: &CanonicalValue) -> String {
    let mut payload = payload.clone();
    let mut body_at = None;
    let mut body = String::new();
    if let CanonicalValue::Map(partitions) = &mut payload {
        for (alias, partition) in partitions.iter_mut() {
            if let CanonicalValue::Map(fields) = partition {
                if let Some(CanonicalValue::String(text)) = fields.get("body") {
                    body = text.clone();
                    fields.remove("body");
                    body_at = Some(format!("{alias}.body"));
                    break;
                }
            }
        }
    }
    let mut head = Map::new();
    head.insert("id".to_owned(), json!(entity.to_string()));
    head.insert("schema".to_owned(), json!(schema.to_string()));
    if let Some(at) = body_at {
        head.insert("body".to_owned(), json!(at));
    }
    head.insert("payload".to_owned(), to_json(&payload));
    let head = serde_json::to_string_pretty(&Value::Object(head)).expect("json");
    format!("---\n{head}\n---\n{body}")
}

pub(crate) fn to_json(value: &CanonicalValue) -> Value {
    match value {
        CanonicalValue::Null => Value::Null,
        CanonicalValue::Bool(value) => json!(value),
        CanonicalValue::Int(value) => json!(value),
        // serde_json 总把 f64 写成带小数点或指数的形式，读回来仍是浮点；只有 NaN/∞ 需要标签。
        CanonicalValue::Float(value) => match Number::from_f64(*value) {
            Some(number) => Value::Number(number),
            None => json!({ "$float": value.to_string() }),
        },
        CanonicalValue::String(text) => json!(text),
        CanonicalValue::Bytes(bytes) => json!({ "$bytes": hex(bytes) }),
        CanonicalValue::List(items) => Value::Array(items.iter().map(to_json).collect()),
        CanonicalValue::Map(map) => {
            let object: Map<String, Value> = map
                .iter()
                .map(|(key, value)| (key.clone(), to_json(value)))
                .collect();
            if object.len() == 1 && TAGS.contains(&object.keys().next().unwrap().as_str()) {
                json!({ "$map": Value::Object(object) })
            } else {
                Value::Object(object)
            }
        }
        CanonicalValue::Ref(entity) => json!({ "$ref": entity.to_string() }),
        CanonicalValue::Blob(hash) => json!({ "$blob": hex(hash.as_bytes()) }),
        CanonicalValue::Hlc(hlc) => json!({ "$hlc": [hlc.physical_ms, hlc.logical, hlc.node_id] }),
    }
}

// ---- 解析 ---------------------------------------------------------------

fn parse(text: &str) -> Result<(EntityId, VersionedName, CanonicalValue), String> {
    let rest = text
        .strip_prefix("---\n")
        .ok_or("file must start with a `---` frontmatter line")?;
    let end = rest
        .find("\n---\n")
        .or_else(|| rest.strip_suffix("\n---").map(|head| head.len()));
    let end = end.ok_or("frontmatter is not closed by a `---` line")?;
    let head: Value =
        serde_json::from_str(&rest[..end]).map_err(|error| format!("frontmatter: {error}"))?;
    let body = rest.get(end + 5..).unwrap_or("");
    let id = head["id"].as_str().ok_or("frontmatter needs `id`")?;
    let entity: EntityId = id
        .parse()
        .map_err(|_| format!("`{id}` is not an entity id"))?;
    let schema = head["schema"]
        .as_str()
        .ok_or("frontmatter needs `schema`")?;
    let schema = VersionedName::parse(schema).map_err(|error| error.to_string())?;
    let mut payload = from_json(&head["payload"], "payload")?;
    if let Some(at) = head.get("body").and_then(Value::as_str) {
        let (alias, field) = at.split_once('.').ok_or("`body` must be `alias.field`")?;
        let CanonicalValue::Map(partitions) = &mut payload else {
            return Err("payload must be a map".to_owned());
        };
        let partition = partitions
            .entry(alias.to_owned())
            .or_insert_with(|| CanonicalValue::Map(BTreeMap::new()));
        let CanonicalValue::Map(fields) = partition else {
            return Err(format!("payload.{alias} must be a map"));
        };
        fields.insert(field.to_owned(), CanonicalValue::String(body.to_owned()));
    }
    Ok((entity, schema, payload))
}

fn from_json(value: &Value, path: &str) -> Result<CanonicalValue, String> {
    Ok(match value {
        Value::Null => CanonicalValue::Null,
        Value::Bool(value) => CanonicalValue::Bool(*value),
        Value::Number(number) => {
            if let Some(int) = number.as_i64() {
                CanonicalValue::Int(int)
            } else {
                CanonicalValue::Float(number.as_f64().ok_or(format!("{path}: bad number"))?)
            }
        }
        Value::String(text) => CanonicalValue::String(text.clone()),
        Value::Array(items) => CanonicalValue::List(
            items
                .iter()
                .enumerate()
                .map(|(index, item)| from_json(item, &format!("{path}[{index}]")))
                .collect::<Result<_, _>>()?,
        ),
        Value::Object(object) => {
            if object.len() == 1 {
                let (tag, inner) = object.iter().next().unwrap();
                if TAGS.contains(&tag.as_str()) {
                    return tagged(tag, inner, path);
                }
            }
            CanonicalValue::Map(
                object
                    .iter()
                    .map(|(key, item)| {
                        Ok((key.clone(), from_json(item, &format!("{path}.{key}"))?))
                    })
                    .collect::<Result<_, String>>()?,
            )
        }
    })
}

fn tagged(tag: &str, inner: &Value, path: &str) -> Result<CanonicalValue, String> {
    let bad = || format!("{path}: malformed `{tag}`");
    Ok(match tag {
        "$ref" => CanonicalValue::Ref(inner.as_str().ok_or_else(bad)?.parse().map_err(|_| bad())?),
        "$bytes" => CanonicalValue::Bytes(unhex(inner.as_str().ok_or_else(bad)?).ok_or_else(bad)?),
        "$blob" => {
            let bytes = unhex(inner.as_str().ok_or_else(bad)?).ok_or_else(bad)?;
            CanonicalValue::Blob(BlobHash::from_bytes(bytes.try_into().map_err(|_| bad())?))
        }
        "$float" => {
            CanonicalValue::Float(inner.as_str().ok_or_else(bad)?.parse().map_err(|_| bad())?)
        }
        "$hlc" => {
            let parts = inner
                .as_array()
                .filter(|parts| parts.len() == 3)
                .ok_or_else(bad)?;
            CanonicalValue::Hlc(Hlc {
                physical_ms: parts[0].as_u64().ok_or_else(bad)?,
                logical: parts[1]
                    .as_u64()
                    .and_then(|v| u16::try_from(v).ok())
                    .ok_or_else(bad)?,
                node_id: parts[2]
                    .as_u64()
                    .and_then(|v| u32::try_from(v).ok())
                    .ok_or_else(bad)?,
            })
        }
        "$map" => match inner {
            Value::Object(object) => CanonicalValue::Map(
                object
                    .iter()
                    .map(|(key, item)| {
                        Ok((key.clone(), from_json(item, &format!("{path}.{key}"))?))
                    })
                    .collect::<Result<_, String>>()?,
            ),
            _ => return Err(bad()),
        },
        _ => return Err(bad()),
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(text.get(index..index + 2)?, 16).ok())
        .collect()
}
