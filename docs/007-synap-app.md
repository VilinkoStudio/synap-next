# 007 synap-app：宿主、桥与视图

本文档界定内核之外的边界：官方 Flutter 客户端如何通过 `synap-bridge` 接上内核，以及宿主、视图与第一方插件如何分工。网络同步与对外接口见 [008](008-synap-future.md)。产品交互原则见 [003 产品形态](003-product-shape.md)，类型系统见 [006 runtime](006-synap-runtime.md)。

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

## 二、Flutter 宿主：微内核

宿主是一个极轻的**无状态渲染壳**：不建本地 SQLite/Isar，不写 ORM，除了极少量纯本机偏好（暗黑模式、窗口尺寸）外，所有业务状态都由 `get` / `range` / `watch` 读到的投影驱动。

它同时是一个**微内核**：只保留做成插件就不安全、或会造成循环依赖的部分——打开库与解锁、插件加载与信任、设计系统、导航框架与视图解析（Lens、嵌入、兜底）、通用 Trait 视图、冲突处理与历史回滚，以及统一入口的系统槽位。其余一切（随手记、待办、情景、今天、收件箱、搜索界面、入口输入框）都是随安装包发行的第一方插件，与第三方走同一套接口。边界的产品理由与两个守则（自举测试、吃自己的狗粮）见 003 第六节。

下面按“数据怎么到视图 → 视图长什么样 → 视图怎么组合 → 组合怎么被保存”的顺序展开。

### 1. 读取与滚动（`RangeStore`）

内核只给了三个读取原语；“窗口”“分页”“懒加载”都不是内核概念，而是宿主在 Dart 侧用它们拼出的通用库 `RangeStore`。它只做一件事：**手机上滚动和展开永远不等数据。**

* **以 key 为游标双向加载：** 内存里保留当前屏幕前后各 2～3 屏的行；离边缘还剩一屏时异步发出下一次 `range`。一次 `range` 是亚毫秒级的 B-Tree 顺序读，用户滚不到未加载的地方。跳转（如“跳到三月”）就是从那个 key 开始 `range`。
* **只监听已加载的区间：** 对已加载的 key 区间 `watch`，收到 `+1` 插入/更新、`-1` 删除；滚走很远的区间释放并取消监听。因为游标是 key，上方插入新数据时锚点不漂移，列表不跳。
* **每帧最多刷新一次：** 一帧（16ms）内到达的 Delta 合成一次局部重绘，同步期间成千上万条 Delta 涌入也不卡。
* **滚动时不碰实体：** 行数据就是覆盖索引的 value（标题、摘要、状态、预估行高），瀑布流在内容渲染前就能算好布局；点开详情才 `get` 完整实体，图片等 Blob 异步加载。
* **树形视图同一套路：** 导图、大纲每展开一层，对该层所有已展开节点做一次 `range_many` 并 `watch` 各自前缀；折叠即取消。坐标由宿主树布局算法在已加载部分上计算，超大画布只绘制视口内节点。

### 2. View 契约

视图像一个带 trait bound 的泛型函数：只声明需要什么能力、能以什么形态出现，从不声明服务于哪个 Schema。

```dart
abstract class View {
  String get id;
  /// Trait bound：对任何满足它的实体都能渲染，例如 ['std/Checkable@1', 'std/HasText@1']
  List<TraitId> get bounds;
  /// 能以哪些形态出现（见下表）
  Set<ViewForm> get forms;
  /// tile 形态支持的尺寸，像手机小组件
  Set<TileSize> get tileSizes;            // small | medium | large
  /// 明确声明只服务横屏或竖屏，放弃强行跨端
  FormFactor get targetFormFactor;        // landscape | portrait
  /// 视图实例可配置的参数（过滤、排序、分组等）
  ParamSchema get params;

  Widget build(BuildContext context, HostContext host, ViewInput input);
}
```

| 形态 | 输入 `ViewInput` | 出现在哪 |
| --- | --- | --- |
| `page` | 一个视图实例 | 主画布，整页 |
| `row` | 一个实体 | 列表的一行 |
| `card` | 一个实体 | 被嵌入正文或其他视图 |
| `summary` | 一组实体 | 情景页的摘要（如“本次花费 ¥8,200”） |
| `tile` | 一个视图实例 | 首页或其他视图里的磁贴 |

* **数据来源：** 视图通过 `host` 拿到基于公共投影的 `RangeStore`，只能看到 Trait 值，看不到 Schema 私有字段。视图可以声明自己需要的投影管道，也可以复用标准库已有的投影（如 `std/tree_children`）。
* **写回：** 一律 `host.call(entityId, 'std/Checkable@1', 'set_done', {done: true})`，或 `host.construct(schemaId, args)`。
* **通用 Trait 视图（`GenericTraitView`）** 就是 bound 最宽松、五种形态都支持的那个视图，不是特殊机制。标准 Trait 的写方法是公开的，所以它在任何设备上都提供基础编辑。

