//! 把类型系统接到 store 上：插件块、实体写入、一轮投影。
//!
//! 这一层不实现 merge/upgrade/冲突；单实体出现多个 Head 时，读取侧先按 Hlc 取最新的一个
//! 并记一条 [`Issue`]，写入侧报错。
//!
//! 错误按范围隔离：坏插件只停用它自己，坏实体只让它在投影里缺席，
//! 都记成 [`Issue`]；库本身永远能打开。
//! store 的 `ProjectionDriver` 仍然不登记——Engine 在每次写入后自己算一轮，
//! 这样 Lua 状态不需要跨线程，也不会有 `ingest -> driver -> Lua` 的回环。

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use synap_core::{
    Block, BlockHash, BlockHeader, CanonicalValue, EntityId, Hlc, IndexKey, LocalSeq,
    ProjNamespace, SchemaId, TraitId,
};
use synap_store::{NamespaceWrite, ProjChange, StoreError, Volume};

use crate::error::PluginError;
use crate::plugin::Plugin;
use crate::pool::{Pool, PoolError};
use crate::registry::{ProjectionMeta, Registry};
use crate::types::VersionedName;

type Row = (IndexKey, Vec<u8>);

/// 记下来、但不阻断库运行的问题：停用的插件、投影里缺席的实体、未合并的分叉。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    /// 出问题的东西，例如 `plugin me.course` 或 `entity 0193…`。
    pub subject: String,
    /// 给人（和 AI）看的原因。
    pub message: String,
}

const MAX_ISSUES: usize = 1000;

