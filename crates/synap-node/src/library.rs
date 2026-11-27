//! `serve` 握住的库。标准库随进程带上，`plugins/*.lua` 是这个库自己的插件。
//!
//! `Engine` 里的 Lua 状态不能跨线程。它住在自己的线程上，请求排队进去，做完再回来。

use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};

use axum::http::StatusCode;
use synap_core::{CanonicalValue, EntityId, TraitId};
use synap_http::{Library, LibraryError, VaultFacts};
use synap_runtime::{Engine, VersionedName, STDLIB};
use synap_store::{KeyWindow, ScanDir, Volume};

use crate::vault::{self, VaultError};

pub struct Session {
    pub created: bool,
    pub server_name: String,
    pub library: Arc<EngineLibrary>,
}

pub struct EngineLibrary {
    tx: mpsc::Sender<Job>,
}

enum Job {
    Tip(mpsc::Sender<Result<u64, String>>),
    Custody(mpsc::Sender<Result<String, String>>),
    Create {
        schema: VersionedName,
        args: CanonicalValue,
        reply: mpsc::Sender<Result<String, LibraryError>>,
    },
    Call {
        entity: EntityId,
        trait_id: TraitId,
        method: String,
        args: CanonicalValue,
        reply: mpsc::Sender<Result<(), LibraryError>>,
    },
    Rows {
        projection: String,
        limit: usize,
        reply: mpsc::Sender<Result<Vec<serde_json::Value>, LibraryError>>,
    },
    Describe(mpsc::Sender<Result<serde_json::Value, LibraryError>>),
    Get {
        entity: EntityId,
        reply: mpsc::Sender<Result<serde_json::Value, LibraryError>>,
    },
    Plugin {
        source: String,
        install: bool,
        reply: mpsc::Sender<Result<serde_json::Value, LibraryError>>,
    },
}

/// 打开或新建卷，装上标准库和 `plugins` 目录里的源码。
pub fn open_session(data_dir: &Path, server_name: &str) -> Result<Session, SessionError> {
    let opened = vault::open_or_create(data_dir, server_name)?;
    let created = opened.created;
    let name = opened.config.server_name.clone();
    let sources = plugin_sources(data_dir)?;
    let library = EngineLibrary::spawn(data_dir.to_path_buf(), opened.into_volume(), sources)?;
    Ok(Session {
        created,
        server_name: name,
        library: Arc::new(library),
    })
}

pub fn plugin_sources(data_dir: &Path) -> Result<Vec<Vec<u8>>, VaultError> {
    let mut sources = vec![STDLIB.as_bytes().to_vec()];
    let dir = data_dir.join("plugins");
    if !dir.exists() {
        return Ok(sources);
    }
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(&dir).map_err(|err| VaultError::Io(err.to_string()))? {
        let entry = entry.map_err(|err| VaultError::Io(err.to_string()))?;
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "lua") {
            paths.push(path);
        }
    }
    paths.sort();
    for path in paths {
        sources.push(std::fs::read(&path).map_err(|err| VaultError::Io(err.to_string()))?);
    }
    Ok(sources)
}

impl EngineLibrary {
    fn spawn(
        data_dir: PathBuf,
        volume: Volume,
        sources: Vec<Vec<u8>>,
    ) -> Result<Self, SessionError> {
        let (tx, rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("synap-engine".to_owned())
            .spawn(move || {
                let borrowed: Vec<&[u8]> = sources.iter().map(Vec::as_slice).collect();
                match Engine::open(volume, &borrowed, 0) {
                    Ok(engine) => {
                        let _ = ready_tx.send(Ok(()));
                        engine_thread(engine, rx, &data_dir);
                    }
                    Err(err) => {
                        let _ = ready_tx.send(Err(err.to_string()));
                    }
                }
            })
            .map_err(|err| SessionError::Engine(err.to_string()))?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self { tx }),
            Ok(Err(err)) => Err(SessionError::Engine(err)),
            Err(_) => Err(SessionError::Engine("library thread stopped".to_owned())),
        }
    }

    fn ask<T>(
        &self,
        job: Job,
        rx: mpsc::Receiver<Result<T, LibraryError>>,
    ) -> Result<T, LibraryError> {
        self.tx.send(job).map_err(|_| stopped())?;
        rx.recv().map_err(|_| stopped())?
    }
}

