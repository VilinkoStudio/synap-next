use std::sync::LazyLock;
use synap_core::{Record, RecordId, Version};
use synap_runtime::{Action, Runtime};
use synap_store::MemKv;

/// 打通链路用的最小单例：内存引擎 + 本机 device 名。
/// 文件引擎（redb）和多设备同步以后再接，先不断 bridge 的 API。
static BRIDGE: LazyLock<Runtime<MemKv>> =
    LazyLock::new(|| Runtime::new(MemKv::new(), "local".to_string()));

fn runtime() -> &'static Runtime<MemKv> {
    &BRIDGE
}

/// flutter_rust_bridge codegen 入口：普通 pub fn 即可被扫到，
/// 不需要手写 extern C。Stream 投影下一步再加，先把动作通路跑通。
pub fn bridge_ping() -> String {
    "synap-bridge ok".to_string()
}

pub fn bridge_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// 最小写入：把 JSON 载荷经 runtime 写成中层记录，返回版本。
/// payload 是 `Record` 的 JSON（kind + payload 由插件 schema 解释）。
pub fn bridge_put_record(payload_json: String) -> Result<String, String> {
    let mut record: Record = serde_json::from_str(&payload_json).map_err(|e| e.to_string())?;
    if record.id.0.is_nil() {
        record.id = RecordId::new();
    }
    let action = Action {
        plugin: "demo".to_string(),
        name: "put_record".to_string(),
        payload: serde_json::to_vec(&record).map_err(|e| e.to_string())?,
    };
    let result = runtime().dispatch(action).map_err(|e| e.to_string())?;
    let version: Version = result.version;
    serde_json::to_string(&version).map_err(|e| e.to_string())
}
