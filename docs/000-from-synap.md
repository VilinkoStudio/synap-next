# 000 与旧 Synap

旧仓库：`/data/projects/Synap`（Rust workspace，Android / Linux GTK / Windows / Web / CLI，UniFFI 共享核心）。

synap-next **不是**在原 monorepo 上开分支，而是重新定义范围后的新起点。本文避免怀旧式罗列，只写**继承、重写、放弃**。

## 旧 Synap 已经证明的

- **碎片捕获**可行：快速记下、多端壳、`core` 里 KV + DAG 状态机等。
- **`接着说` + DAG** 是差异化语义，不是 Obsidian 双链换皮（见 [002 架构](002-architecture.md)）。
- **Rust 核心 + UniFFI** 能跑通多平台，但维护成本真实（多 adapter、xtask 代码生成、wasm 门控等）。

## 旧架构无法满足的新要求

内部讨论归纳如下（不含私聊全文）：

| 需求 | 旧 Synap 的局限 |
| --- | --- |
| 材料 / 主张 / 视图分层 | 模型偏「笔记记录」单一形态 |
| 多投影（Wiki / Todo / Loam）互操作 | 没有插件契约与公开查询面 |
| Agent 写入边界与 MCP | 未作为一等设计 |
| 插件 UI 生态 | 原生多端 → 每插件多 Renderer，不可持续 |
| 「自我档案」与「知识库」不同 UX | 同一套 feed 隐喻 |

因此 synap-next 先写文档、后写代码：**避免在旧 crate 图上硬叠平台层。**

## 语义映射（意向）

| 旧概念 | synap-next 落点 |
| --- | --- |
| Note / Fragment | 001 的 Fragment（自我档案主载体） |
| Tag | 横向索引，非世界骨架 |
| DAG edge「接着说」 | 保留；继续是应用层语义，非全局唯一边类型 |
| Feed / 同步 / relay | 待定是否移植 |
| 搜索 | 底座能力；是否向量检索未决 |

具体字段、表结构：**不在此承诺**——等 v0 契约。

## 技术资产是否复用

可选路径（未决定）：

1. **移植 `synap-core` 子集**（实体、DAG、存储）进新仓库，换外壳与 FFI 策略。  
2. **参考行为重写**，存储格式不兼容，换迁移工具时再议。  
3. **首版 Dart/SQLite 原型**，验证后再决定是否 Rust 化核心。

Flutter+Rust 还是全 Dart，要等原型再定。情感上倾向 Rust 底座，**但不接受空桥**。

## 对贡献者的说明

实现细节可查 `/data/projects/Synap`；v1 的 `docs/adr/` 仅供对照旧代码边界，**不**当作 synap-next 的已定决策。

若你熟悉旧 Synap：把「当时为什么这样写」和「现在文档里的应然」分开谈；默认不承诺数据格式兼容，直到单独写迁移说明。

## 名字

**synap-next** 表示方向延续，不保证 API 或数据延续。最终产品名可再改；仓库名先保持低调。