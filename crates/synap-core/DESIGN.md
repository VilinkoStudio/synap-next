# synap-core 设计：共享契约

core 是依赖图的底：零重度依赖，只放纯数据结构与基础 Trait/错误定义。上层（store、runtime、bridge）都从这里拿词汇，不准各自发明同义词。

## Crate 映射

| 设计书 | 本仓库 | 职责 |
| --- | --- | --- |
| `db-core` | `synap-core` | 本文件：共享契约 |
| `db-storage` | `synap-store` | 块存储、codec、同步 |
| `db-runtime` | `synap-runtime` | 插件沙箱、响应式计算图 |
| — | `synap-bridge` | FFI（flutter_rust_bridge），只做跨语言搬运，不做业务 |

## 核心词汇表

| 术语 | 定义 |
| --- | --- |
| **Block** | 物理最小单位。不可变。`BlockHash = Hash(规范化逻辑内容)`，与压缩加密后的物理字节无关。固定头部（信封）+ 裸字节负载。 |
| **Entity** | 逻辑对象（如某条待办）。`EntityId` 标识，生命周期 = 一组父子串起的 Block（DAG）。 |
| **Head** | 某 Entity 无子节点的最新 Block。正常 1 个；多端离线并发时多个（多 Head 分叉）。 |
| **LocalSeq** | 本机自增 `u64`，永不参与同步。每落盘一块 +1，驱动本机响应式引擎。只是到达日志，不是可重放的 Delta 日志。 |
| **Blob** | 大二进制内容的独立块。快照里只存其内容哈希，不内嵌字节。 |
| **Data Plugin** | Lua 纯函数模块，本身也作为 Block 入库。负责编解码、创建/变更逻辑、Trait 提取。 |
| **UI Plugin** | 客户端本地界面层，不进块存储。订阅投影渲染，通过标准接口写回。 |
| **Trait** | 跨插件能力契约（如 `Create`、`Checkable`、`HasText`、`Conflict`）。上层只认 Trait，不认插件。命名保留 `Trait`，不用 `Struct`；结构侧叫 Schema，实例侧叫 Entity。 |
| **Conflict** | 冲突记录。单实体多 Head 由插件 `resolve` 收敛；跨实体约束破坏是带 `participants` 的独立记录，不挂靠任何单个 Entity。 |
| **Projection** | 统一的查询/索引/视图：基于 Trait 的响应式计算结果，底层自动增量维护。身份 = 名字 + 计算定义版本（见 runtime 设计）。 |
| **Delta** | 图中流动单位 `(Key, Value, Weight)`：`+1` 新增，`-1` 撤销。旧值更新 = 先 `(key, old, -1)` 再 `(key, new, +1)`。 |
| **Tombstone** | 逻辑删除块。保证“删除事实”同步全端；删与改谁赢由 `resolve` 决定，Tombstone 本身不赋予删除优先权。 |

`Block`、`BlockHash`、`EntityId`、`Hlc`、`LocalSeq`、`Delta` 的 Rust 定义归这里；读写语义归 store，计算语义归 runtime。
