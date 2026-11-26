# 007 synap-app：宿主、桥与视图

本文档界定内核之外的边界：官方 Flutter 客户端如何通过 `synap-bridge` 接上内核、视图如何在宿主内运行，以及内核如何通过独立扩展包向网络与外部世界开放。产品交互原则见 [003 产品形态](003-product-shape.md)，类型系统见 [006 runtime](006-synap-runtime.md)。

```text
Workspace 演进全景：

核心层（无头）：   synap-core ◀── synap-store ◀── synap-runtime
                                      ▲                 ▲
外围接入层：       synap-net (网络同步) ┘                 ├── synap-bridge (专供 Flutter FFI)
                                                        └── synap-http   (统一 HTTP / MCP 接口)

```

---

## 一、`synap-bridge`：专供 Flutter 的 C-ABI 褪壳层

`synap-bridge` **不是**通用对外 API，它只服务于一件事：**把 Rust 内核的类型系统“褪壳”，翻译成适合 C ABI 与 `flutter_rust_bridge`（FRB）的平坦接口给 Dart 调用。**

* **为什么必须独立成一层：** Rust 侧大量使用泛型、生命周期、带负载的嵌套 Enum、`Arc`/Trait 对象与零拷贝切片，这些概念无法直接穿过 C ABI。`synap-bridge` 负责把它们压扁成拥有所有权的平坦 DTO（`struct`、简单 `enum`、`Vec<u8>` 或规范化编码载荷）以及不透明的订阅句柄。
* **零业务逻辑铁律：** `bridge` 内部只持有一个 `Arc<Runtime>`，严禁出现任何业务判断、缓存逻辑或数据拼装。
* **暴露面极窄，只有 5 类入口：**
1. `open_vault / close_vault`：初始化数据目录、解锁密钥、启动 `runtime`。
2. `get / range / range_many / watch`：读取原语（语义见 006 §9.4）。`range` 一次 FFI 调用返回一整批行和下一个游标；`range_many` 把多个前缀的 `range` 合成一次调用（导图逐层展开用）；`watch(index, key_range) -> Stream<DeltaBatchDto>` 只推送落入该区间的 Delta。临时投影也走这一入口，只算一次。
3. `dispatch_action(WriteReq)`：唯一写入口。`WriteReq` 只有两种形态——`TraitCall { entity_id, trait_id, method, args }` 与 `Construct { schema_id, args }`；插件管理（信任升级、归档）走保留的 `sys/*` Trait。
4. `inspect_entity(entity_id)`：拉取实体的 `SchemaId`、当前具备的 Trait 列表、Heads、历史版本链与字段级冲突详情（用于 Lens 切换、时间轴和冲突 Diff 界面）。
5. `sync_trigger / export_trigger`：触发外部同步器或文件桥接导入导出。



**关键联动——用 FFI 生命周期直接驱动内核 GC：**
当 Flutter 中某个视图被关闭、或列表滚走后某段区间不再需要时，Dart 取消对应 `watch` 的 `StreamSubscription`，FRB 自动 Drop Rust 侧的流发送端。`synap-runtime` 捕获到断开后，把这段区间从 GC Roots 中移除；某条管道再无任何区间被监听时转入休眠池。前端完全不需要手写任何“清理内核缓存”的代码。

---

## 二、Flutter 宿主架构设计

Flutter 宿主本身是一个极轻的**无状态渲染壳**：它不建本地 SQLite/Isar，不写 ORM，除了极少量纯本机 UI 偏好（暗黑模式、窗口尺寸）外，**所有业务数据状态 100% 由 `get` / `range` / `watch` 读到的投影驱动**。

### 1. 读取与滚动（`RangeStore`）

内核只给了三个读取原语；“窗口”“分页”“懒加载”都不是内核概念，而是宿主在 Dart 侧用它们拼出的一个通用库 `RangeStore`。它要做到的只有一件事：**手机上滚动和展开永远不等数据。**

* **以 key 为游标双向加载：** 内存里保留当前屏幕前后各 2～3 屏的行；离边缘还剩一屏时异步发出下一次 `range`。一次 `range` 是亚毫秒级的 B-Tree 顺序读，用户滚不到未加载的地方。跳转（如“跳到三月”）就是从那个 key 开始 `range`。
* **只监听已加载的区间：** 对已加载的 key 区间 `watch`，收到 `+1` 插入/更新、`-1` 删除；滚走很远的区间释放并取消监听。因为游标是 key，上方插入新数据时锚点不漂移，列表不跳。
* **每帧最多刷新一次：** 一帧（16ms）内到达的 Delta 合成一次局部重绘，同步期间成千上万条 Delta 涌入也不卡。底层管道已做拓扑批处理和早停，Dart 侧拿到的是去重后的干净状态。
* **滚动时不碰实体：** 行数据就是覆盖索引的 value（标题、摘要、状态、预估行高），瀑布流在内容渲染前就能算好布局；点开详情才 `get` 完整实体，图片等 Blob 异步加载，不阻塞行布局。
* **树形视图同一套路：** 导图、大纲每展开一层，对该层所有已展开节点做一次 `range_many` 并 `watch` 各自前缀；折叠即取消。未展开的子树零成本，坐标由宿主树布局算法在已加载部分上计算，超大画布只绘制视口内节点。