/// Engine 的错误。
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// 插件层错误。
    #[error("plugin: {0}")]
    Plugin(#[from] PluginError),
    /// 存储层错误。
    #[error("store: {0}")]
    Store(#[from] StoreError),
    /// 规范值编解码错误。
    #[error("codec: {0}")]
    Codec(#[from] synap_core::CoreError),
    /// 实体不存在。
    #[error("entity `{0}` does not exist")]
    EntityNotFound(EntityId),
    /// 单实体多 Head 需要 merge，本层还没实现。
    #[error("entity `{entity}` has {heads} heads; merge is not implemented")]
    MultipleHeads {
        /// 实体。
        entity: EntityId,
        /// Head 数。
        heads: usize,
    },
    /// 数据块引用的插件版本不在本机。
    #[error("schema `{0}` is not owned by a loaded plugin")]
    UnknownSchema(String),
    /// 同一个插件 id 的来源已经安装，且内容不同。
    #[error("plugin `{0}` is already installed with a different source")]
    PluginConflict(String),
    /// 投影名不存在。
    #[error("projection `{0}` is unknown")]
    UnknownProjection(String),
    /// 投影检查点超过了库的 tip。
    #[error("projection `{name}` is ahead of the vault")]
    ProjectionAhead {
        /// 投影名。
        name: String,
    },
    /// 发现了多个同名 Schema（不同大版本），本层不会选。
    #[error("schema `{0}` has several loaded major versions")]
    AmbiguousSchema(String),
    /// 线程池错误。
    #[error("pool: {0}")]
    Pool(#[from] PoolError),
    /// worker 内的错误。`PluginError` 不是 `Send`，跨线程只带文本。
    #[error("worker: {0}")]
    Worker(String),
    /// 读写明文导出目录失败。
    #[error("io: {0}")]
    Io(String),
}

/// 单调 HLC。物理时间只前进不后退；同一毫秒靠 logical 排序。
struct Clock {
    physical_ms: u64,
    logical: u16,
    node_id: u32,
}

impl Clock {
    fn new(node_id: u32) -> Self {
        Self {
            physical_ms: 0,
            logical: 0,
            node_id,
        }
    }

    /// 观察到一个已有的时间戳，保证之后发出的都比它大。
    fn observe(&mut self, seen: Hlc) {
        if (seen.physical_ms, seen.logical) > (self.physical_ms, self.logical) {
            self.physical_ms = seen.physical_ms;
            self.logical = seen.logical;
        }
    }

    /// 比所有父块都新的时间戳。
    fn after(&mut self, parents: &[Hlc]) -> Hlc {
        for parent in parents {
            self.observe(*parent);
        }
        self.next()
    }

    fn next(&mut self) -> Hlc {
        let now = now_ms();
        if now > self.physical_ms {
            self.physical_ms = now;
            self.logical = 0;
        } else if let Some(logical) = self.logical.checked_add(1) {
            self.logical = logical;
        } else {
            self.physical_ms = self.physical_ms.saturating_add(1);
            self.logical = 0;
        }
        Hlc {
            physical_ms: self.physical_ms,
            logical: self.logical,
            node_id: self.node_id,
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// 一个库的运行时：插件、写入、投影。
pub struct Engine {
    volume: Volume,
    registry: Registry,
    /// 当前加载的每个插件用的源码块。
    plugins: BTreeMap<String, BlockHash>,
    /// 库里见过的所有插件块 → 插件 id。旧版本块写的数据用当前版本的代码读。
    versions: BTreeMap<BlockHash, String>,
    /// 所有已加载插件的定义合起来的哈希；任何插件变了，投影都换命名空间重建。
    definitions_hash: [u8; 32],
    clock: Clock,
    pool: Pool,
    issues: Mutex<Vec<Issue>>,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("plugins", &self.plugins)
            .finish_non_exhaustive()
    }
}

impl Engine {
    /// 打开一个库。`sources` 里是随包发行/用户安装的插件源码，它们是各插件的当前版本：
    /// 与库里一致就复用，不一致就写入新版本块。`node_id` 传 0 表示从设备身份派生。
    pub fn open(volume: Volume, sources: &[&[u8]], node_id: u32) -> Result<Self, EngineError> {
        Self::open_with_workers(volume, sources, node_id, 1)
    }

    /// 指定投影 worker 数。每个 worker 一套自己的解释器。
    pub fn open_with_workers(
        volume: Volume,
        sources: &[&[u8]],
        node_id: u32,
        workers: usize,
    ) -> Result<Self, EngineError> {
        // 调用方没给 node_id（传 0）时，从本设备的 DeviceId 派生，保证不同设备的 Hlc 不撞。
        let node_id = if node_id == 0 {
            let bytes = volume.device_id();
            let bytes = bytes.as_bytes();
            u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]).max(1)
        } else {
            node_id
        };
        let mut clock = Clock::new(node_id);
        let mut issues = Vec::new();
        let mut versions: BTreeMap<BlockHash, String> = BTreeMap::new();
        let mut installed: BTreeMap<String, (BlockHash, Vec<u8>)> = BTreeMap::new();
        // 插件第一次出现的顺序。冲突时先来的留下。
        let mut first_seen: Vec<String> = Vec::new();

        // 1. 先发现库里已有的插件块。同一插件 id 取最后到达的那一份。顺带把时钟追到库里最新的块。
        {
            let snapshot = volume.snapshot()?;
            let mut latest: BTreeMap<String, (LocalSeq, BlockHash, Vec<u8>)> = BTreeMap::new();
            for arrival in snapshot.since(LocalSeq::ZERO)? {
                let (seq, hash) = arrival?;
                let Some(block) = snapshot.block(&hash)? else {
                    continue;
                };
                clock.observe(block.header.hlc);
                if block.header.is_tombstone
                    || block.header.schema_id.as_str() != "sys/plugin"
                    || !block.header.schema_version.is_zero()
                {
                    continue;
                }
                let plugin = match Plugin::load(&block.payload) {
                    Ok(plugin) => plugin,
                    Err(error) => {
                        issues.push(Issue {
                            subject: format!("plugin block {hash}"),
                            message: format!("cannot be loaded: {error}"),
                        });
                        continue;
                    }
                };
                let id = plugin.id().to_owned();
                versions.insert(hash, id.clone());
                if !first_seen.contains(&id) {
                    first_seen.push(id.clone());
                }
                if latest
                    .get(&id)
                    .is_none_or(|(previous, _, _)| seq > *previous)
                {
                    latest.insert(id, (seq, hash, block.payload.clone()));
                }
            }
            for (id, (_, hash, source)) in latest {
                installed.insert(id, (hash, source));
            }
        }

        // 2. 本次提供的源码（随包发行的、用户目录里的）就是这个插件的当前版本。
        //    内容和库里一致就复用；不一致就是升级：写一个新块，旧块写的数据由新代码读。
        for source in sources {
            let candidate = match Plugin::load(source) {
                Ok(candidate) => candidate,
                Err(error) => {
                    issues.push(Issue {
                        subject: "plugin source".to_owned(),
                        message: format!("cannot be loaded: {error}"),
                    });
                    continue;
                }
            };
            let id = candidate.id().to_owned();
            if let Some((_, existing)) = installed.get(&id) {
                if existing.as_slice() == *source {
                    continue;
                }
            }
            let block = Block {
                header: BlockHeader {
                    entity_id: EntityId::new(),
                    parents: Vec::new(),
                    schema_id: SchemaId::parse("sys/plugin").expect("constant"),
                    schema_version: BlockHash::ZERO,
                    hlc: clock.next(),
                    is_tombstone: false,
                },
                payload: source.to_vec(),
            };
            let ingested = volume.ingest(&block)?;
            versions.insert(ingested.hash, id.clone());
            if !first_seen.contains(&id) {
                first_seen.push(id.clone());
            }
            installed.insert(id, (ingested.hash, source.to_vec()));
        }

        // 3. 逐个加入插件：标准库先来，其余按第一次出现的顺序。
        //    和已加入的插件冲突的那个被停用，库照常打开。
        let mut order: Vec<String> = first_seen
            .into_iter()
            .filter(|id| installed.contains_key(id))
            .collect();
        order.sort_by_key(|id| id != crate::plugin::STD_PLUGIN);
        let mut accepted: Vec<Plugin> = Vec::new();
        let mut plugins: BTreeMap<String, BlockHash> = BTreeMap::new();
        for id in order {
            let (hash, source) = &installed[&id];
            let plugin = Plugin::load(source)?;
            let mut trial: Vec<Plugin> = accepted
                .iter()
                .map(|plugin| Plugin::load(plugin.source()))
                .collect::<Result<_, _>>()?;
            trial.push(plugin);
            match crate::registry::Definitions::from_plugins(trial) {
                Ok(_) => {
                    accepted.push(Plugin::load(source)?);
                    plugins.insert(id, *hash);
                }
                Err(error) => issues.push(Issue {
                    subject: format!("plugin {id}"),
                    message: format!("disabled: {error}"),
                }),
            }
        }
        let registry = Registry::from_plugins(accepted)?;

        let mut hasher = blake3::Hasher::new();
        hasher.update(b"synap-definitions-v1");
        for (id, hash) in &plugins {
            hasher.update(id.as_bytes());
            hasher.update(hash.as_bytes());
        }
        let definitions_hash = *hasher.finalize().as_bytes();

        let pool = Pool::new(registry.definitions_arc(), workers)?;
        let engine = Self {
            volume,
            registry,
            plugins,
            versions,
            definitions_hash,
            clock,
            pool,
            issues: Mutex::new(issues),
        };
        engine.catch_up()?;
        Ok(engine)
    }

    /// 记下来但没有阻断运行的问题，最早的在前。
    pub fn issues(&self) -> Vec<Issue> {
        self.issues
            .lock()
            .map(|issues| issues.clone())
            .unwrap_or_default()
    }

    fn note(&self, subject: String, message: String) {
        if let Ok(mut issues) = self.issues.lock() {
            let issue = Issue { subject, message };
            if issues.contains(&issue) {
                return;
            }
            if issues.len() >= MAX_ISSUES {
                issues.remove(0);
            }
            issues.push(issue);
        }
    }

    /// 写入已经落盘之后追平投影。追平失败不让这次写入看起来失败，只记下来。
    fn settle(&self) {
        if let Err(error) = self.catch_up() {
            self.note(
                "projections".to_owned(),
                format!("catch-up failed: {error}"),
            );
        }
    }

    pub(crate) fn settle_public(&self) {
        self.settle();
    }

    pub(crate) fn next_hlc(&mut self, parents: &[Hlc]) -> Hlc {
        self.clock.after(parents)
    }

    pub(crate) fn schema_of(&self, block: &Block) -> Result<Option<VersionedName>, EngineError> {
        self.schema_for_block(&block.header.schema_id, block.header.schema_version)
    }

    /// 关掉运行时，交回卷（换插件后重开用）。
    pub fn into_volume(self) -> Volume {
        self.volume
    }

    /// 底层卷，测试与读取侧用。
    pub fn volume(&self) -> &Volume {
        &self.volume
    }

    /// 已加载的插件。
    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// 某个插件当前使用的源码块哈希。
    pub fn plugin_version(&self, plugin_id: &str) -> Option<BlockHash> {
        self.plugins.get(plugin_id).copied()
    }

    /// 投影 worker 数。
    pub fn worker_count(&self) -> usize {
        self.pool.size()
    }

    /// 投影的物理命名空间。名字不存在时是 `None`。
    pub fn projection_namespace(&self, name: &str) -> Option<ProjNamespace> {
        let meta = self
            .registry
            .projection_metas()
            .into_iter()
            .find(|meta| meta.name == name)?;
        self.plugins.get(&meta.plugin_id)?;
        Some(namespace_for(&meta, &self.definitions_hash))
    }

    /// 构造一条新实体并落盘，随后追平投影。
    pub fn create(
        &mut self,
        schema: &VersionedName,
        args: &CanonicalValue,
    ) -> Result<EntityId, EngineError> {
        let payload = self.registry.construct(schema, args)?;
        let owner = self
            .registry
            .schema_plugin_id(schema)
            .ok_or_else(|| EngineError::UnknownSchema(schema.to_string()))?
            .to_owned();
        let version = *self
            .plugins
            .get(&owner)
            .ok_or_else(|| EngineError::UnknownSchema(schema.to_string()))?;
        let entity = EntityId::new();
        let block = Block {
            header: BlockHeader {
                entity_id: entity,
                parents: Vec::new(),
                schema_id: schema.name.clone(),
                schema_version: version,
                hlc: self.clock.next(),
                is_tombstone: false,
            },
            payload: payload.encode(),
        };
        self.volume.ingest(&block)?;
        self.settle();
        Ok(entity)
    }

    /// 调一个 Trait 写方法，追加一个块，随后追平投影。
    pub fn write(
        &mut self,
        entity: EntityId,
        trait_id: &TraitId,
        method: &str,
        args: &CanonicalValue,
    ) -> Result<(), EngineError> {
        let snapshot = self.volume.snapshot()?;
        let heads = snapshot.heads(&entity)?;
        let head = match heads.as_slice() {
            [] => return Err(EngineError::EntityNotFound(entity)),
            [head] => snapshot
                .block(head)?
                .ok_or(EngineError::EntityNotFound(entity))?,
            _ => {
                return Err(EngineError::MultipleHeads {
                    entity,
                    heads: heads.len(),
                });
            }
        };
        drop(snapshot);

        let schema = self
            .schema_for_block(&head.header.schema_id, head.header.schema_version)?
            .ok_or_else(|| EngineError::UnknownSchema(head.header.schema_id.to_string()))?;
        if head.header.is_tombstone {
            return Err(EngineError::EntityNotFound(entity));
        }
        let old = CanonicalValue::decode(&head.payload)?;
        let new = self.registry.call(&schema, trait_id, method, &old, args)?;
        // 新块用当前版本的插件写（读时已经由当前代码读过旧版本）。
        let version = self
            .registry
            .schema_plugin_id(&schema)
            .and_then(|owner| self.plugins.get(owner))
            .copied()
            .unwrap_or(head.header.schema_version);
        let block = Block {
            header: BlockHeader {
                entity_id: entity,
                parents: heads.clone(),
                schema_id: head.header.schema_id.clone(),
                schema_version: version,
                hlc: self.clock.after(&[head.header.hlc]),
                is_tombstone: false,
            },
            payload: new.encode(),
        };
        self.volume.ingest(&block)?;
        self.settle();
        Ok(())
    }

    /// 把所有投影追到当前 tip。已追平的直接跳过。
    ///
    /// 行计算交给 worker 池；主线程只做快照解码、diff 和提交。
    pub fn catch_up(&self) -> Result<(), EngineError> {
        let metas = self.registry.projection_metas();
        if metas.is_empty() {
            return Ok(());
        }
        let mut writes = Vec::new();
        {
            let snapshot = self.volume.snapshot()?;
            let tip = snapshot.tip()?;
            for meta in &metas {
                if !self.plugins.contains_key(&meta.plugin_id) {
                    continue;
                }
                let namespace = namespace_for(meta, &self.definitions_hash);
                let checkpoint = snapshot.projection_checkpoint(&namespace)?;
                if checkpoint > tip {
                    return Err(EngineError::ProjectionAhead {
                        name: meta.name.clone(),
                    });
                }
                if checkpoint == tip {
                    continue;
                }

                let mut jobs = Vec::new();
                let mut entities = Vec::new();
                {
                    let world = snapshot.world(checkpoint, tip)?;
                    for unit in world.changed() {
                        let unit = unit?;
                        // 读不出来的状态当作缺席。代码和数据都确定，所以同一个状态
                        // 每次都缺席，before/after 的 diff 仍然自洽。
                        let before = self.state_or_note(unit.entity, &unit.before);
                        let after = self.state_or_note(unit.entity, &unit.after);
                        if before.is_none() && after.is_none() {
                            continue;
                        }
                        let meta = meta.clone();
                        let entity = unit.entity;
                        jobs.push(move |registry: &Registry| -> Result<(Option<Row>, Option<Row>), String> {
                            let row = |state: &Option<(VersionedName, CanonicalValue)>| match state {
                                Some((schema, payload)) => registry
                                    .projection_row(&meta, entity, schema, payload)
                                    .map_err(|error| error.to_string()),
                                None => Ok(None),
                            };
                            // before 出错时它当初也没写进索引（同一份代码、同一份数据），按缺席处理；
                            // after 出错要报出来，由主线程记成这个实体的问题。
                            let before = row(&before).unwrap_or(None);
                            let after = row(&after)?;
                            Ok((before, after))
                        });
                        entities.push(entity);
                    }
                }

                let mut ops = Vec::new();
                for (result, entity) in self.pool.run_many(jobs).into_iter().zip(entities) {
                    match result.map_err(EngineError::Pool)? {
                        Ok((before, after)) => diff(before, after, &mut ops),
                        Err(message) => {
                            self.note(
                                format!("entity {entity}"),
                                format!("left out of projection `{}`: {message}", meta.name),
                            );
                        }
                    }
                }
                writes.push(NamespaceWrite {
                    namespace,
                    from: checkpoint,
                    to: tip,
                    ops,
                });
            }
        }
        if !writes.is_empty() {
            self.volume.commit_round(&writes)?;
        }
        Ok(())
    }

    fn state_for(
        &self,
        entity: EntityId,
        blocks: &[Block],
    ) -> Result<Option<(VersionedName, CanonicalValue)>, EngineError> {
        let block = match blocks {
            [] => return Ok(None),
            [block] => block,
            _ => {
                // merge 还没落地：先显示 Hlc 最新的一个，并记下分叉。
                self.note(
                    format!("entity {entity}"),
                    format!(
                        "has {} heads; showing the latest until merge lands",
                        blocks.len()
                    ),
                );
                blocks
                    .iter()
                    .max_by_key(|block| block.header.hlc)
                    .expect("not empty")
            }
        };
        if block.header.is_tombstone {
            return Ok(None);
        }
        let Some(schema) =
            self.schema_for_block(&block.header.schema_id, block.header.schema_version)?
        else {
            return Ok(None);
        };
        Ok(Some((schema, CanonicalValue::decode(&block.payload)?)))
    }

    fn state_or_note(
        &self,
        entity: EntityId,
        blocks: &[Block],
    ) -> Option<(VersionedName, CanonicalValue)> {
        match self.state_for(entity, blocks) {
            Ok(state) => state,
            Err(error) => {
                self.note(
                    format!("entity {entity}"),
                    format!("cannot be read: {error}"),
                );
                None
            }
        }
    }

    fn schema_for_block(
        &self,
        schema_id: &SchemaId,
        version: BlockHash,
    ) -> Result<Option<VersionedName>, EngineError> {
        if version.is_zero() {
            return Ok(None);
        }
        let Some(plugin_id) = self.versions.get(&version).cloned() else {
            return Ok(None);
        };
        let mut found: Option<VersionedName> = None;
        for (plugin, def) in self.registry.schemas() {
            if plugin.id() != plugin_id || def.id.name != *schema_id {
                continue;
            }
            if found.is_some() {
                return Err(EngineError::AmbiguousSchema(schema_id.to_string()));
            }
            found = Some(def.id.clone());
        }
        Ok(found)
    }
}

/// 投影命名空间覆盖全部已加载插件：Impl、组件、Schema 都可能改变投影的输入，
/// 只哈希投影所在插件会在别的插件变化后留下旧行。原型期宁可多重建。
fn namespace_for(meta: &ProjectionMeta, definitions_hash: &[u8; 32]) -> ProjNamespace {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"synap-projection-v2");
    hasher.update(definitions_hash);
    hasher.update(meta.name.as_bytes());
    hasher.update(meta.trait_id.to_string().as_bytes());
    ProjNamespace {
        proj_id: meta.name.clone(),
        def_hash: *hasher.finalize().as_bytes(),
    }
}

fn diff(
    before: Option<(IndexKey, Vec<u8>)>,
    after: Option<(IndexKey, Vec<u8>)>,
    ops: &mut Vec<ProjChange>,
) {
    match (before, after) {
        (None, None) => {}
        (Some((key, _)), None) => ops.push(ProjChange::Delete { key }),
        (None, Some((key, value))) => ops.push(ProjChange::Put { key, value }),
        (Some((old_key, old_value)), Some((new_key, new_value))) => {
            let same_key = old_key == new_key;
            if !same_key {
                ops.push(ProjChange::Delete { key: old_key });
            }
            if !same_key || old_value != new_value {
                ops.push(ProjChange::Put {
                    key: new_key,
                    value: new_value,
                });
            }
        }
    }
}
