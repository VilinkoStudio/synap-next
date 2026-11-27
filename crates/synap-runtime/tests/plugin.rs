use std::collections::BTreeMap;

use synap_core::{Block, BlockHash, BlockHeader, CanonicalValue, EntityId, Hlc, SchemaId, TraitId};
use synap_runtime::{Plugin, PluginError, Registry, HOST_API, SELF_PARTITION, STDLIB};

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
  extract = function(r)
    return { done = r.status == "done" }
  end,
  set_done = function(r, a)
    r.status = a.done and "done" or "open"
    return r
  end,
})

schema "vilinko.note/page" {
  compose = {
    title = "std/Title@1",
    body  = "std/Text@1",
    check = "std/Check@1?",
  },
}
"#;

fn registry(extra: &str) -> Registry {
    Registry::load([STDLIB.as_bytes(), extra.as_bytes()]).expect("plugins load")
}

fn name(text: &str) -> synap_runtime::VersionedName {
    synap_runtime::VersionedName::parse(text).expect("versioned name")
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

fn int(value: i64) -> CanonicalValue {
    CanonicalValue::Int(value)
}

fn at(ms: u64) -> CanonicalValue {
    CanonicalValue::Hlc(Hlc {
        physical_ms: ms,
        logical: 0,
        node_id: 1,
    })
}

fn field<'a>(value: &'a CanonicalValue, name: &str) -> &'a CanonicalValue {
    value.get(name).expect("field exists")
}

fn partition<'a>(payload: &'a CanonicalValue, name: &str) -> &'a CanonicalValue {
    payload
        .get(name)
        .unwrap_or_else(|| panic!("partition `{name}` missing in {payload:?}"))
}

#[test]
fn constructs_a_todo_from_components_and_reads_its_traits() {
    let registry = registry(TODO);
    let todo = name("vilinko.todo/item@1");
    let payload = registry
        .construct(
            &todo,
            &map(&[
                ("title", text("写设计")),
                ("priority", int(3)),
                ("due", at(1_700_000_000_000)),
                ("note", CanonicalValue::Bool(true)),
                ("text", text("正文")),
            ]),
        )
        .expect("construct works");

    assert_eq!(
        field(partition(&payload, "title"), "title"),
        &text("写设计")
    );
    assert_eq!(
        field(partition(&payload, "check"), "done"),
        &CanonicalValue::Bool(false)
    );
    assert_eq!(
        field(partition(&payload, "due"), "due"),
        &at(1_700_000_000_000)
    );
    assert_eq!(field(partition(&payload, "note"), "body"), &text("正文"));
    assert_eq!(
        field(partition(&payload, SELF_PARTITION), "priority"),
        &int(3)
    );
    assert_eq!(
        field(partition(&payload, SELF_PARTITION), "status"),
        &text("open")
    );

    let titled = registry
        .extract(&todo, &trait_id("std/Titled@1"), &payload)
        .expect("titled")
        .expect("has title");
    assert_eq!(field(&titled, "title"), &text("写设计"));

    let temporal = registry
        .extract(&todo, &trait_id("std/Temporal@1"), &payload)
        .expect("temporal")
        .expect("has due");
    assert_eq!(field(&temporal, "due"), &at(1_700_000_000_000));

    // 显式 Impl 优先于 Check 组件的委托。
    let checkable = registry
        .extract(&todo, &trait_id("std/Checkable@1"), &payload)
        .expect("checkable")
        .expect("has status");
    assert_eq!(field(&checkable, "done"), &CanonicalValue::Bool(false));

    let done = registry
        .call(
            &todo,
            &trait_id("std/Checkable@1"),
            "set_done",
            &payload,
            &map(&[("done", CanonicalValue::Bool(true))]),
        )
        .expect("write works");
    assert_eq!(
        field(partition(&done, SELF_PARTITION), "status"),
        &text("done")
    );
    let checkable = registry
        .extract(&todo, &trait_id("std/Checkable@1"), &done)
        .expect("checkable")
        .expect("has status");
    assert_eq!(field(&checkable, "done"), &CanonicalValue::Bool(true));
}

#[test]
fn optional_components_grow_and_shrink_the_instance() {
    let registry = registry(TODO);
    let todo = name("vilinko.todo/item@1");
    let bare = registry
        .construct(&todo, &map(&[("title", text("只有标题"))]))
        .expect("construct");
    assert!(registry
        .extract(&todo, &trait_id("std/Temporal@1"), &bare)
        .expect("query")
        .is_none());
    assert!(registry
        .extract(&todo, &trait_id("std/HasText@1"), &bare)
        .expect("query")
        .is_none());
    assert!(bare.get("due").is_none());
    assert!(bare.get("note").is_none());
}