fn engine_thread(mut engine: Engine, rx: mpsc::Receiver<Job>, data_dir: &Path) {
    while let Ok(job) = rx.recv() {
        match job {
            Job::Tip(reply) => {
                let _ = reply.send(read_tip(&engine));
            }
            Job::Custody(reply) => {
                let _ = reply.send(read_custody(&engine));
            }
            Job::Create {
                schema,
                args,
                reply,
            } => {
                let _ = reply.send(create_entity(&mut engine, &schema, &args));
            }
            Job::Call {
                entity,
                trait_id,
                method,
                args,
                reply,
            } => {
                let _ = reply.send(call_entity(&mut engine, entity, &trait_id, &method, &args));
            }
            Job::Rows {
                projection,
                limit,
                reply,
            } => {
                let _ = reply.send(read_rows(&engine, &projection, limit));
            }
            Job::Describe(reply) => {
                let _ = reply.send(engine.describe().map_err(engine_error));
            }
            Job::Get { entity, reply } => {
                let _ = reply.send(engine.get_entity(entity).map_err(engine_error));
            }
            Job::Plugin {
                source,
                install,
                reply,
            } => {
                let checked = engine.check_plugin(source.as_bytes()).map_err(engine_error);
                let ready = matches!(&checked, Ok(report) if report["ok"] == true);
                if !(install && ready) {
                    let _ = reply.send(checked);
                    continue;
                }
                let mut report = checked.unwrap_or_default();
                let id = report["plugin"].as_str().unwrap_or_default().to_owned();
                if let Err(message) = write_plugin_file(&data_dir.join("plugins"), &id, &source) {
                    let _ = reply.send(Err(LibraryError {
                        status: StatusCode::INTERNAL_SERVER_ERROR,
                        message,
                    }));
                    continue;
                }
                match reopen(engine, data_dir) {
                    Ok(reopened) => {
                        engine = reopened;
                        report["installed"] = serde_json::json!(true);
                        let issues: Vec<_> = engine
                            .issues()
                            .into_iter()
                            .map(|issue| serde_json::json!({ "subject": issue.subject, "message": issue.message }))
                            .collect();
                        report["issues"] = serde_json::json!(issues);
                        let _ = reply.send(Ok(report));
                    }
                    Err(message) => {
                        let _ = reply.send(Err(LibraryError {
                            status: StatusCode::INTERNAL_SERVER_ERROR,
                            message: message.clone(),
                        }));
                        eprintln!("synap-engine: stopped after plugin install failed: {message}");
                        return;
                    }
                }
            }
        }
    }
}

/// 用 `plugins/` 里的新插件集重开库。
fn reopen(engine: Engine, data_dir: &Path) -> Result<Engine, String> {
    let sources = plugin_sources(data_dir).map_err(|err| err.to_string())?;
    let borrowed: Vec<&[u8]> = sources.iter().map(Vec::as_slice).collect();
    Engine::open(engine.into_volume(), &borrowed, 0).map_err(|err| err.to_string())
}

/// 把源码写进 `plugins/`：同 id 的旧文件就地覆盖，先写临时文件再改名。
fn write_plugin_file(dir: &Path, id: &str, source: &str) -> Result<(), String> {
    if id.is_empty()
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
    {
        return Err(format!("plugin id `{id}` cannot be a file name"));
    }
    std::fs::create_dir_all(dir).map_err(|err| err.to_string())?;
    let mut target = dir.join(format!("{id}.lua"));
    for entry in std::fs::read_dir(dir)
        .map_err(|err| err.to_string())?
        .flatten()
    {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "lua") {
            let same = std::fs::read(&path)
                .ok()
                .and_then(|bytes| synap_runtime::Plugin::load(&bytes).ok())
                .is_some_and(|plugin| plugin.id() == id);
            if same {
                target = path;
                break;
            }
        }
    }
    let temp = target.with_extension("lua.tmp");
    std::fs::write(&temp, source).map_err(|err| err.to_string())?;
    std::fs::rename(&temp, &target).map_err(|err| err.to_string())
}

