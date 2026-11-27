# 006 synap-runtime：类型系统与计算语义

`runtime` 负责把 `store` 里内核看不懂的字节，变成有类型、有能力、可查询的对象世界。概念出处见 [001 哲学](001-philosophy.md)，数据结构见 [004 synap-core](004-synap-core.md)，字节读写见 [005 store 字节层](005-synap-store.md)。

本文的设计取向一句话概括：**类型系统学编程语言，合并语义学 CRDT，演化学数据库迁移。** 编程语言只解决了第一件事；持久化、多端并发的数据还要面对语言从不需要面对的三件事——实例比代码活得久、同一对象被并发修改、两台设备跑着不同版本的代码。

还有一个贯穿全文的前提：**这些定义多半由 AI 来写。** 所以原语要少、靠组合，作者写的永远是"针对一条记录的纯函数"，遍历、增量、合并、缓存这些复杂性由 runtime 隐藏。

---

## 0. 概念总览

```
L4 视图        View<bound>  ·  视图实例（本身是实体）          ← 宿主侧，见 007
L3 投影        公共投影只吃 Trait  ·  私有投影可吃 Schema
L2 领域 Schema todo / note / ledger …  = 组件组合 + 少量自有字段
L1 标准组件    Title  Text  Check  Time  Tags  Children …（字段 + 合并 + 实现 + 构造）
L0 内核        Block · Entity · Projection · ref 值类型 · 实现表
```

每层只依赖下层，每层都可以跳过：可以不用组件、直接在 L0 上手写 Schema；可以不写视图，靠通用视图兜底。

| 概念 | 回答的问题 | 语言类比 | 存在哪 |
| --- | --- | --- | --- |
| **Schema（结构）** | 这东西长什么样、怎么改 | `struct` + 私有方法 | Lua，进库同步 |
| **Component（组件）** | 可复用的结构片段 | 被嵌入的 `struct`（Go embedding / Kotlin `by`） | Lua，进库同步 |
| **Trait（能力）** | 它能被当成什么用 | `trait` 定义 | 进库同步 |
| **Impl（实现）** | 这个结构怎么满足这个能力 | `impl Trait for Struct` | Lua，进库同步 |
| **Projection（投影）** | 从池子里算出什么 | 迭代器链（在 Delta 上执行） | 有序 B-Tree 索引 |
| **View（视图）** | 一组行怎么画 | 带 trait bound 的泛型函数 | Dart，纯本地 |

**插件不是概念层，而是分发单位。** 一个插件包可以装任意组合：一个 Schema 加几条 Impl、一个只发 Trait 定义的标准库包、一组组件、一组视图。数据侧插件（Schema / 组件 / Trait / Impl / 投影定义）是 Lua，作为 Block 进库同步；视图插件是 Dart，纯本地。

**明确不做的：继承、类型转换。** 继承加持久化数据是"脆弱基类"的放大版——基类一改，所有历史数据跟着出事。类型转换见 §7。

---

## 1. Schema：名义身份与版本分离

* **`SchemaId`**：稳定的名义身份，如 `vilinko.todo/item`，跨版本不变。投影、视图、实现表都只认它。
* **大版本号**：由作者声明，写作 `vilinko.todo/item@1`。和 Trait 一样，小版本只允许兼容扩展；拆字段、改含义这类不兼容的变化要升大版本。别的插件依赖这个 Schema 时，锁定的是大版本。
* **`SchemaVersion`**：具体实现 = 定义该 Schema 的插件源码块的 `BlockHash`。

每个 Block 头部记录 `SchemaId` 与 `SchemaVersion`（见 004）。

**一个实体的 `SchemaId` 终身不变。** 创世块定下，此后所有块必须一致，`store` 落盘时即校验。这条约束直接消灭了"跨 Schema 分叉"这一整类边界情况。`SchemaVersion` 则会随插件升级而变，怎么变见 §6。

---

## 2. Trait：能力契约

```lua
trait "std/Checkable@1" {
  read  = { done = "bool" },
  write = { set_done = { args = { done = "bool" } } },
  doc   = "可完成的事项",
}
```

