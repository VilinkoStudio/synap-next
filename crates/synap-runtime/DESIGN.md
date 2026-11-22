# synap-runtime 设计：语义层

runtime 是沙漏的上半：插件定义结构、提取能力、算出投影。只跟 store 的字节打交道，不碰 Flutter。共享词汇见 `../synap-core/DESIGN.md`；字节层见 `../synap-store/DESIGN.md`。

## 1. 分工：Rust 调度，Lua 语义

* Rust：校验写入、读写路由、维护索引与投影、计算图调度、内存缓存与 GC，对上层 App 暴露异步订阅接口。
* LuaJIT（`mlua`）：插件沙箱，只跑纯函数。内核仅注入无状态原生加速函数（如 `sys.cbor`、`sys.json`）。
* 沙箱是硬约束：屏蔽时间、随机数、IO，fuel 上限。确定性不成立，早停与增量正确性全塌。

## 2. 最简插件契约：四层拆开

`extract(heads)` 一体五责是错的，拆成四层：

```text
decode(block)              → Value        自包含快照解码，无祖先读取
resolve(head_values)       → Resolved     可选；{ value, conflicts }
extract(resolved.value)    → Trait Rows   纯函数，不管版本 DAG
action(resolved.value, input) → Write Intent  写什么由宿主落成新块并设 Parents
```

* 单 Head 插件只实现 `decode + extract + action`。
* 可自动合并的插件额外实现 `resolve`；没有 `resolve` 的多 Head 由宿主生成通用 Conflict。
* 普通 Projection 只接触 Resolved Value 与 Trait，不接触 BlockHash 和版本 DAG。

数据流：`HeadSet 改变 → decode → resolve → extract → Trait Delta → Projection`。

三级早停（同步旧历史时省掉绝大部分无效计算）：HeadSet 是否变化 → Resolved Value 是否变化 → Trait Row 是否变化。

## 3. 两类插件：数据与视图彻底剥离

| 维度 | 数据插件（Data Plugin） | 视图插件（UI Plugin） |
| --- | --- | --- |
| 核心职责 | 定义 Schema、实现上节四层契约、定义迁移/冲突策略 | 声明并订阅 Query（投影）、组织界面渲染、触发用户交互 |
| 存储与同步 | 必须作为不可变块进内核，随数据多端同步 | 不进块存储，是客户端本地的壳，随时可换 |
| 生命周期 | 只能归档（禁写可读），不能物理删除，历史永远可读 | 可随时彻底卸载或原地升级，不影响底层数据 |
| 运行约束 | 严格纯函数沙箱，保证确定性 | 自由度高，可为 Web/Native 组件或轻量 UI 脚本 |

红利是 M:N 解绑：一数多用（同一 `MarkdownNote` 被大纲、双链图谱、日历三个 UI 同时订阅），一端多源（同一待办 UI 订阅 `trait.Checkable`，聚合读书笔记、会议纪要、独立待办）。

写回契约：UI 插件写数据走 Trait 标准接口，不直调数据插件——点勾选发出的是“该实体在 `trait.Checkable` 上绑定的 `set_done`”，而不是“调 `std.todo` 的 `toggle`”。

## 4. 数据插件生命周期 + 代码三级信任

1. **注册**：源码以 `schema_hash = 0` 的块落盘，得 `plugin_hash`。
2. **活跃**：实现 `Create` 的插件可建新实体；`actions` 产生新版本块；`extract` 向投影输出 Trait。
3. **演进**：发 v2。v1/v2 实现同一业务 Trait 即可共存，无需迁移；也可 `migrate_from` 按需升级旧块。
4. **归档**：卸载 = 禁用写能力，读能力是否保留是本机策略（可用通用解码器/导出器代替 `extract`）。

同步来的源码不等于可执行。`meta` 里的插件表是信任分级，不是禁用名单：

```text
可用：源码已同步，可校验可查看，不执行
受信：本机明确允许执行
激活：允许 Create / Action / Extract
```

## 5. 冲突：单实体收敛，跨实体独立记录

* 单实体多 Head（含 `[H_del, H_edit]`）整体喂给 `resolve`。能合并的（标签并集、LWW 覆盖）直接收敛；定不了的按默认策略兜底展示，另吐标准 `trait.Conflict`（附分叉 Hash 列表）。
* 跨实体约束破坏（如 A→B 与 B→A 成环）是独立 `Conflict` 记录（`id/kind/participants/involved_heads/detector_version/status`），由校验投影检测，不挂靠任何单个 Entity，不混进 `trait.Conflict`。
* 写时闭合：UI 订阅冲突建冲突中心，支持拉历史块 Diff。用户决断后走一次普通写 Action，新块 `parents` 指向所有冲突 heads，分叉在物理层闭合。
* v1 范围：跨实体只做检测与记录；原子多实体 WriteBatch / Resolution 对象以后再议，解决靠逐实体写 Action。

## 6. 响应式计算图与三级缓存

* 核心算子统一消费 Delta：无状态 `FromTrait`、`Filter`、`FlatMap`；有状态 `Join`、`Reduce`、`Sort/TopK`。能增量的走增量，不支持的（递归可达、路径枚举、全局不变量、任意 Lua UDF、插件升级后失效）走专门算子或重建——Continuation 以后有自己的 `Traverse`，不伪装成 Join。
* Delta 更新必须能取到旧值（`(key, old, -1)` + `(key, new, +1)`）；Trait 输出必须有确定性规范化编码，Early Cutoff 不依赖 Lua table 遍历顺序。
* 调度铁律：按拓扑深度逐层批处理 Delta（消灭菱形依赖的界面闪烁）。
* 投影身份 = 名字 + 计算定义哈希（插件/extract/host API/输入 Trait 版本）。定义变化：建新命名空间 → 重建或迁移 → 切消费者 → 回收旧空间。温区断点同样绑定定义版本。
* 缓存按可达性分级：锚点（GC Roots：打开的 UI 订阅 + Pinned 常驻索引）→ 热区（内存，随 `local_seq` 实时算）→ 温区（失锚休眠池，记（定义版本，断点），短时补算复活，超时/LRU 回收；复苏从源头重算差值）→ 冷区（落盘 `projections` 表，重启从 `meta` 断点续算）。
* Agent 也走运行时：看到已投影好的内容，不是原始块。

## 7. v0 不做（接受现状）

* `Join` 只做一跳；递归/路径算子以后再加。
* 多插件 Trait 命名冲突：先约后定，不做中心注册。
* 全文/向量检索不进内核：外部订阅者消费 Delta 流（如 Tantivy），算完写回普通块。
* Pruning 不实现；跨实体原子写不设计。
