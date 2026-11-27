use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use synap_core::{CanonicalValue, Hlc, LocalSeq, TraitId};
use synap_runtime::{Engine, VersionedName, STDLIB};
use synap_store::{KeyWindow, MasterCustody, ProjChange, ScanDir, Volume};

const TODO: &str = r#"
plugin "vilinko.todo" { api = 1 }

schema "vilinko.todo/item" {
  compose = {
    title = "std/Title@1",
    check = "std/Check@1",
    due   = "std/Time@1?",
    note  = "std/Text@1?",
  },
  fields = { priority = "u8", status = "text" },
  merge = { priority = "lww", status = "lww" },
  construct = function(args, base)
    base.priority = args.priority or 0
    base.status = args.status or "open"
    return base
  end,
}

impl("std/Checkable@1", "vilinko.todo/item@1", {
  extract = function(r) return { done = r.status == "done" } end,
  set_done = function(r, a) r.status = a.done and "done" or "open"; return r end,
})

projection "vilinko.todo/open_by_due" {
  from("std/Checkable@1")
    :filter(function(t) return not t.Checkable.done end)
    :map(function(t)
      local due = t.Temporal and t.Temporal.due
      local title = t.Titled and t.Titled.title or ""
      return { due and due.physical_ms or 0, title },
             { id = t.id, title = title, due = due and due.physical_ms or nil }
    end)
    :index()
}
"#;

struct TempFile {
    path: PathBuf,
}

impl TempFile {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "synap-engine-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        Self { path }
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn open_engine(path: &Path) -> Engine {
    open_engine_workers(path, 1)
}

fn open_engine_workers(path: &Path, workers: usize) -> Engine {
    let created = Volume::create(path, MasterCustody::Embedded).expect("create volume");
    Engine::open_with_workers(
        created.volume,
        &[STDLIB.as_bytes(), TODO.as_bytes()],
        7,
        workers,
    )
    .expect("open engine")
}

fn name(text: &str) -> VersionedName {
    VersionedName::parse(text).expect("versioned name")
}

fn trait_id(text: &str) -> TraitId {
    let (name, major) = text.rsplit_once('@').expect("@major");
    TraitId::parse(name, major.parse().expect("major")).expect("trait id")
}

fn map(pairs: &[(&str, CanonicalValue)]) -> CanonicalValue {
    CanonicalValue::Map(
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), value.clone()))
            .collect(),
    )
}

fn text(value: &str) -> CanonicalValue {
    CanonicalValue::String(value.to_owned())
}

fn at(ms: u64) -> CanonicalValue {
    CanonicalValue::Hlc(Hlc {
        physical_ms: ms,
        logical: 0,
        node_id: 7,
    })
}

fn args(title: &str, due_ms: u64, status: &str) -> CanonicalValue {
    map(&[
        ("title", text(title)),
        ("due", at(due_ms)),
        ("status", text(status)),
    ])
}

fn rows(engine: &Engine, namespace: &synap_core::ProjNamespace) -> Vec<CanonicalValue> {
    let snapshot = engine.volume().snapshot().expect("snapshot");
    snapshot
        .projection_range(namespace, &KeyWindow::All, ScanDir::Forward, None, 16)
        .expect("range")
        .rows
        .into_iter()
        .map(|row| CanonicalValue::decode(&row.value).expect("value decodes"))
        .collect()
}

fn drain(watch: &synap_store::Watch) -> Vec<ProjChange> {
    let mut changes = Vec::new();
    while let Some(event) = watch.try_recv() {
        changes.extend(event.changes);
    }
    changes
}

fn block_count(volume: &Volume) -> usize {
    volume
        .snapshot()
        .expect("snapshot")
        .since(LocalSeq::ZERO)
        .expect("since")
        .count()
}