### 2. 宿主工作台骨架

* **导航锚点栏：** 桌面横屏放左侧边栏，手机竖屏放底部栏。里面钉住的是**视图实例**（如收件箱、本周待办、读书笔记），随时可增删排序。视图实例与导航顺序都是 `sys/` 命名空间下的实体，跨端同步。
* **主画布（Main Canvas）：** 完整交给当前激活的视图自由排版。
* **宿主全局层（不归单个插件管）：** 快速记录、全局搜索、反向链接面板、“今天”聚合、冲突中心（订阅 `ConflictRecord`）、历史时间轴，以及**视角切换器（Lens Switcher）**——在任意页面点开一个 Entity，列出所有 bound 被它当前能力满足的视图，一键切换。产品含义见 003 第二节。

### 3. 设计系统与受控组件库（布局自由，零件受控）

为了保证插件有足够表现空间的同时全端视觉高度统一，宿主向视图暴露两层能力：

* **锁死的 Design Tokens：** 配色板（明暗主题自动切换）、字阶、圆角、间距体系、基础动效曲线。视图不能硬编码颜色和字体。
* **标准语义组件库：**
* 布局容器：`SplitPane`、`Column/Row`、`Grid`、`VirtualList`、`Drawer`；
* 信息展示：`Card`、`Tag/Badge`、`MarkdownView`、`Timeline`、`EmptyState`；
* 交互控件：`Button`、`TextInput`、`Checkbox`、`ActionMenu`、`Dialog`。

### 4. View 契约

视图像一个带 trait bound 的泛型函数：只声明需要什么能力，从不声明服务于哪个 Schema。

```dart
abstract class View {
  String get id;
  /// Trait bound：对任何满足它的实体都能渲染，例如 ['std/Checkable@1', 'std/HasText@1']
  List<TraitId> get bounds;
  /// 支持的形态：page（整页）/ card（嵌入卡片）/ row（列表行）
  Set<ViewForm> get forms;
  /// 明确声明只服务横屏或竖屏，放弃强行跨端
  FormFactor get targetFormFactor; // landscape | portrait
  /// 视图实例可配置的参数（投影过滤、排序、分组等）
  ParamSchema get params;

  Widget build(BuildContext context, HostContext host, ViewInstance instance);
}
```

* **数据来源：** 视图通过 `host` 拿到基于公共投影的 `RangeStore`，只能看到 Trait 值，看不到 Schema 私有字段。视图可以声明自己需要的投影管道，也可以复用标准库已有的投影（如 `std/tree_children`）。
* **写回规范：** 一律 `host.call(entityId, 'std/Checkable@1', 'set_done', {done: true})`，或 `host.construct(schemaId, args)`。
* **嵌入：** 正文里遇到 `ref` 时，宿主取被引用实体当前具备的 Trait，挑一个 bound 被满足、支持 `card` 形态的视图就地渲染；没有就用通用卡片。视图自己不处理嵌入逻辑。
* **Lens 选择顺序：** 用户上次为该 Schema 选过的视图 → bound 最具体（满足的 Trait 最多）的视图 → 通用 Trait 视图。
* **通用 Trait 视图（`GenericTraitView`）：** 就是 bound 最宽松、三种形态都支持的那个视图，不是特殊机制。因为标准 Trait 的写方法是公开的，它在任何设备上都提供基础编辑。

### 5. 视图实例

视图实例 = `{ view_id, params, title, icon }`，Schema 为 `sys/view-instance`，跨端同步。

* 同一视图实例在不同端由不同视图渲染：实例记录的是“想看什么”（参数），而端上挑选满足形态的视图来决定“怎么画”。桌面端的 `view_id` 在手机上不可用时，回退到 bound 兼容的竖屏视图或通用视图。
* 导航栏顺序是 `sys/nav` 实体，里面是视图实例的 `ref` 列表（`seq` 合并）。

### 6. 动态形态留白

现阶段先把上述 `View` 接口在 Dart 模块内跑通（视图编译进宿主）。未来若需要运行时下发视图，或让外部 AI 现场生成视图，只需在 `View` 背后挂一个声明式 DSL（JSON / Lua-UI 树）解释器，映射到同一套宿主组件库，无需改动数据通路与宿主骨架。