一条 Trait 定义包含三部分：**读取形状**（extract 产出的规范化值）、**写入方法**（带签名的动作）、**文档**。

* `TraitId@major` 版本化；小版本只允许向后兼容扩展（加可选字段、加方法）。
* Trait 定义本身也作为 Block 入库同步。
* 标准库 Trait 在 `std/` 命名空间；领域 Trait 先放插件自己的命名空间，**有两个以上插件需要共享时再提进 std**——标准词汇表要小、稳、慢，这是从 `embedded-hal` 学的节奏。

---

## 3. Impl：显式声明，名义匹配

```lua
impl "std/Checkable@1" for "vilinko.todo/item" {
  extract  = function(s) return { done = s.status == "done" } end,
  set_done = function(s, a) s.status = a.done and "done" or "open"; return s end,
}
```

* **不做结构匹配**（不学 Go interface）。内核只认 `(TraitId, SchemaId) → Impl` 这张**实现表**，从不猜一个结构"长得像"某个 Trait。
* **任何插件都能给别人的 Schema 写 Impl（学 Kotlin 扩展）：** 官方待办没有实现"可计入周报"，你的周报插件可以替它实现，不需要原作者配合。代价是插件之间出现了依赖，所以扩展 Impl 必须锁定它依赖的版本，写作 `impl "me.report/Reportable@1" for "vilinko.todo/item@1"`：它读的是对方 `@1` 的结构，对方升到 `@2` 时，runtime 能明确知道这条 Impl 需要更新（§6.4）。
* **同一对 `(Trait, Schema)` 出现多个 Impl 时：** Schema 所属插件写的优先；剩下的由用户选一个，选择结果存成一条 `sys/` 数据。Rust 用孤儿规则在编译期杜绝重复，是因为它要在全世界的代码之间保持一致；这里是一个人的库，用户本人就是最终裁决者，不值得为这种少见的情况立一条全局规则。
* **静态声明，实例动态成员：** 实现表是静态的（Schema "可能"具备某能力）；某个具体实体此刻是否具备，由 `extract` 返回值决定，返回 `nil` 即"当前不具备"。成员资格变化就是普通的 Delta（进入 `+1`，离开 `-1`），不需要新原语。

---

## 4. Component：组合与委托

### 组件

```lua
component "std/Text@1" {
  fields    = { body = "text" },
  merge     = "text-3way",
  impl      = { "std/HasText@1" },
  construct = function(args) return { body = args.text or "" } end,
  upgrade   = { },                     -- 小版本迁移，跟着组件走
}
```

组件 = **字段 + 合并策略 + Trait 实现 + 构造 + 升级**。组件**只能被组合进 Schema**，不能单独成为实体——实体和组件两个概念不重叠。

### Schema 由组件拼成，能力委托获得

```lua
schema "vilinko.todo/item" {
  compose = {
    title = "std/Title@1",
    check = "std/Check@1",
    due   = "std/Time@1?",               -- ? = 可选组件
    note  = "std/Text@1?",
  },
  fields  = { priority = "u8" },         -- 自有字段
  merge   = { priority = "lww" },
}
```

`todo/item` 一行 impl 不写，自动获得 `Titled`、`Checkable`、`Temporal`、`HasText`。

委托规则：

1. **委托是名义的、显式的**：只有写进 `compose` 的组件才委托，不按字段名猜。
2. **同一 Trait 被两个组件实现时必须消歧**：`delegate "std/HasText@1" = "note"`，或手写 Impl 覆盖，否则加载报错。
3. **Schema 手写的 Impl 优先于组件委托**，作为逃生口。
4. **不用组件也合法**：纯手写 Lua Schema 永远可以，组合只是加速手段。

### 可选组件：实例能力随时间生长

```lua
schema "vilinko.note/page" {
  compose = { title = "std/Title@1", body = "std/Text@1", check = "std/Check@1?" },
}
```

给某条笔记"标记为待办"，只是一次普通写入——往实例里添上 `check` 组件。`SchemaId` 不变、`EntityId` 不变、所有引用不受影响；它以 `+1` 进入所有 Checkable 投影，去掉组件时 `-1` 离开。能挂哪些组件由 Schema 作者用代码声明，用户只是在声明过的形态之间选择。