#[test]
fn create_inserts_and_projects_rows() {
    let file = TempFile::new();
    let mut engine = open_engine(&file.path);
    let todo = name("vilinko.todo/item@1");
    let namespace = engine
        .projection_namespace("vilinko.todo/open_by_due")
        .expect("projection exists");
    let watch = engine
        .volume()
        .watch(namespace.clone(), KeyWindow::All)
        .expect("watch");

    let first = engine
        .create(&todo, &args("写投影", 200, "open"))
        .expect("create first");
    let second = engine
        .create(&todo, &args("已完成的", 100, "done"))
        .expect("create second");
    assert_ne!(first, second);

    let projected = rows(&engine, &namespace);
    assert_eq!(projected.len(), 1, "done row must be filtered out");
    assert_eq!(projected[0].get("title"), Some(&text("写投影")));
    assert_eq!(projected[0].get("due"), Some(&CanonicalValue::Int(200)));

    let changes = drain(&watch);
    assert!(
        changes
            .iter()
            .any(|change| matches!(change, ProjChange::Put { .. })),
        "watch should see the insert: {changes:?}"
    );

    engine
        .write(
            first,
            &trait_id("std/Checkable@1"),
            "set_done",
            &map(&[("done", CanonicalValue::Bool(true))]),
        )
        .expect("finish");
    assert!(
        rows(&engine, &namespace).is_empty(),
        "finished row must leave"
    );
    let changes = drain(&watch);
    assert!(
        changes
            .iter()
            .any(|change| matches!(change, ProjChange::Delete { .. })),
        "watch should see the delete: {changes:?}"
    );
}

#[test]
fn key_changes_delete_the_old_row_and_order_by_key() {
    let file = TempFile::new();
    let mut engine = open_engine(&file.path);
    let todo = name("vilinko.todo/item@1");
    let namespace = engine
        .projection_namespace("vilinko.todo/open_by_due")
        .expect("projection exists");

    let early = engine
        .create(&todo, &args("先做", 100, "open"))
        .expect("early");
    engine
        .create(&todo, &args("后做", 200, "open"))
        .expect("late");

    let projected = rows(&engine, &namespace);
    assert_eq!(projected.len(), 2);
    assert_eq!(projected[0].get("title"), Some(&text("先做")));
    assert_eq!(projected[1].get("title"), Some(&text("后做")));

    // 改标题会改 key：旧 key 删除，新 key 写入。
    engine
        .write(
            early,
            &trait_id("std/Titled@1"),
            "set_title",
            &map(&[("title", text("最早"))]),
        )
        .expect("retitle");
    let projected = rows(&engine, &namespace);
    assert_eq!(projected.len(), 2);
    assert_eq!(projected[0].get("title"), Some(&text("最早")));

    // 改 due 会换位置。
    engine
        .write(
            early,
            &trait_id("std/Temporal@1"),
            "set_time",
            &map(&[("due", at(300))]),
        )
        .expect("reschedule");
    let projected = rows(&engine, &namespace);
    assert_eq!(projected[0].get("title"), Some(&text("后做")));
    assert_eq!(projected[1].get("title"), Some(&text("最早")));
}

#[test]
fn reopening_reuses_plugin_blocks_and_projection_checkpoint() {
    let file = TempFile::new();
    let mut engine = open_engine(&file.path);
    let todo = name("vilinko.todo/item@1");
    let namespace = engine
        .projection_namespace("vilinko.todo/open_by_due")
        .expect("projection exists");
    engine
        .create(&todo, &args("保留", 50, "open"))
        .expect("create");
    let before = block_count(engine.volume());
    let version = engine
        .plugin_version("vilinko.todo")
        .expect("plugin version");
    drop(engine);

    let volume = Volume::open(&file.path, None).expect("reopen volume");
    let engine =
        Engine::open(volume, &[STDLIB.as_bytes(), TODO.as_bytes()], 7).expect("reopen engine");
    assert_eq!(
        block_count(engine.volume()),
        before,
        "reopen must not write plugin blocks"
    );
    assert_eq!(engine.plugin_version("vilinko.todo"), Some(version));
    assert_eq!(rows(&engine, &namespace).len(), 1);
}

#[test]
fn projection_rows_run_on_the_worker_pool() {
    let file = TempFile::new();
    let mut engine = open_engine_workers(&file.path, 2);
    assert_eq!(engine.worker_count(), 2);
    let todo = name("vilinko.todo/item@1");
    for (index, due) in [400_u64, 100, 300, 200].into_iter().enumerate() {
        engine
            .create(&todo, &args(&format!("T{index}"), due, "open"))
            .expect("create");
    }
    let namespace = engine
        .projection_namespace("vilinko.todo/open_by_due")
        .expect("projection exists");
    let projected = rows(&engine, &namespace);
    assert_eq!(projected.len(), 4);
    let titles: Vec<_> = projected
        .iter()
        .map(|row| row.get("title").expect("title"))
        .collect();
    assert_eq!(
        titles,
        vec![&text("T1"), &text("T3"), &text("T2"), &text("T0")]
    );
}
