# 007 App

本文档界定内核之外的边界：官方 Flutter 客户端如何通过 `synap-bridge` 接上内核、UI 插件如何在宿主内运行，以及内核如何通过独立扩展包向网络与外部世界开放。产品交互原则见 [003 产品形态](https://www.google.com/search?q=003-product-shape.md)。

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
2. `subscribe_projection(req) -> Stream<DeltaBatchDto>`：订阅投影，将内核的增量流持续推给 Dart。
3. `dispatch_action(entity_id, trait_name, action, payload)`：按 Trait 契约派发写动作，或执行插件管理（信任升级、归档）。
4. `inspect_entity(entity_id)`：拉取实体当前 Heads、历史版本链与冲突分叉详情（用于时间轴和冲突 Diff 界面）。
5. `sync_trigger / export_trigger`：触发外部同步器或文件桥接导入导出。



**关键联动——用 FFI 生命周期直接驱动内核 GC：**
当 Flutter 中某个视图页面被关闭、Widget `dispose()` 取消对应的 Dart `StreamSubscription` 时，FRB 会自动 Drop Rust 侧的流发送端。`synap-runtime` 捕获到订阅断开后，立刻将该投影从 GC Roots（锚点）中移除，触发计算图节点转入**温区休眠池**。前端完全不需要手写任何“清理内核缓存”的代码。

---

## 二、Flutter 宿主架构设计

Flutter 宿主本身是一个极轻的**无状态渲染壳**：它不建本地 SQLite/Isar，不写 ORM，除了极少量纯 UI 偏好（暗黑模式、钉住的导航顺序、窗口尺寸）外，**所有业务数据状态 100% 由 `synap-bridge` 推上来的投影流驱动**。

### 1. 增量视图状态管理（`ProjectionStore`）

* 宿主在 Dart 侧提供一个通用的 `ProjectionStore` 容器，内部维护有序结构，直接消费 `Stream<DeltaBatchDto>`：收到 `+1` 插入/更新，收到 `-1` 删除，并按批次通知依赖它的 Widget 局部重绘。
* 因为底层计算图已经做了按拓扑层批处理和早停，Dart 侧拿到的永远是无闪烁、去重后的干净状态。

### 2. 宿主工作台骨架

* **导航锚点栏：** 桌面横屏放左侧边栏，手机竖屏放底部栏。里面钉住的是用户自定义的常用投影视图页面（如收件箱、待办、笔记），随时可增删排序。
* **主画布（Main Canvas）：** 完整交给当前激活的 UI Plugin 自由排版。
* **宿主全局层（不归单个插件管）：** 全局搜索、快速记录（Quick Capture）、冲突解决中心（订阅 `trait.Conflict` 与跨实体 `ConflictRecord`）、以及**视角切换器（Lens Switcher）**——在任意页面点开一个 Entity，都可以一键切换到支持它当前 Trait 的其他 UI 插件。

### 3. 设计系统与受控组件库（布局自由，零件受控）

为了保证插件有足够表现空间的同时全端视觉高度统一，宿主向 UI Plugin 暴露两层能力：

* **锁死的 Design Tokens：** 配色板（明暗主题自动切换）、字阶、圆角、间距体系、基础动效曲线。插件不能硬编码颜色和字体。
* **标准语义组件库：**
* 布局容器：`SplitPane`、`Column/Row`、`Grid`、`VirtualList`、`Drawer`；
* 信息展示：`Card`、`Tag/Badge`、`MarkdownView`、`Timeline`、`EmptyState`；
* 交互控件：`Button`、`TextInput`、`Checkbox`、`ActionMenu`、`Dialog`。



### 4. UI Plugin 契约与单端兜底

每个 UI Plugin 在宿主里都遵循同一份最小契约：

```dart
abstract class UiPlugin {
  String get id;
  /// 明确声明只服务横屏或竖屏，放弃强行跨端
  FormFactor get targetFormFactor; // landscape | portrait
  /// 声明需要订阅的投影定义与支持的 Trait
  List<ProjectionSpec> get projections;
  List<String> get supportedTraits;

  /// 插件拥有画布布局权，通过 HostContext 获取组件库、投影流和动作派发器
  Widget build(BuildContext context, HostContext host);
}

```

* **写回规范：** UI Plugin 通过 `host.dispatchTraitAction(entityId, 'trait.Checkable', 'set_done', ...)` 写回数据，不直接耦合底层某个具体的 Data Plugin。
* **缺插件兜底（通用 Trait 视图）：** 当手机端同步来了只有桌面端 UI Plugin 的数据时，宿主内置的 `GenericTraitView` 接管渲染，按实体暴露的标准 Trait 自动拼装出干净的可读/基础编辑界面，保证跨端零死角。
* **动态形态留白：** 现阶段先把上述 `UiPlugin` 接口在 Dart 模块内跑通；未来若需要运行时动态下发或让外部 AI 现场生成视图，只需在 `UiPlugin` 背后挂载一个声明式 DSL（如 JSON/Lua-UI 树）解释器映射到同一套宿主组件库，无需改动数据通路与宿主骨架。