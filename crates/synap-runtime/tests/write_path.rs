use synap_core::{Kv, Record, RecordId, Version};
use synap_runtime::{Action, Runtime};
use synap_store::MemKv;

fn put_action(record: &Record) -> Action {
    Action {
        plugin: "demo".to_string(),
        name: "put_record".to_string(),
        payload: serde_json::to_vec(record).expect("record encodes"),
    }
}

fn demo_record() -> Record {
    Record {
        id: RecordId::new(),
        version: Version {
            seq: 0,
            device: String::new(),
        },
        kind: "demo.note".to_string(),
        payload: b"hello".to_vec(),
    }
}

#[test]
fn put_then_get_round_trip() {
    let rt = Runtime::new(MemKv::new(), "test-device".to_string());
    let record = demo_record();

    let put_result = rt.dispatch(put_action(&record)).expect("put works");
    assert_eq!(put_result.version.seq, 1);
    assert_eq!(put_result.version.device, "test-device");

    let get = Action {
        plugin: "demo".to_string(),
        name: "get_record".to_string(),
        payload: serde_json::to_vec(&record.id).expect("id encodes"),
    };
    let get_result = rt.dispatch(get).expect("get works");
    assert_eq!(get_result.record_id, put_result.record_id);
    assert_eq!(get_result.version.seq, 1);
}

#[test]
fn put_twice_bumps_version() {
    let rt = Runtime::new(MemKv::new(), "d".to_string());
    let record = demo_record();
    let first = rt.dispatch(put_action(&record)).expect("put 1");
    let second = rt.dispatch(put_action(&record)).expect("put 2");
    assert_eq!(first.version.seq + 1, second.version.seq);
}

#[test]
fn mem_kv_put_get_scan() {
    let kv = MemKv::new();
    kv.put(b"idx/demo/a", b"rec/1").expect("put");
    kv.put(b"idx/demo/b", b"rec/2").expect("put");
    kv.put(b"other", b"x").expect("put");
    let scanned = kv.scan_prefix(b"idx/demo/").expect("scan");
    assert_eq!(scanned.len(), 2);
    assert_eq!(kv.get(b"idx/demo/a").expect("get"), Some(b"rec/1".to_vec()));
}