### 扩展组件：给别人的类型挂数据

可选组件只能由 Schema 作者预先声明。但你常常想给不属于自己的类型加点东西，比如给官方待办加一个"预计耗时"。扩展 Impl（§3）只能根据对方已有的数据计算，存不下新数据，所以还需要扩展组件：

```lua
extend "vilinko.todo/item@1" {
  estimate = "me.plan/Estimate@1?",      -- 扩展组件一律可选
}
```

* **存在实体自己身上：** payload 本来就按组件分区，扩展组件只是多出来的一个分区，字段、合并、Impl 都归扩展插件。原 Schema 不知道它存在，`SchemaId` 不变。
* **原插件写入时原样带上：** 分区的解码与合并在 Rust 侧完成，原插件的写方法只改自己的分区，认不出的分区由 runtime 原样搬进新块。
* **扩展插件不在了：** 数据不丢，处于休眠状态，通用视图仍能显示原始字段；插件回来后自动恢复。

### 内嵌组件 vs 引用实体

只问一个问题：**它需要独立身份吗？**（被别处引用、单独移动、被单独并发编辑）需要 → 独立实体，用 `ref` 组合（§8）；不需要 → 组件。

| 需求 | 机制 |
| --- | --- |
| 这条笔记顺便要能勾选 | 可选组件 |
| 给官方待办加"预计耗时" | 扩展组件 |
| 笔记里有一条独立的待办 | 引用组合：嵌入一个 todo 实体 |
| 日记里记一笔账 | 引用组合：嵌入一个 ledger 实体 |

### 组合由 runtime 原生完成

`compose` 是声明式清单。payload 按组件分区的规范化结构编码，**解码、合并、extract 的委托都在 Rust 侧完成**；Lua 只承担组件与 Schema 自己那一点逻辑。这样 Lua 的面很小，iOS 上只能解释执行时的性能压力也随之下降。

---

## 5. 合并：分叉收敛按组件委托

多端离线并发会让一个实体出现多个 Head。v1 不再要求每个插件手写 `resolve`：

```
merge(todo 的 N 个 Head)
  = { title:    Title.merge(…)    -- lww（按 Hlc）
      check:    Check.merge(…)    -- lww
      note:     Text.merge(…)     -- 三方文本合并，失败则产出 Conflict
      priority: lww }
```

* **内置合并策略：** `lww`、`set`（并集，如标签）、`text-3way`（以最近公共祖先为基）、`seq`（有序列表，如子节点顺序）。组件也可以提供 Lua 自定义 `merge`。
* **可选组件的增删** 按"存在性"做 lww。
* **合并失败才产生冲突，而且精确到字段路径**：`ConflictRecord { participants: [这个实体], path: "note.body", … }`，而不是"整个实体冲突了"。
* **删改冲突：** Tombstone 与编辑并发时，默认产生冲突交给人决定；Schema 可声明 `on_delete_conflict = "keep" | "delete"` 自动处理。
* **合并是读时计算的确定性结果**，不自动落块；用户下一次写入（或在冲突中心确认）时，新块 `parents` 指向全部 Head，分叉闭合。
* **跨实体约束**（如引用成环）由独立校验投影检出，记为带多个 `participants` 的 `ConflictRecord`。

---

## 6. 演化：版本也是被引用的东西

插件会一直变：作者调整结构，AI 给你的类型加字段，官方待办发了新版本，你的扩展又依赖着官方待办。演化要同时守住三件事：数据不丢；引擎不必永远背着所有旧版本；几台设备各自升级，不会因此制造混乱。

整节只靠一条原则：

> **版本也是被引用的东西。还有人引用就保留，没人引用就回收。**

这和投影的 GC（§10.5）是同一套逻辑：投影靠 `watch` 保活，旧版本靠仍停在它上面的实体保活。

### 6.1 依赖一律写明版本

插件对外的每一条依赖都锁定大版本：用 Trait 写 `std/Checkable@1`，扩展别人的 Schema 写 `vilinko.todo/item@1`。依赖是显式的，runtime 就能画出完整的依赖图。改一个插件之前，可以先知道哪些扩展、投影、视图会受影响。