#[test]
fn delegated_writes_only_touch_the_component_they_came_from() {
    let registry = registry(TODO);
    let note = name("vilinko.note/page@1");
    let payload = registry
        .construct(
            &note,
            &map(&[
                ("title", text("会议记录")),
                ("body", text("……")),
                ("check", CanonicalValue::Bool(true)),
            ]),
        )
        .expect("construct");

    let checkable = registry
        .extract(&note, &trait_id("std/Checkable@1"), &payload)
        .expect("checkable")
        .expect("check component present");
    assert_eq!(field(&checkable, "done"), &CanonicalValue::Bool(false));

    let done = registry
        .call(
            &note,
            &trait_id("std/Checkable@1"),
            "set_done",
            &payload,
            &map(&[("done", CanonicalValue::Bool(true))]),
        )
        .expect("delegated write");
    assert_eq!(
        field(partition(&done, "check"), "done"),
        &CanonicalValue::Bool(true)
    );
    // 正文分区没被碰。
    assert_eq!(field(partition(&done, "body"), "body"), &text("……"));

    let retitled = registry
        .call(
            &note,
            &trait_id("std/Titled@1"),
            "set_title",
            &done,
            &map(&[("title", text("周会"))]),
        )
        .expect("delegated write");
    assert_eq!(field(partition(&retitled, "title"), "title"), &text("周会"));
    assert_eq!(
        field(partition(&retitled, "check"), "done"),
        &CanonicalValue::Bool(true)
    );
}

#[test]
fn an_absent_optional_component_cannot_be_written() {
    let registry = registry(TODO);
    let note = name("vilinko.note/page@1");
    let payload = registry
        .construct(&note, &map(&[("title", text("x")), ("body", text("y"))]))
        .expect("construct");
    let error = registry
        .call(
            &note,
            &trait_id("std/Checkable@1"),
            "set_done",
            &payload,
            &map(&[("done", CanonicalValue::Bool(true))]),
        )
        .expect_err("no check component");
    assert!(matches!(error, PluginError::ComponentAbsent { .. }));
}

#[test]
fn unknown_partitions_survive_a_write() {
    let registry = registry(TODO);
    let note = name("vilinko.note/page@1");
    let payload = registry
        .construct(&note, &map(&[("title", text("x")), ("body", text("y"))]))
        .expect("construct");
    let mut partitions = payload.as_map().expect("map").clone();
    partitions.insert("me.plan/estimate".to_owned(), map(&[("minutes", int(30))]));
    let payload = CanonicalValue::Map(partitions);

    let retitled = registry
        .call(
            &note,
            &trait_id("std/Titled@1"),
            "set_title",
            &payload,
            &map(&[("title", text("z"))]),
        )
        .expect("write");
    assert_eq!(
        field(partition(&retitled, "me.plan/estimate"), "minutes"),
        &int(30)
    );
}

#[test]
fn write_arguments_are_checked_against_the_trait_signature() {
    let registry = registry(TODO);
    let note = name("vilinko.note/page@1");
    let payload = registry
        .construct(&note, &map(&[("title", text("x")), ("body", text("y"))]))
        .expect("construct");

    let missing = registry
        .call(
            &note,
            &trait_id("std/Titled@1"),
            "set_title",
            &payload,
            &map(&[]),
        )
        .expect_err("missing argument");
    assert!(matches!(missing, PluginError::MissingField { .. }));

    let unknown = registry
        .call(
            &note,
            &trait_id("std/Titled@1"),
            "set_title",
            &payload,
            &map(&[("title", text("x")), ("extra", int(1))]),
        )
        .expect_err("unknown argument");
    assert!(matches!(unknown, PluginError::UnknownField { .. }));

    let method = registry
        .call(
            &note,
            &trait_id("std/Titled@1"),
            "set_nothing",
            &payload,
            &map(&[]),
        )
        .expect_err("unknown method");
    assert!(matches!(method, PluginError::UnknownMethod { .. }));
}

#[test]
fn context_membership_is_deduplicated() {
    let registry = registry(TODO);
    let context = name("std/context@1");
    let member = EntityId::new();
    let payload = registry
        .construct(&context, &map(&[("title", text("日本旅行"))]))
        .expect("construct");
    let add = map(&[("member", CanonicalValue::Ref(member))]);
    let once = registry
        .call(
            &context,
            &trait_id("std/Context@1"),
            "add_member",
            &payload,
            &add,
        )
        .expect("add");
    let twice = registry
        .call(
            &context,
            &trait_id("std/Context@1"),
            "add_member",
            &once,
            &add,
        )
        .expect("add again");
    let members = registry
        .extract(&context, &trait_id("std/Context@1"), &twice)
        .expect("extract")
        .expect("has members");
    assert_eq!(
        members.get("members"),
        Some(&CanonicalValue::List(vec![CanonicalValue::Ref(member)]))
    );
}