fn read_tip(engine: &Engine) -> Result<u64, String> {
    engine
        .volume()
        .snapshot()
        .and_then(|snapshot| snapshot.tip())
        .map(|seq| seq.get())
        .map_err(|err| err.to_string())
}

fn read_custody(engine: &Engine) -> Result<String, String> {
    match engine.volume().custody().map_err(|err| err.to_string())? {
        synap_store::MasterCustody::Embedded => Ok("embedded".to_owned()),
        synap_store::MasterCustody::External => Ok("external".to_owned()),
    }
}

fn create_entity(
    engine: &mut Engine,
    schema: &VersionedName,
    args: &CanonicalValue,
) -> Result<String, LibraryError> {
    engine
        .create(schema, args)
        .map(|id| id.to_string())
        .map_err(engine_error)
}

fn call_entity(
    engine: &mut Engine,
    entity: EntityId,
    trait_id: &TraitId,
    method: &str,
    args: &CanonicalValue,
) -> Result<(), LibraryError> {
    engine
        .write(entity, trait_id, method, args)
        .map_err(engine_error)
}

fn read_rows(
    engine: &Engine,
    projection: &str,
    limit: usize,
) -> Result<Vec<serde_json::Value>, LibraryError> {
    let namespace = engine
        .projection_namespace(projection)
        .ok_or_else(|| LibraryError {
            status: StatusCode::NOT_FOUND,
            message: format!("projection `{projection}` is unknown"),
        })?;
    let page = engine
        .volume()
        .snapshot()
        .and_then(|snapshot| {
            snapshot.projection_range(&namespace, &KeyWindow::All, ScanDir::Forward, None, limit)
        })
        .map_err(|err| LibraryError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: err.to_string(),
        })?;
    page.rows
        .into_iter()
        .map(|row| {
            CanonicalValue::decode(&row.value)
                .map(to_json)
                .map_err(|err| LibraryError {
                    status: StatusCode::INTERNAL_SERVER_ERROR,
                    message: err.to_string(),
                })
        })
        .collect()
}

impl VaultFacts for EngineLibrary {
    fn tip(&self) -> Result<u64, String> {
        let (reply, rx) = mpsc::channel();
        self.tx
            .send(Job::Tip(reply))
            .map_err(|_| "library thread stopped".to_owned())?;
        rx.recv().map_err(|_| "library thread stopped".to_owned())?
    }

    fn custody(&self) -> Result<String, String> {
        let (reply, rx) = mpsc::channel();
        self.tx
            .send(Job::Custody(reply))
            .map_err(|_| "library thread stopped".to_owned())?;
        rx.recv().map_err(|_| "library thread stopped".to_owned())?
    }
}

impl Library for EngineLibrary {
    fn describe(&self) -> Result<serde_json::Value, LibraryError> {
        let (reply, rx) = mpsc::channel();
        self.ask(Job::Describe(reply), rx)
    }

    fn get_entity(&self, entity: &str) -> Result<serde_json::Value, LibraryError> {
        let entity = parse_entity(entity)?;
        let (reply, rx) = mpsc::channel();
        self.ask(Job::Get { entity, reply }, rx)
    }

    fn plugin(&self, source: &str, install: bool) -> Result<serde_json::Value, LibraryError> {
        let (reply, rx) = mpsc::channel();
        self.ask(
            Job::Plugin {
                source: source.to_owned(),
                install,
                reply,
            },
            rx,
        )
    }

    fn create_entity(&self, schema: &str, args: serde_json::Value) -> Result<String, LibraryError> {
        let schema = VersionedName::parse(schema).map_err(|err| LibraryError {
            status: StatusCode::BAD_REQUEST,
            message: err.to_string(),
        })?;
        let args = object_args(args)?;
        let (reply, rx) = mpsc::channel();
        self.ask(
            Job::Create {
                schema,
                args,
                reply,
            },
            rx,
        )
    }