### 6.2 升级：所有实体一起迁移

新版本激活后，runtime 立即对该 Schema 的全部实体执行迁移，不在新旧两个版本之间长期并行：

* **只加东西**（新增可选字段或组件）：新代码能直接读旧块，什么都不用写，旧代码立刻可以回收。
* **结构变化**（新版本带了 `upgrade`）：为每个实体写一个迁移块。`upgrade` 以迁移链的形式随最新版本一起发布，像 SQL 的 migration 目录，从任何仍可能出现的旧版本都能一路升上来。
* **先试运行：** `upgrade` 是纯函数，所以可以先不落块地对全部实体跑一遍，报告成功多少、失败多少、哪些投影要重建，再决定是否激活。
* **迁移失败的实体**（`upgrade` 报错或返回 `nil`）留在旧版本，进入冲突中心，等 `upgrade` 修好后重试。

### 6.3 迁移块是确定性的

手机和电脑各自迁移同一条待办，如果写出两个不同的块，就会凭空制造一次分叉。所以**迁移块的内容只由父块和新版本决定**，`hlc` 取父块中最大的一个，不读本机时钟（见 004）。两台设备的迁移结果哈希相同，同步时自然去重。

这还顺带保证了一件事：迁移块的时间戳不会比它的父块新，所以并发的真实修改在 lww 合并中总是胜过迁移。

离线设备在旧版本上做的修改照常形成分叉：先用新版本的迁移链把它提升上来，再走 §5 的合并。

### 6.4 依赖者跟不上时

实体**总是全部迁移**，从不因为有人依赖旧版本而停留在旧版本上。停在旧版本的实体，只能读到一份越来越旧的快照，这比直接报错还糟。依赖者分三种情况：

| 升级类型 | 依赖它的东西 | 结果 |
| --- | --- | --- |
| 小版本 | 锁定 `@1` 的依赖自动满足 | 无影响 |
| 大版本，依赖的是 Trait | 新版本继续实现 `Checkable@1` 即可（一个 Schema 可以同时实现同一 Trait 的多个大版本） | 无影响 |
| 大版本，依赖的是 Schema 本身（扩展 Impl、扩展组件） | 新结构不再满足 `@1` | 该扩展暂停生效，数据休眠不丢，"需要更新"进入待处理 |

公共投影与视图只认 Trait，所以它们落在第二行；会被大版本升级影响到的，只有直接扩展这个 Schema 的插件。

### 6.5 回收是卸载，不是删除

常驻系统投影 `sys/schema_versions` 统计每个 `SchemaVersion` 上还有多少实体的当前 Head。计数归零，这个版本的代码就从运行时卸载。

块本身不删：块不可变，还要参与同步和历史回看。历史版本也不需要旧代码才能看，payload 是规范化编码，由 Rust 直接解码，通用视图不运行任何 Lua 就能显示旧版本的字段。作者也可以在旧版本计数长期为零后删减迁移链；此后才冒出来的更旧写入按迁移失败处理。

### 6.6 新版本在本机未受信时

另一台设备已经迁移，迁移块同步到了这台还不信任新版本的设备上。这时这些实体只读展示原始字段，不允许写入，避免用旧代码覆盖新数据。插件块与数据走同一条同步通道，通常会一起到达，所以这种情况只是短暂的。

### 6.7 另一条路：导出成明文，改完再导入

结构大改时，写迁移函数有时比直接改数据还麻烦。这时可以把插件和数据一起导出成明文，交给 agent 批量修改，再导回来：

```
vault-export/
  plugins/me.course/grade.lua
  me.course/grade/0193a…f2.md
```

```markdown
---
id: 0193a…f2
schema: me.course/grade@1
title: 高等数学
score: 58
credits: 4
tags: [必修]
me.plan/Estimate: { minutes: 30 }   # 扩展组件、休眠的组件照样导出
---
正文（std/Text）
```

一个实体一个文件：frontmatter 放组件，正文放文本，人能读，git 能比较，agent 用文本工具就能批量处理。CSV 只适合平铺的字段，所以只作为分析用的导出（连同 SQLite / DuckDB）。

