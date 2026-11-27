//! 审查里复现过的问题：坏插件、坏实体不能让库打不开或卡住。

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use synap_core::CanonicalValue;
use synap_runtime::{Engine, PluginError, Registry, VersionedName, STDLIB};
use synap_store::{KeyWindow, MasterCustody, ScanDir, Volume};

struct TempFile(PathBuf);

impl TempFile {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "synap-resilience-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        Self(path)
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn name(text: &str) -> VersionedName {
    VersionedName::parse(text).expect("versioned name")
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

fn construct_err(body: &str) -> PluginError {
    let source = format!(
        "plugin \"test.x\" {{ api = 1 }}\nschema \"test.x/x\" {{\n  construct = function(args)\n{body}\n  end,\n}}\n"
    );
    let registry = Registry::load([source.as_bytes()]).expect("load");
    registry
        .construct(&name("test.x/x@1"), &map(&[]))
        .expect_err("must fail")
}

#[test]
fn coroutines_are_not_available() {
    let error =
        construct_err("coroutine.resume(coroutine.create(function() while true do end end))");
    assert!(error.to_string().contains("coroutine"), "{error}");
}

#[test]
fn pcall_cannot_swallow_the_budget() {
    let error = construct_err("while true do pcall(function() while true do end end) end");
    assert!(matches!(error, PluginError::Preempted), "{error}");
}

#[test]
fn cyclic_tables_are_an_error_not_a_crash() {
    let source = r#"
plugin "test.cycle" { api = 1 }
schema "test.cycle/x" {
  fields = { data = "any" },
  construct = function(args)
    local t = {}
    t.self = t
    return { data = t }
  end,
}
"#;
    let registry = Registry::load([source.as_bytes()]).expect("load");
    let error = registry
        .construct(&name("test.cycle/x@1"), &map(&[]))
        .expect_err("cycle must fail");
    assert!(error.to_string().contains("nested deeper"), "{error}");
}

#[test]
fn metatables_and_addresses_are_locked() {
    let error = construct_err("setmetatable(string, nil)");
    assert!(error.to_string().contains("metatable"), "{error}");
    let error = construct_err("getmetatable(\"\").__index = {}");
    assert!(!error.to_string().is_empty());
    let error = construct_err("local mt = getmetatable(_G); return mt.__index");
    assert!(!error.to_string().is_empty());
    let error = construct_err("error(tostring({}))");
    assert!(!error.to_string().contains("0x"), "{error}");
}

#[test]
fn unknown_field_types_fail_at_load() {
    let source = r#"
plugin "test.typo" { api = 1 }
schema "test.typo/x" { fields = { n = "integer" } }
"#;
    let error = Registry::load([source.as_bytes()]).expect_err("typo must fail");
    assert!(
        error.to_string().contains("unknown type `integer`"),
        "{error}"
    );
}

#[test]
fn std_namespace_is_reserved() {
    let source = r#"
plugin "evil" { api = 1 }
trait "std/Titled@1" { read = { title = "text" } }
"#;
    let error = Registry::load([source.as_bytes()]).expect_err("std is reserved");
    assert!(error.to_string().contains("reserved"), "{error}");
}

#[test]
fn large_integers_are_rejected_instead_of_rounded() {
    let source = r#"
plugin "test.big" { api = 1 }
schema "test.big/x" { fields = { n = "i64", label = "text" } }
impl("test.big/Labeled@1", "test.big/x@1", {
  extract = function(r) return { label = r.label } end,
  relabel = function(r, a) r.label = a.label; return r end,
})
trait "test.big/Labeled@1" {
  read = { label = "text" },
  write = { relabel = { args = { label = "text" } } },
}
"#;
    let registry = Registry::load([STDLIB.as_bytes(), source.as_bytes()]).expect("load");
    let payload = map(&[(
        "$self",
        map(&[
            ("n", CanonicalValue::Int((1 << 53) + 1)),
            ("label", text("a")),
        ]),
    )]);
    let trait_id = synap_core::TraitId::parse("test.big/Labeled", 1).unwrap();
    let error = registry
        .call(
            &name("test.big/x@1"),
            &trait_id,
            "relabel",
            &payload,
            &map(&[("label", text("b"))]),
        )
        .expect_err("2^53+1 must not be silently rounded");
    assert!(error.to_string().contains("2^53"), "{error}");
}

const NOTES: &str = r#"
plugin "test.notes" { api = 1 }
schema "test.notes/note" {
  compose = { title = "std/Title@1" },
  fields = { mood = "text?" },
}
projection "test.notes/titles" {
  from("std/Titled@1")
    :map(function(t)
      if t.Titled.title == "boom" then error("bad row") end
      return { t.Titled.title }, { title = t.Titled.title }
    end)
    :index()
}
"#;

fn titles(engine: &Engine) -> Vec<CanonicalValue> {
    let namespace = engine
        .projection_namespace("test.notes/titles")
        .expect("namespace");
    let snapshot = engine.volume().snapshot().expect("snapshot");
    snapshot
        .projection_range(&namespace, &KeyWindow::All, ScanDir::Forward, None, 100)
        .expect("range")
        .rows
        .into_iter()
        .map(|row| CanonicalValue::decode(&row.value).expect("decode"))
        .collect()
}

#[test]
fn one_bad_row_does_not_stop_the_projection_or_the_vault() {
    let file = TempFile::new();
    let created = Volume::create(&file.0, MasterCustody::Embedded).expect("create");
    let sources = [STDLIB.as_bytes(), NOTES.as_bytes()];
    let mut engine = Engine::open(created.volume, &sources, 1).expect("open");
    let note = name("test.notes/note@1");
    engine
        .create(&note, &map(&[("title", text("a"))]))
        .expect("a");
    engine
        .create(&note, &map(&[("title", text("boom"))]))
        .expect("the write itself succeeds");
    engine
        .create(&note, &map(&[("title", text("c"))]))
        .expect("c");
    assert_eq!(titles(&engine).len(), 2, "the bad row is left out");
    assert!(engine
        .issues()
        .iter()
        .any(|issue| issue.message.contains("bad row")));
    drop(engine);

    let volume = Volume::open(&file.0, None).expect("reopen");
    let engine = Engine::open(volume, &sources, 1).expect("vault still opens");
    assert_eq!(titles(&engine).len(), 2);
}

#[test]
fn a_conflicting_plugin_is_disabled_instead_of_bricking_the_vault() {
    let file = TempFile::new();
    let created = Volume::create(&file.0, MasterCustody::Embedded).expect("create");
    let copycat = NOTES.replace("plugin \"test.notes\"", "plugin \"test.copycat\"");
    let engine = Engine::open(
        created.volume,
        &[STDLIB.as_bytes(), NOTES.as_bytes(), copycat.as_bytes()],
        1,
    )
    .expect("vault opens");
    assert!(engine.plugin_version("test.notes").is_some());
    assert!(engine.plugin_version("test.copycat").is_none());
    assert!(engine
        .issues()
        .iter()
        .any(|issue| issue.subject == "plugin test.copycat"));
}

#[test]
fn editing_a_plugin_upgrades_it_and_keeps_old_data_readable() {
    let file = TempFile::new();
    let created = Volume::create(&file.0, MasterCustody::Embedded).expect("create");
    let mut engine =
        Engine::open(created.volume, &[STDLIB.as_bytes(), NOTES.as_bytes()], 1).expect("open");
    engine
        .create(&name("test.notes/note@1"), &map(&[("title", text("old"))]))
        .expect("create");
    let first = engine.plugin_version("test.notes").unwrap();
    drop(engine);

    // 只加东西：多一个可选字段。
    let edited = NOTES.replace("mood = \"text?\"", "mood = \"text?\", stars = \"u8?\"");
    let volume = Volume::open(&file.0, None).expect("reopen");
    let engine = Engine::open(volume, &[STDLIB.as_bytes(), edited.as_bytes()], 1)
        .expect("an edited plugin is an upgrade, not a conflict");
    assert_ne!(engine.plugin_version("test.notes").unwrap(), first);
    assert_eq!(
        titles(&engine).len(),
        1,
        "data written by the old version still shows"
    );
}

#[test]
fn an_extension_impl_cannot_override_the_owners_delegation() {
    let extension = r#"
plugin "test.ext" { api = 1 }
impl("std/Titled@1", "test.notes/note@1", {
  extract = function(r) return { title = "hijacked" } end,
})
"#;
    let registry =
        Registry::load([STDLIB.as_bytes(), NOTES.as_bytes(), extension.as_bytes()]).expect("load");
    let note = name("test.notes/note@1");
    let payload = registry
        .construct(&note, &map(&[("title", text("mine"))]))
        .expect("construct");
    let titled = registry
        .extract(
            &note,
            &synap_core::TraitId::parse("std/Titled", 1).unwrap(),
            &payload,
        )
        .expect("extract")
        .expect("has title");
    let CanonicalValue::Map(fields) = titled else {
        panic!()
    };
    assert_eq!(fields["title"], text("mine"));
}

// ---- 明文导出与导入 --------------------------------------------------------

const DIARY: &str = r#"
plugin "test.diary" { api = 1 }
schema "test.diary/entry" {
  compose = { title = "std/Title@1", body = "std/Text@1", tags = "std/Tags@1?" },
  fields = { score = "float?", link = "ref?" },
}
"#;

fn temp_dir() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let mut path = std::env::temp_dir();
    path.push(format!(
        "synap-text-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    path
}

fn block_count(engine: &Engine) -> u64 {
    engine.volume().snapshot().unwrap().tip().unwrap().get()
}

#[test]
fn export_then_import_writes_nothing_and_edits_become_new_blocks() {
    use synap_runtime::ImportOptions;
    let file = TempFile::new();
    let dir = temp_dir();
    let created = Volume::create(&file.0, MasterCustody::Embedded).expect("create");
    let mut engine =
        Engine::open(created.volume, &[STDLIB.as_bytes(), DIARY.as_bytes()], 1).expect("open");
    let entry = name("test.diary/entry@1");
    let first = engine
        .create(
            &entry,
            &map(&[("title", text("浅草")), ("text", text("早晨\n多行\n"))]),
        )
        .expect("create");
    engine
        .create(
            &entry,
            &map(&[
                ("title", text("第二天")),
                ("text", text("")),
                ("tags", CanonicalValue::List(vec![text("旅行")])),
            ]),
        )
        .expect("create");

    let report = engine.export_text(&dir).expect("export");
    assert_eq!(report.entities, 2);
    let path = dir
        .join("test.diary")
        .join("entry")
        .join(format!("{first}.md"));
    let written = std::fs::read_to_string(&path).expect("file exists");
    assert!(
        written.ends_with("早晨\n多行\n"),
        "body goes to markdown: {written}"
    );

    let before = block_count(&engine);
    let report = engine
        .import_text(&dir, ImportOptions::default())
        .expect("import");
    assert_eq!(report.writes(), 0, "{report:?}");
    assert_eq!(report.unchanged, 2);
    assert_eq!(block_count(&engine), before, "round trip writes no blocks");

    // 一个 agent 改了正文，删了另一个文件。
    std::fs::write(&path, written.replace("早晨", "傍晚")).unwrap();
    let other = std::fs::read_dir(dir.join("test.diary").join("entry"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|other| *other != path)
        .unwrap();
    std::fs::remove_file(other).unwrap();

    let dry = engine
        .import_text(
            &dir,
            ImportOptions {
                delete_missing: true,
                dry_run: true,
            },
        )
        .expect("dry run");
    assert_eq!((dry.updated, dry.deleted), (1, 1));
    assert_eq!(block_count(&engine), before, "dry run writes nothing");

    let report = engine
        .import_text(&dir, ImportOptions::default())
        .expect("import");
    assert_eq!(
        (report.updated, report.deleted),
        (1, 0),
        "deleting is opt-in"
    );
    let report = engine
        .import_text(
            &dir,
            ImportOptions {
                delete_missing: true,
                dry_run: false,
            },
        )
        .expect("import");
    assert_eq!(report.deleted, 1);
    assert_eq!(block_count(&engine), before + 2);

    let fields = engine
        .registry()
        .fields(&entry, &{
            let snapshot = engine.volume().snapshot().unwrap();
            let heads = snapshot.heads(&first).unwrap();
            assert_eq!(heads.len(), 1, "the edit continues the history");
            CanonicalValue::decode(&snapshot.block(&heads[0]).unwrap().unwrap().payload).unwrap()
        })
        .expect("fields");
    assert_eq!(fields["body"], text("傍晚\n多行\n"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_bad_file_is_reported_and_the_rest_still_import() {
    use synap_runtime::ImportOptions;
    let file = TempFile::new();
    let dir = temp_dir();
    let created = Volume::create(&file.0, MasterCustody::Embedded).expect("create");
    let mut engine =
        Engine::open(created.volume, &[STDLIB.as_bytes(), DIARY.as_bytes()], 1).expect("open");
    std::fs::create_dir_all(dir.join("x")).unwrap();
    std::fs::write(dir.join("x").join("broken.md"), "no frontmatter").unwrap();
    let id = synap_core::EntityId::new();
    std::fs::write(
        dir.join("x").join("new.md"),
        format!(
            "---\n{{\"id\": \"{id}\", \"schema\": \"test.diary/entry@1\", \"body\": \"body.body\",\n \"payload\": {{\"title\": {{\"title\": \"新的\"}}, \"body\": {{}}, \"$self\": {{\"score\": 4.0}}}}}}\n---\n正文"
        ),
    )
    .unwrap();
    let report = engine
        .import_text(&dir, ImportOptions::default())
        .expect("import");
    assert_eq!(report.created, 1, "{report:?}");
    assert_eq!(report.errors.len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn schemas_without_construct_take_their_own_fields_from_args() {
    let source = r#"
plugin "me.course" { api = 1 }
schema "me.course/grade" {
  compose = { title = "std/Title@1" },
  fields = { score = "float", credits = "float", note = "text?" },
}
"#;
    let registry = Registry::load([STDLIB.as_bytes(), source.as_bytes()]).expect("load");
    let grade = name("me.course/grade@1");
    let payload = registry
        .construct(
            &grade,
            &map(&[
                ("title", text("高等数学")),
                ("score", CanonicalValue::Int(58)),
                ("credits", CanonicalValue::Float(4.5)),
            ]),
        )
        .expect("construct");
    let fields = registry.fields(&grade, &payload).expect("fields");
    assert_eq!(fields["score"], CanonicalValue::Float(58.0));
    assert_eq!(fields["credits"], CanonicalValue::Float(4.5));

    let error = registry
        .construct(
            &grade,
            &map(&[("title", text("x")), ("score", CanonicalValue::Int(1))]),
        )
        .expect_err("credits is required");
    assert!(error.to_string().contains("credits"), "{error}");
    let error = registry
        .construct(
            &grade,
            &map(&[
                ("title", text("x")),
                ("score", text("high")),
                ("credits", CanonicalValue::Int(1)),
            ]),
        )
        .expect_err("score must be a number");
    assert!(error.to_string().contains("score"), "{error}");
}

/// 自省给出的写插件说明里那段示例必须真能用。
fn guide_example() -> String {
    synap_runtime::AUTHORING_GUIDE.to_owned()
}

#[test]
fn the_authoring_guide_example_loads_and_works() {
    let file = TempFile::new();
    let created = Volume::create(&file.0, MasterCustody::Embedded).expect("create");
    let example = guide_example();
    let mut engine =
        Engine::open(created.volume, &[STDLIB.as_bytes(), example.as_bytes()], 1).expect("open");
    assert!(engine.issues().is_empty(), "{:?}", engine.issues());
    let grade = name("me.course/grade@1");
    let low = engine
        .create(
            &grade,
            &map(&[
                ("title", text("algebra")),
                ("score", CanonicalValue::Float(42.0)),
                ("credits", CanonicalValue::Float(3.0)),
            ]),
        )
        .expect("create");
    let failing = |engine: &Engine| {
        let namespace = engine
            .projection_namespace("me.course/failing")
            .expect("ns");
        engine
            .volume()
            .snapshot()
            .unwrap()
            .projection_range(&namespace, &KeyWindow::All, ScanDir::Forward, None, 10)
            .unwrap()
            .rows
            .len()
    };
    assert_eq!(failing(&engine), 1);
    let graded = synap_core::TraitId::parse("me.course/Graded", 1).unwrap();
    engine
        .write(
            low,
            &graded,
            "rescore",
            &map(&[("score", CanonicalValue::Float(90.0))]),
        )
        .expect("rescore");
    assert_eq!(failing(&engine), 0);

    let entity = engine.get_entity(low).expect("get");
    assert_eq!(entity["fields"]["score"], serde_json::json!(90.0));
    assert_eq!(
        entity["traits"]["me.course/Graded@1"]["score"],
        serde_json::json!(90.0)
    );
    assert_eq!(entity["history"].as_array().unwrap().len(), 2);

    let described = engine.describe().expect("describe");
    let schema = described["schemas"]
        .as_array()
        .unwrap()
        .iter()
        .find(|schema| schema["id"] == "me.course/grade@1")
        .expect("schema listed");
    assert_eq!(schema["entities"], 1);
    assert_eq!(schema["fields"]["note"], "text?");
    assert!(described["projections"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["name"] == "me.course/failing"));
}

#[test]
fn checking_a_plugin_reports_entities_it_can_no_longer_read() {
    let file = TempFile::new();
    let created = Volume::create(&file.0, MasterCustody::Embedded).expect("create");
    let example = guide_example();
    let mut engine =
        Engine::open(created.volume, &[STDLIB.as_bytes(), example.as_bytes()], 1).expect("open");
    engine
        .create(
            &name("me.course/grade@1"),
            &map(&[
                ("title", text("algebra")),
                ("score", CanonicalValue::Float(42.0)),
                ("credits", CanonicalValue::Float(3.0)),
            ]),
        )
        .expect("create");

    let fine = engine
        .check_plugin(
            example
                .replace("note = \"text?\"", "note = \"text?\", room = \"text?\"")
                .as_bytes(),
        )
        .unwrap();
    assert_eq!(fine["ok"], true, "{fine}");
    assert_eq!(fine["existing_entities_readable"], 1);

    let broken = example.replace(
        "return { score = r.score }",
        "return { score = r.missing.score }",
    );
    let report = engine.check_plugin(broken.as_bytes()).unwrap();
    assert_eq!(report["ok"], false, "{report}");
    assert_eq!(
        report["existing_entities_failing"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let syntax = engine
        .check_plugin(b"plugin \"me.course\" { api = 1 ")
        .unwrap();
    assert_eq!(syntax["ok"], false);
    assert!(syntax["error"].is_string());
}