#[test]
fn plugin_blocks_load_with_the_same_path() {
    let block = Block {
        header: BlockHeader {
            entity_id: EntityId::new(),
            parents: Vec::new(),
            schema_id: SchemaId::parse("sys/plugin").unwrap(),
            schema_version: BlockHash::ZERO,
            hlc: Hlc {
                physical_ms: 0,
                logical: 0,
                node_id: 0,
            },
            is_tombstone: false,
        },
        payload: STDLIB.as_bytes().to_vec(),
    };
    let plugin = Plugin::from_block(&block).expect("plugin block");
    assert_eq!(plugin.id(), "std");
    assert_eq!(plugin.api(), HOST_API);

    let mut wrong = block.clone();
    wrong.header.schema_id = SchemaId::parse("std/context").unwrap();
    assert!(Plugin::from_block(&wrong).is_err());
}

#[test]
fn a_bad_host_api_is_rejected() {
    let error = Registry::load([b"plugin \"old\" { api = 99 }".as_slice()]).expect_err("old api");
    assert!(matches!(error, PluginError::HostApi { found: 99, .. }));
}

#[test]
fn field_collisions_are_rejected_at_load() {
    let source = r#"
plugin "test.dup" { api = 1 }
schema "test.dup/x" {
  compose = { title = "std/Title@1" },
  fields = { title = "text" },
}
"#;
    let error = Registry::load([STDLIB.as_bytes(), source.as_bytes()]).expect_err("collision");
    assert!(
        matches!(error, PluginError::DuplicateField { .. }),
        "{error:?}"
    );
}

#[test]
fn ambiguous_delegation_is_rejected_at_load() {
    let source = r#"
plugin "test.ambiguous" { api = 1 }
component "test.a/One@1" {
  fields = { title = "text" },
  impl = { "std/Titled@1" },
  construct = function(args) return { title = args.title } end,
}
component "test.a/Two@1" {
  fields = { other = "text" },
  impl = { "std/Titled@1" },
  construct = function(args) return { other = args.title } end,
}
schema "test.ambiguous/x" {
  compose = { a = "test.a/One@1", b = "test.a/Two@1" },
}
"#;
    let error = Registry::load([STDLIB.as_bytes(), source.as_bytes()]).expect_err("ambiguous");
    assert!(
        matches!(error, PluginError::AmbiguousDelegate { .. }),
        "{error:?}"
    );
}

#[test]
fn the_sandbox_stops_runaway_plugins() {
    let source = r#"
plugin "test.loop" { api = 1 }
schema "test.loop/x" {
  construct = function(args) while true do end end,
}
"#;
    let registry = Registry::load([source.as_bytes()]).expect("load");
    let error = registry
        .construct(&name("test.loop/x@1"), &map(&[]))
        .expect_err("loop must stop");
    assert!(
        matches!(error, PluginError::Preempted),
        "unexpected error: {error}"
    );
}

#[test]
fn the_sandbox_has_no_ambient_io_or_time() {
    let source = r#"
plugin "test.os" { api = 1 }
schema "test.os/x" {
  construct = function(args) return { t = os.time() } end,
}
"#;
    let registry = Registry::load([source.as_bytes()]).expect("load");
    let error = registry
        .construct(&name("test.os/x@1"), &map(&[]))
        .expect_err("os is gone");
    assert!(
        error.to_string().contains("os"),
        "unexpected error: {error}"
    );
}

#[test]
fn traits_declare_their_standard_shapes() {
    let registry = registry(TODO);
    let mut shapes = BTreeMap::new();
    for (_, def) in registry.traits() {
        shapes.insert(def.id.to_string(), def.read.len());
    }
    assert_eq!(shapes.get("std/Checkable@1"), Some(&1));
    assert_eq!(shapes.get("std/Context@1"), Some(&1));
    assert!(shapes.contains_key("std/Temporal@1"));
}

#[test]
fn pairs_is_deterministic() {
    let source = r#"
plugin "test.order" { api = 1 }
schema "test.order/x" {
  fields = { order = "[string]" },
  construct = function(args, base)
    local keys = {}
    local source = { b = 1, a = 2, c = 3 }
    for key in pairs(source) do
      table.insert(keys, key)
    end
    base.order = keys
    return base
  end,
}
"#;
    let registry = Registry::load([source.as_bytes()]).expect("load");
    let payload = registry
        .construct(&name("test.order/x@1"), &map(&[]))
        .expect("construct");
    assert_eq!(
        field(partition(&payload, SELF_PARTITION), "order"),
        &CanonicalValue::List(vec![text("a"), text("b"), text("c")])
    );
}