关键在于**导入按差异写入，不清空重来**。导出保留了 `EntityId`，导入时逐个实体比较：没变的不写，改了的在当前 Head 后面接一个新块，新出现的新建，被删掉的写 Tombstone。历史不丢，引用不断，离线设备上的修改照常按 §5 合并；整批导入就是一次普通的批量写入，可以整批撤销。一条测试守住它：**导出后原样导入，不写任何块。**

这条路是所有人的逃生口和编辑界面：原型期引擎有破坏性修改时靠它，AI 大改插件时可以用它，想离开 Synap 时也靠它。但它不替代 §6.2 的自动迁移，普通用户升级不应该需要一个 agent。

---

## 7. 构造，以及为什么不做类型转换

### 构造即 Trait，自动委托

`std/Construct@1` 是唯一的构造契约。组合 Schema 的构造默认 = 逐个调用必需组件的 `construct`，再由 Schema 补上自有字段（类似 C++ 委托构造、Rust 的 `..Default::default()`）：

```lua
construct = function(args, base)      -- base 已由组件构造好
  base.priority = args.priority or 0
  return base
end
```

### 不做类型转换

一个实体是什么类型，从创世起就定了。需要"变成另一种东西"的场景只有两类：

1. **查询与组合（绝大多数情况）：** 原样保留，靠 Trait 投影聚合，靠 `ref` 嵌入。一条笔记不需要"变成"待办才能出现在待办列表里——给它挂上可选的 `check` 组件，或者在它里面嵌一条 todo。
2. **用户明确要求时：** 就是一次普通的"基于此新建"。宿主用源实体的 Trait 值预填目标 Schema 的 `Construct` 参数，新实体带一个指回源实体的 `ref`，**源实体不动**。这是 UI 层的便利，内核不需要 `From<T>`、原地换类型或任何专门机制。

---

## 8. 引用：内核认得的唯一"关系"

`CanonicalValue` 有一个内核认识的值类型：`ref(EntityId)`（及 `[ref]`）。任何 Trait 的读取形状里都可以出现它。

* **反向链接是内核常驻投影 `sys/backlinks`**：对所有 Trait 输出中的 `ref` 字段统一展开建索引，不需要任何插件配合。
* **嵌入、图谱、"被谁引用"面板** 都建立在这一条之上。
* 引用指向的实体被删除时，`ref` 仍然成立（指向一个 Tombstone），由视图决定如何展示。

这是内核破例认识的唯一一点"语义"：它不知道待办是什么，但知道"这个值指向另一个实体"。

---

## 9. 标准库 v1

类型系统给出了词法，标准库给出第一批词汇。它要小、稳、慢，v1 只发这些：

| 组件 | 字段 | 合并 | 提供的 Trait |
| --- | --- | --- | --- |
| `std/Title` | `title: string` | lww | `std/Titled` |
| `std/Text` | `body: text` | text-3way | `std/HasText` |
| `std/Check` | `done: bool, done_at: hlc?` | lww | `std/Checkable` |
| `std/Time` | `at: time?, due: time?` | lww | `std/Temporal` |
| `std/Tags` | `tags: [string]` | set | `std/Tagged` |
| `std/Children` | `children: [ref]` | seq | `std/Tree` |
| `std/Members` | `members: [ref]` | set | `std/Context` |
| `std/Attention` | `reason: string, since: hlc, due: time?` | lww | `std/Attention` |

另有两个不由组件提供、而由 Schema 直接实现的 Trait：

* **`std/Construct`**：构造契约（§7）。
* **`std/Capturable`**：统一入口的识别契约。它挂在 Schema 上而不是实例上——`capture(text) → { confidence, args }?`，返回 `nil` 表示“这段话不归我”。宿主把一段输入交给所有实现者，按置信度给出建议，用户确认后以 `args` 走 `Construct`。纯函数、确定性，不依赖 AI；接了 AI 时，AI 只是另一个给出 `args` 的来源。

以及一个随标准库发布的 Schema：

* **`std/context`（情景）** = `Title + Text + Time? + Members`。成员关系存在情景一侧，所以任何 Schema 的实体都能加入任何情景，被加入方零改动；“它属于哪些情景”由 `sys/backlinks` 反查。推断出的候选成员只是一条投影，用户确认后才写进 `members`。产品含义见 003 第四节。

