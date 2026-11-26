# synap-next

以前的 Synap使用了碎片 + Tag + Link的形态，解决了部分碎片化记录的问题。但走向通用底座、插件互操作和 Agent 边界时，架构和设计都跟不上我们后来的想象。于是我们准备进行一次大重构，无论是从技术栈还是产品架构，具体的情况还在敲定。


## 我们大致在做什么

笔记会被拿去做完全不同的事：随手记、待办、AI 知识库、个人记录。这些事不该共用同一种组织结构。旧 Synap 用 DAG 去装全部，这条路不再走。

我们使用一个统一的数据库原语，它是一个通用数据存储单元，包含版本和同步，允许不同的上层应用在上层进行建设，并且以不同形式与用户交互。插件可以有渐进式设计，并在插件之间使用松耦合进行交互。形成TodoList、知识库Wiki、个人复盘等各种结构化文字管理工具，他们基于统一的数据库原语，可以进行便携的同步。愿景应该类似一个个人文字原理平台，可以持续的以各种形态结构化的记录自己的信息。接入agent之后，形成一个内容池，里面有关于你的所有。也不必再手动整理内容到一个单一的session，然后复制出来。


## 文档（内部讨论用）

按编号顺序读；后文只引用前文已出现的概念。原型期概念文档即规格，改实现前先改文档。

| 文档 | 内容 |
| --- | --- |
| [000 与旧 Synap](docs/000-from-synap.md) | 为什么放弃了Synap而进行产品级的重构 |
| [001 哲学](docs/001-philosophy.md) | 统一存储；结构 / 能力 / 视图三分与组合；术语表 |
| [002 架构](docs/002-architecture.md) | 具体架构实现和技术选型 |
| [003 产品形态](docs/003-product-shape.md) | 用户体验和产品的设计 |
| [004 core 共享契约](docs/004-synap-core.md) | core crate：Rust 定义归属与 crate 映射，术语表见 001 |
| [005 store 字节层](docs/005-synap-store.md) | 厚 Block、5 张 redb 表、Sans-IO 同步、Tombstone 删除 |
| [006 runtime 类型系统](docs/006-synap-runtime.md) | Schema / 组件 / Trait / Impl、组合与委托、按组件合并、投影管道与 `get`/`range`/`watch` 读取 |
| [007 app 宿主](docs/007-synap-app.md) | synap-bridge、Flutter 宿主、滚动读取、View 契约与视图实例 |
| [008 外围扩展](docs/008-synap-future.md) | synap-net 同步传输、synap-http / MCP 网关 |

## 开发

纯 Flutter + Rust，不再有多端原生壳。单一 codegen 入口：flutter_rust_bridge。
UniFFI 第一版不引；要加时版本只写进根 `Cargo.toml` 的 `[workspace.dependencies]`。

crates/synap-core/      # 共享契约：Block/Entity/Trait/Delta/Tombstone 定义，无重依赖
crates/synap-store/     # 字节层：厚 Block、5 张 redb 表、Sans-IO 同步
crates/synap-runtime/   # 语义层：类型系统（Schema/组件/Trait/Impl）、Lua 沙箱、投影管道与游标读取
crates/synap-bridge/    # 唯一 FFI 薄层：codegen 只扫这里的 api.rs，组装根+翻译层
app/                    # Flutter 宿主（工作台 + Capture/Reader/Actions/Context 槽位）
app/hook/build.dart     # 原生构建钩子：flutter run/build/test 自动编 Rust，无需手动 cargo-ndk
```

```sh
cargo check --workspace --all-targets  # Rust 全仓
cargo test --workspace --all-targets   # 写入通路回归
just gen                               # 读 app/flutter_rust_bridge.yaml，生成 app/lib/bridge_generated/
cd app && flutter analyze && flutter test   # test 会先触发 hook 自动编 Rust
```

构建方式：flutter_rust_bridge `native-assets` 后端。`flutter run/build/test`
经 `hook/build.dart` 自动编译 `crates/synap-bridge` 并打包，不拷 `.so`、
不手跑 cargo-ndk（旧仓 `build-android.sh` 那套不要了）。工具链钉在
`crates/synap-bridge/rust-toolchain.toml`，`just` 经 `RUSTUP_TOOLCHAIN` 对齐。

约束：`core`/`runtime` 不许出现 `flutter_rust_bridge` 依赖；`bridge` 只做翻译转发不写业务；
投影缓存丢了重算，不做第二份真相。提交约定见 [CONTRIBUTING.md](CONTRIBUTING.md)。

## License

[Apache-2.0](LICENSE)