    fn call_entity(
        &self,
        entity: &str,
        trait_name: &str,
        method: &str,
        args: serde_json::Value,
    ) -> Result<(), LibraryError> {
        let entity = parse_entity(entity)?;
        let (name, major) = trait_name.rsplit_once('@').ok_or_else(|| LibraryError {
            status: StatusCode::BAD_REQUEST,
            message: format!("trait `{trait_name}` must end with @major"),
        })?;
        let major: u16 = major.parse().map_err(|_| LibraryError {
            status: StatusCode::BAD_REQUEST,
            message: format!("trait `{trait_name}` has a bad major version"),
        })?;
        let trait_id = TraitId::parse(name, major).map_err(|_| LibraryError {
            status: StatusCode::BAD_REQUEST,
            message: format!("trait `{name}` is not namespace/name"),
        })?;
        let args = object_args(args)?;
        let (reply, rx) = mpsc::channel();
        self.ask(
            Job::Call {
                entity,
                trait_id,
                method: method.to_owned(),
                args,
                reply,
            },
            rx,
        )
    }

    fn rows(&self, projection: &str, limit: usize) -> Result<Vec<serde_json::Value>, LibraryError> {
        let (reply, rx) = mpsc::channel();
        self.ask(
            Job::Rows {
                projection: projection.to_owned(),
                limit,
                reply,
            },
            rx,
        )
    }
}

fn parse_entity(entity: &str) -> Result<EntityId, LibraryError> {
    entity.parse().map_err(|_| LibraryError {
        status: StatusCode::BAD_REQUEST,
        message: format!("entity id `{entity}` is not a uuid v7"),
    })
}

fn stopped() -> LibraryError {
    LibraryError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        message: "library thread stopped".to_owned(),
    }
}

fn object_args(value: serde_json::Value) -> Result<CanonicalValue, LibraryError> {
    if value.is_null() {
        return Ok(CanonicalValue::map());
    }
    if !value.is_object() {
        return Err(LibraryError {
            status: StatusCode::BAD_REQUEST,
            message: "args must be a JSON object".to_owned(),
        });
    }
    to_canonical(value)
}

fn to_canonical(value: serde_json::Value) -> Result<CanonicalValue, LibraryError> {
    match value {
        serde_json::Value::Null => Ok(CanonicalValue::Null),
        serde_json::Value::Bool(value) => Ok(CanonicalValue::Bool(value)),
        serde_json::Value::Number(value) => {
            if let Some(int) = value.as_i64() {
                Ok(CanonicalValue::Int(int))
            } else if let Some(float) = value.as_f64() {
                Ok(CanonicalValue::Float(float))
            } else {
                Err(LibraryError {
                    status: StatusCode::BAD_REQUEST,
                    message: "number is out of range".to_owned(),
                })
            }
        }
        serde_json::Value::String(value) => Ok(CanonicalValue::String(value)),
        serde_json::Value::Array(values) => values
            .into_iter()
            .map(to_canonical)
            .collect::<Result<Vec<_>, _>>()
            .map(CanonicalValue::List),
        serde_json::Value::Object(map) => map
            .into_iter()
            .map(|(key, value)| Ok((key, to_canonical(value)?)))
            .collect::<Result<_, _>>()
            .map(CanonicalValue::Map),
    }
}

fn to_json(value: CanonicalValue) -> serde_json::Value {
    match value {
        CanonicalValue::Null => serde_json::Value::Null,
        CanonicalValue::Bool(value) => serde_json::Value::Bool(value),
        CanonicalValue::Int(value) => serde_json::json!(value),
        CanonicalValue::Float(value) => serde_json::json!(value),
        CanonicalValue::String(value) => serde_json::Value::String(value),
        CanonicalValue::Bytes(value) => serde_json::json!({ "bytes": hex(&value) }),
        CanonicalValue::List(values) => {
            serde_json::Value::Array(values.into_iter().map(to_json).collect())
        }
        CanonicalValue::Map(map) => serde_json::Value::Object(
            map.into_iter()
                .map(|(key, value)| (key, to_json(value)))
                .collect(),
        ),
        CanonicalValue::Ref(id) => serde_json::json!({ "ref": id.to_string() }),
        CanonicalValue::Blob(hash) => serde_json::json!({ "blob": hex(hash.as_bytes()) }),
        CanonicalValue::Hlc(hlc) => serde_json::json!({
            "hlc": {
                "physical_ms": hlc.physical_ms,
                "logical": hlc.logical,
                "node_id": hlc.node_id,
            }
        }),
    }
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0xf) as usize] as char);
    }
    out
}