`Amount` 这类领域能力先在插件命名空间里长，等真有第二个插件要用再提进来。

拿几个场景验算这套词汇够不够：

* **思维导图不是数据类型**，是 bound 为 `Tree + HasText` 的视图；大纲笔记同样满足，于是同一棵树能在导图和大纲之间切换视角。它的数据怎么算出来，见 §10 末尾。
* **“待处理”入口**：一条以 `std/Attention` 为源的公共投影。记账的“待核实”、学习的“待复习”各自用可选的 `Attention` 组件声明，宿主不认识其中任何一个。
* **日本旅行**：一个 `std/context`，成员里有待办、`ledger/entry`、日记。情景页的时间线按成员的 `Temporal` 排开；“本次花费”是记账插件对这组成员给出的摘要。
* **“今天”聚合页**：只认 `Temporal`，不认识任何 Schema；新类型只要实现 `Temporal` 就自动出现。

后三条都落在“投影”上——词汇有了，接下来是怎么在这些词汇上做计算。

---

## 10. Projection：在 Trait 之上计算

查询、索引、缓存在这里是同一件事，统称投影。设计它时要同时满足三个要求，它们通常互相打架：

1. **好写**——作者多半是 AI，写法得是它最熟悉的样子；
2. **更新便宜**——数据一直在变（编辑、同步），改一条不能重算一万条；
3. **读得快**——手机上滚动列表、展开导图要丝滑。

下面每一小节回答其中一个。

### 10.1 写法：像迭代器链，在 Delta 上执行

```lua
projection "todo/open_by_due" {
  from("std/Checkable@1")
    :filter(function(t) return not t.Checkable.done end)
    :map(function(t) return { t.Temporal and t.Temporal.due or MAX, t.Titled.title },
                            { title = t.Titled.title } end)
    :index()
}
```

写起来就是 Rust 的 iterator adapter 或 LINQ，但 runtime **不是在整个集合上跑它，而是在 Delta 上跑它**：一条待办变了，只有这一条的新旧值流过 `filter` 和 `map`，得出 `-1/+1`。

这一切只依赖一条约束：

> **传给算子的闭包只看一条记录。** 可以遍历这条记录自己的字段（比如它的 `children` 列表），可以用 `get` 读它引用的实体，但不能自己去遍历集合。

满足这条约束，增量就是自动的，作者不需要知道"增量"这个词。反过来，`for t in all(...) do ... end; table.sort(...)` 这种自由迭代 runtime 看不懂，只能整段重算，所以不提供。

### 10.2 算子集

| 算子 | 状态 | 说明 |
| --- | --- | --- |
| `from(Trait)` | — | 数据源：当前具备该能力的实体的 Delta |
| `filter` / `map` / `flat_map` | 无 | 逐条处理 |
| `get(ref)`（闭包内） | 依赖追踪 | 读被引用实体的 Trait 值；它变了，重算读过它的记录。代替 join 处理跨实体条件 |
| `group_by` + `count` / `sum` | 有 | 只提供可逆聚合，删除即减回 |
| `join` | 有 | 两个 `index` 的有序归并连接 |
| `index` | 物化 | 落进有序 B-Tree，成为可读取的点；也可作另一条管道的 `from` |

两条纪律：

* **公共投影的 `from` 只能是 Trait**，看不见 Schema，所以天然覆盖未来出现的新类型——这是池子不碎成孤岛的关键约束。**私有投影**可以 `from(Schema)`，但只有该 Schema 所属插件能定义和读取，用于内部索引与幕后数据。
* **算子集里没有递归。** 树和图的遍历不进管道，交给读取侧按需驱动（§10.6）。`min/max/top-k` 这类不可逆聚合也不提供——它们要保存整个多重集合才能撤销。

### 10.3 落地：有序索引

`index` 把管道输出写进 store 的 `projections` B-Tree（常驻）或内存 `BTreeMap`（临时），两者共用 `KeyCodec`：