#[test]
fn plugins_cannot_assign_globals() {
    let source = r#"
plugin "test.globals" { api = 1 }
schema "test.globals/x" {
  fields = { n = "int" },
  construct = function(args, base)
    counter = (counter or 0) + 1
    base.n = counter
    return base
  end,
}
"#;
    let registry = Registry::load([source.as_bytes()]).expect("load");
    let error = registry
        .construct(&name("test.globals/x@1"), &map(&[]))
        .expect_err("global write must fail");
    assert!(
        error.to_string().contains("cannot assign global"),
        "unexpected error: {error}"
    );
}

#[test]
fn definitions_are_shareable_and_replicas_agree() {
    use synap_runtime::{Definitions, Task, TaskOutcome};

    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Definitions>();

    let definitions = std::sync::Arc::new(
        Definitions::load([STDLIB.as_bytes(), TODO.as_bytes()]).expect("definitions load"),
    );
    assert_eq!(definitions.plugins().len(), 2);
    let shared_a = synap_runtime::Registry::from_shared(definitions.clone()).expect("first");
    let shared_b = synap_runtime::Registry::from_shared(definitions).expect("second");
    let payload_a = shared_a
        .construct(&name("vilinko.todo/item@1"), &args_for("共享", 5))
        .expect("a");
    let payload_b = shared_b
        .construct(&name("vilinko.todo/item@1"), &args_for("共享", 5))
        .expect("b");
    assert_eq!(payload_a, payload_b);

    // 同一个源码在两套解释器里构造同一实体，结果必须一致。
    let first = registry(TODO)
        .construct(&name("vilinko.todo/item@1"), &args_for("同一条", 7))
        .expect("first");
    let second = registry(TODO)
        .construct(&name("vilinko.todo/item@1"), &args_for("同一条", 7))
        .expect("second");
    assert_eq!(first, second);

    // 任务描述可以跨线程；组件 construct 只吃一个参数表。
    let registry = registry(TODO);
    let title = name("std/Title@1");
    let (plugin, function) = registry
        .components()
        .find(|(_, def)| def.id == title)
        .map(|(plugin, def)| (plugin.id().to_owned(), def.construct.clone()))
        .expect("title component");
    let task = Task {
        plugin,
        function,
        args: vec![map(&[("title", text("任务"))])],
        budget: synap_runtime::Budget::small(),
    };
    match registry.run_task(&task).expect("task runs") {
        TaskOutcome::Value(value) => {
            assert_eq!(value.get("title"), Some(&text("任务")));
        }
        TaskOutcome::Preempted => panic!("small construct should fit"),
    }
}

fn args_for(title: &str, due: u64) -> CanonicalValue {
    map(&[
        ("title", text(title)),
        ("due", at(due)),
        ("status", text("open")),
    ])
}

#[test]
fn a_task_can_be_preempted_by_its_budget() {
    use synap_runtime::{Budget, Task, TaskOutcome};

    let source = r#"
plugin "test.spin" { api = 1 }
component "test.spin/Spin@1" {
  fields = { n = "int" },
  construct = function(args) while true do end end,
}
"#;
    let registry = Registry::load([source.as_bytes()]).expect("load");
    let spin = name("test.spin/Spin@1");
    let (plugin, function) = registry
        .components()
        .find(|(_, def)| def.id == spin)
        .map(|(plugin, def)| (plugin.id().to_owned(), def.construct.clone()))
        .expect("spin component");
    let task = Task {
        plugin,
        function,
        args: Vec::new(),
        budget: Budget::small(),
    };
    assert!(matches!(
        registry.run_task(&task).expect("task runs"),
        TaskOutcome::Preempted
    ));
}

#[test]
fn runtime_helpers_stay_available_after_the_declaration_phase() {
    let source = r#"
plugin "test.ref" { api = 1 }
schema "test.ref/x" {
  fields = { target = "ref" },
  construct = function(args, base)
    base.target = synap.ref(args.id)
    return base
  end,
}
"#;
    let registry = Registry::load([source.as_bytes()]).expect("load");
    let target = synap_core::EntityId::new();
    let payload = registry
        .construct(
            &name("test.ref/x@1"),
            &map(&[("id", text(&target.to_string()))]),
        )
        .expect("construct");
    assert_eq!(
        field(partition(&payload, SELF_PARTITION), "target"),
        &CanonicalValue::Ref(target)
    );
}

#[test]
fn standard_library_tables_are_read_only() {
    let source = r#"
plugin "test.freeze" { api = 1 }
schema "test.freeze/x" {
  fields = { n = "int" },
  construct = function(args, base)
    string.bad = "x"
    base.n = 1
    return base
  end,
}
"#;
    let registry = Registry::load([source.as_bytes()]).expect("load");
    let error = registry
        .construct(&name("test.freeze/x@1"), &map(&[]))
        .expect_err("mutation must fail");
    assert!(
        error.to_string().contains("read-only"),
        "unexpected error: {error}"
    );
}