fn engine_error(err: synap_runtime::EngineError) -> LibraryError {
    let status = match &err {
        synap_runtime::EngineError::EntityNotFound(_)
        | synap_runtime::EngineError::UnknownSchema(_)
        | synap_runtime::EngineError::UnknownProjection(_) => StatusCode::NOT_FOUND,
        synap_runtime::EngineError::MultipleHeads { .. }
        | synap_runtime::EngineError::PluginConflict(_) => StatusCode::CONFLICT,
        synap_runtime::EngineError::AmbiguousSchema(_)
        | synap_runtime::EngineError::Plugin(_)
        | synap_runtime::EngineError::Codec(_) => StatusCode::BAD_REQUEST,
        synap_runtime::EngineError::Store(_)
        | synap_runtime::EngineError::ProjectionAhead { .. }
        | synap_runtime::EngineError::Pool(_)
        | synap_runtime::EngineError::Worker(_)
        | synap_runtime::EngineError::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    LibraryError {
        status,
        message: err.to_string(),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error(transparent)]
    Vault(#[from] VaultError),
    #[error("{0}")]
    Engine(String),
}

/// 不起服务，直接在本进程里打开库做一件事（导出、导入）。节点在跑时库被锁住，会打不开。
pub fn with_engine<T>(
    data_dir: &Path,
    run: impl FnOnce(&mut Engine) -> Result<T, String>,
) -> Result<T, SessionError> {
    if !vault::exists(data_dir) {
        return Err(SessionError::Engine(format!(
            "{} is not a vault",
            data_dir.display()
        )));
    }
    let opened = vault::open_or_create(data_dir, "synap-node")?;
    let sources = plugin_sources(data_dir)?;
    let borrowed: Vec<&[u8]> = sources.iter().map(Vec::as_slice).collect();
    let mut engine = Engine::open(opened.into_volume(), &borrowed, 0)
        .map_err(|err| SessionError::Engine(err.to_string()))?;
    for issue in engine.issues() {
        eprintln!("warning: {}: {}", issue.subject, issue.message);
    }
    run(&mut engine).map_err(SessionError::Engine)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn temp_dir() -> TempDir {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "synap-node-library-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        TempDir(path)
    }

    #[test]
    fn a_plugin_installed_through_the_library_is_usable_and_survives_restart() {
        let dir = temp_dir();
        let session = open_session(&dir.0, "test").expect("open");
        let library = session.library;
        let guide = synap_runtime::AUTHORING_GUIDE;

        let checked = library.plugin(guide, false).expect("check");
        assert_eq!(checked["ok"], true, "{checked}");
        assert!(library
            .create_entity("me.course/grade@1", serde_json::json!({}))
            .is_err());

        let installed = library.plugin(guide, true).expect("install");
        assert_eq!(installed["installed"], true, "{installed}");
        let id = library
            .create_entity(
                "me.course/grade@1",
                serde_json::json!({ "title": "algebra", "score": 42, "credits": 3 }),
            )
            .expect("create after install");
        assert_eq!(library.rows("me.course/failing", 10).unwrap().len(), 1);
        library
            .call_entity(
                &id,
                "me.course/Graded@1",
                "rescore",
                serde_json::json!({ "score": 95 }),
            )
            .expect("call");
        assert_eq!(library.get_entity(&id).unwrap()["fields"]["score"], 95.0);

        // 同 id 再装一次覆盖同一个文件。
        let edited = guide.replace("note = \"text?\"", "note = \"text?\", room = \"text?\"");
        assert_eq!(library.plugin(&edited, true).unwrap()["installed"], true);
        let files = std::fs::read_dir(dir.0.join("plugins")).unwrap().count();
        assert_eq!(files, 1);
        drop(library);

        let session = open_session(&dir.0, "test").expect("reopen");
        let described = session.library.describe().unwrap();
        let grade = described["schemas"]
            .as_array()
            .unwrap()
            .iter()
            .find(|schema| schema["id"] == "me.course/grade@1")
            .cloned()
            .expect("installed plugin is loaded on restart");
        assert_eq!(grade["entities"], 1);
        assert_eq!(grade["fields"]["room"], "text?");
    }
}