```
key   = (ProjNamespace, 管道产出的 key 元组…, EntityId)   -- EntityId 收尾保证唯一
value = 管道产出的 value
```

两个由此而来的取舍：

* **写时排序，读时免费。** B-Tree 的 key 顺序就是显示顺序，排序在设计 key 时就决定了；正序倒序共用一个索引（B-Tree 可双向走）。换一种排序就是换一个索引。
* **覆盖索引。** value 里直接放显示要用的东西——标题、摘要、勾选状态、预估行高。滚动时只读索引，不碰 Block、不解密实体、不跑 Lua；点开详情才 `get` 完整实体。

### 10.4 读取：把 B-Tree 的形状直接交出去

B-Tree 擅长两件事：按 key 定位（`O(log n)`）和从当前位置顺着走（每条近乎 `O(1)`）；不擅长"第 5000 条是谁"。读取原语就照着这个形状设计，只有三个：

| 原语 | 语义 |
| --- | --- |
| `get(id)` | 点查一个实体的 Trait 值 |
| `range(index, from_key, dir, limit)` | 定位后顺着走 N 条，返回一批行和下一个游标 |
| `watch(index, key_range)` | 这段区间有 Delta 落入时通知 |

* **游标是 key，不是 offset，也不持有事务。** 停下十分钟再接着滚，从那个 key 重新定位即可；期间上方插入了数据也不会跳行。滚动成本与总数据量无关。
* **快照读。** 每次 `range` 在 redb 的 MVCC 读事务上执行，后台同步写入再多也不阻塞滚动，也读不到半新半旧。
* **`watch` 判定很便宜。** 每个命名空间按 key 有序维护活跃区间，一个 Delta 只需对数时间查它落在哪些区间里；没人 watch 的区间什么都不做。Delta 合批后推送。

所有前台形态都是这三个原语的组合：无限滚动 = 连续 `range` + `watch` 已加载区间；按日期跳转 = 从日期 key 开始 `range`；反向链接 = 对 `sys/backlinks` 按目标 ID 前缀 `range`；搜索 = 对倒排索引几次 `range` 后在内存里求交。宿主侧怎么用它们拼出丝滑滚动，见 007。

### 10.5 维护还是只算一次：由 runtime 决定

同一条管道，作者不区分用途：

* **有人 `watch` 或被 pin 住时**，runtime 增量维护它；
* **只是问一句时**（AI 临时查询、一次性导出），runtime 对全集跑一遍，返回结果，丢掉状态。

增量是拿常驻状态换更新速度，只在"长期存在 × 每次变化小 × 要立刻看到"时划算；这个判断交给 runtime，不交给作者。维护中的投影按可达性做 GC：

* **Roots** = 活跃的 `watch` + pin 住的常驻索引（含 `sys/backlinks`、全文倒排）。
* 失去 watch 的管道进入休眠池并记下 `local_seq` 断点，短时间切回只补算差值；超时或内存吃紧按 LRU 回收。有状态算子的中间结果本身就是 `index`，可以落盘，不必全在内存。
* **心跳与身份：** `local_seq` 推进唤醒管道，本地写入与远端同步走同一路径；按拓扑深度批处理，输出不变时早停。`ProjNamespace = 名字 + 定义哈希`（涵盖算子定义、依赖的 Trait 版本、Impl 所在插件版本），定义一变即切新命名空间，**后台重建期间旧命名空间继续服务读取，建好后原子切换**，用户不会看到空白页。

### 10.6 验算：思维导图

导图需要的数据里，子节点及其顺序、标题、折叠角标归投影；哪些节点展开归视图实例；坐标归宿主布局；"整棵子树多少后代"需要递归，v1 只显示直接子节点数。

```lua
projection "std/tree_children" {
  from("std/Tree@1")
    :flat_map(function(parent)                  -- 遍历自己的字段：合法
      local rows = {}
      for i, ref in ipairs(parent.Tree.children) do
        local c = get(ref)                      -- 依赖追踪
        rows[#rows + 1] = {
          key   = { parent.id, i },             -- 前缀 = 父节点
          value = { id = ref,
                    title = c.Titled and c.Titled.title or (c.HasText and c.HasText.body:sub(1, 40)),
                    child_count = c.Tree and #c.Tree.children or 0,
                    done = c.Checkable and c.Checkable.done },
        }
      end
      return rows
    end)
    :index()
}
```