### 3. 组合：`host.embed`

视图嵌入视图只有一个入口，并且**只能按契约、不能按名字**：

```dart
host.embed(EmbedTarget.entity(id),        form: ViewForm.card)     // 正文里的 ref
host.embed(EmbedTarget.set(ids, bound),   form: ViewForm.summary)  // 情景页摘要
host.embed(EmbedTarget.instance(ref),     form: ViewForm.tile)     // 磁贴
```

宿主据目标当前具备的 Trait，挑一个 bound 被满足、支持该形态的视图来渲染；挑选顺序是“用户上次为这类东西选过的 → bound 最具体的 → 通用 Trait 视图”。Lens 切换走的也是同一套解析。

组合规则保持最简单：

* **数据单向往下传：** 父视图只给子视图“显示哪些东西 + 参数”；子视图要写数据，直接走 Trait 写方法，不经过父视图。
* **视图之间不互相发消息：** 没有事件总线，没有共享状态。两个磁贴要联动，只能通过数据——一个写了，另一个的 `watch` 自然收到。
* **尺寸由父视图分配：** 子视图只声明支持的 `tileSizes`。
* **性能自然成立：** 每个被嵌入的视图各自 `range` / `watch` 自己的区间，滚出屏幕即释放，GC 不需要特殊处理。

情景插件因此很薄：它的 `page` 视图 = 正文 + 成员时间线 + 对成员按能力分组后逐组 `embed(set, summary)`。它不认识任何别的插件。

### 4. 视图实例、布局与预设

视图实例 = `{ view_id, params, title, icon }`，Schema 为 `sys/view-instance`，跨端同步。实例记录的是“想看什么”，端上挑选满足形态的视图决定“怎么画”；桌面端的 `view_id` 在手机上不可用时，回退到 bound 兼容的竖屏视图或通用视图。

在它之上，布局全部是数据：

* **导航栏**是 `sys/nav` 实体，一个视图实例的 `ref` 列表（`seq` 合并）。桌面横屏放左侧边栏，手机竖屏放底部栏。
* **首页**本身是一个视图实例，它的磁贴是一组视图实例的 `ref`。
* **预设**是一包数据：要启用的插件列表 + 若干视图实例 + 导航 + 首页磁贴 + 情景模板。首次打开时选择预设，就是写入这些实体、启用这些插件；预设可以分享、导入，也可以由 AI 生成。
* **活跃情景**是宿主的一项状态，存于本设备的 `sys/` 实体。处于某个活跃情景时，宿主对每一次 `construct` 追加一次对该情景的 `add_member` 写入——这是宿主在两个普通写入之间做的编排，内核不知道“活跃情景”这回事。

### 5. 宿主全局层

主画布完整交给当前视图自由排版。不归任何单个视图管的全局层，由宿主提供**槽位**、由第一方插件填充：

* **统一入口：** 宿主负责全局快捷键、手势与系统分享入口，把文本交给所有 `std/Capturable` 实现者并展示建议；输入框与建议列表的 UI 是插件。
* **待处理、今天、搜索、反向链接面板：** 都是第一方视图，读的是 `std/Attention`、`Temporal`、全文倒排与 `sys/backlinks` 等公共投影。
* **冲突中心与历史时间轴：** 留在宿主内核（安全网），订阅 `ConflictRecord` 并使用 `inspect_entity`。

### 6. 设计系统与受控组件库（布局自由，零件受控）

为了保证视图有足够表现空间的同时全端视觉高度统一，宿主向视图暴露两层能力：

* **锁死的 Design Tokens：** 配色板（明暗主题自动切换）、字阶、圆角、间距体系、基础动效曲线。视图不能硬编码颜色和字体。
* **标准语义组件库：**
  * 布局容器：`SplitPane`、`Column/Row`、`Grid`、`VirtualList`、`Drawer`；
  * 信息展示：`Card`、`Tag/Badge`、`MarkdownView`、`Timeline`、`EmptyState`；
  * 交互控件：`Button`、`TextInput`、`Checkbox`、`ActionMenu`、`Dialog`。

### 7. 视图怎么分发：v1 编译进宿主

v1 的视图是 Dart 模块，**编译进宿主、随版本发行**。这回答了 002 留白的第二项，也意味着 v1 能在运行时下载的只有数据：

| 能下载的东西 | v1 |
| --- | --- |
| 预设、情景模板（纯数据） | 可以 |
| 数据插件（Lua） | Android 与桌面可以；iOS 需评估审核风险 |
| 视图 | 只能随版本发行 |

可下载的视图留给下一阶段：在 `View` 背后挂一个声明式描述（JSON / Lua 描述的 UI 树）的解释器，映射到同一套宿主组件库，数据通路与宿主骨架都不用改。它同时是“对 AI 说一句话，生成一个小工具”这条路线需要的东西——AI 生成数据插件已经可行，生成视图要等这一层。