* **打开：** 每一层对所有已展开节点做一次批量前缀 `range` 并 `watch`，展开 4 层就是 4 次调用；没展开的子树成本为零，只显示 `child_count`。递归由"用户展开"按需驱动，而不是由管道计算。
* **改标题：** 子节点写入新块 → 父节点的 `flat_map` 因 `get` 依赖被重算 → 只有那一行 `-1/+1` → 落在被 watch 的父节点前缀里 → 只重绘一个节点。拖动节点、远端同步走同一路径。
* **顺带得到：** 节点可以是任何实现了相应 Trait 的类型；大纲视图读同一条投影；一个节点可以挂在多棵树下，改一处处处更新。

---

## 11. 插件运行环境

* **纯函数沙箱：** LuaJIT（`mlua`），屏蔽时间、随机数、文件 IO 等非确定性接口；需要时间的地方由 runtime 以参数注入（写入时的 `Hlc`）。
* **宿主 API 带版本：** LuaJIT 停在 Lua 5.1 语义上多年，语言本身不会再变；会变的是本文定义的写法（`schema`、`component`、`impl`、`from`、`get`……）。插件声明自己依赖的宿主 API 版本，引擎为旧版本保留兼容层，或者明确拒绝加载并说明原因，不能悄悄出错。原型期 API 有破坏性修改时，走 §6.7 的导出、改写、导入。
* **作者要写的东西，汇总：**

| 位置 | 函数 | 何时调用 |
| --- | --- | --- |
| 组件 | `construct` / `merge`（可选，覆盖内置策略） / `upgrade` | 创建 / 分叉收敛 / 升级迁移 |
| Impl | `extract` + 每个写方法 | Trait 值更新 / 派发写动作 |
| Schema | `construct` 补充、私有 action、`upgrade` | 同上 |
| 投影 | 算子闭包（只看一条记录） | Delta 流过时 |

  每一项都是"针对一条记录的纯函数"。遍历、增量、合并、缓存、索引、游标全部由 runtime 承担——原语少、靠组合，AI 只需学会这一种写法。

  旧设计中的四层契约（decode / resolve / extract / action）对应关系：decode 由 runtime 原生完成；resolve 变成按组件委托的 merge；extract 归 Impl；action 拆为 Trait 写方法（公开）与 Schema 私有 action。
* **写入通路只有一条：** 任何写入——UI、外部工具、AI——都表达为 `(EntityId, TraitId, method, args)` 或 `(SchemaId, Construct, args)`，经沙箱执行后生成新 Block。读取通路也只有一条：`get` / `range` / `watch`。

---

## 12. 对外：几个通用工具，加上自省

每个 Trait 的写方法都有签名和文档，读取形状 + 实现表 + `sys/backlinks` 构成完整的类型化上下文。但不把每个写方法都生成为一个 MCP tool：插件一多，工具就有几百个，会撑爆 AI 的工具列表，也违背少原语。对外（见 008）只给固定的几个通用工具：

| 工具 | 作用 |
| --- | --- |
| `describe` | 有哪些 Schema、组件、Trait、方法签名、文档，以及各自的数量 |
| `query` | 读一个已有投影，或提交一条只算一次的管道 |
| `get` / `history` | 读一个实体及其历史 |
| `call` / `construct` | 调用 Trait 写方法，或构造新实体 |
| `plugin` | 写入、试运行（§6.2）、激活插件 |
| `export` / `import` | 明文导出与按差异导入（§6.7） |

AI 先 `describe`，再用通用的 `call` 调具体方法：工具数量不变，能力随插件增长。它看到的不是一堆 Markdown，而是"这里有 214 个 Checkable，37 个未完成，每个都可以 `set_done`，其中 12 个被这篇周报引用"。第一阶段先用这组工具跑起来，在使用中再看 `describe` 还缺哪些信息。

对人而言，"一池数据"的感觉取决于宿主和视图的设计（见 003、007）；对 AI 而言，这一点由类型系统直接保证。
