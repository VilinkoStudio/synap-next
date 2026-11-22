# synap-store 设计：字节层

store 只管字节怎么存、怎么合并、怎么被读出来。不解读 payload，不认业务语义。共享词汇见 `../synap-core/DESIGN.md`；产品层面的为什么见 `docs/001-philosophy.md`。

## 1. 厚实的 Block

CoW，只增不减，只有 CR 没有 UD：

* 头部（内核管理）：`entity_id`、`parents: Vec<BlockHash>`（本 Entity 的版本祖先，不混别人的 head）、`schema_hash`、`hlc`（混合逻辑时钟，跨端排序用）、`codec_flags`、`is_tombstone`。
* 负载（插件管理）：`payload: Vec<u8>`，v1 约定为**自包含逻辑快照**（读任一 Head 即得完整状态，无需回溯祖先）。大二进制走 Blob，payload 里只存其哈希。
* 创世约定：`schema_hash == 0` 即该块 payload 是原生 UTF-8 Lua 插件源码；其内容哈希就是该插件的 `plugin_hash`，天然寻址，无需注册表。
* `BlockHash = Hash(规范化逻辑内容)`：压缩、加密、编码差异不影响寻址与去重。加密用 AEAD（ChaCha20-Poly1305 / AES-GCM），nonce 随机，落盘传输前统一处理。

## 2. 硬盘上永远只有 5 张 redb 表

1. **`blocks`**：`BlockHash -> RawBytes`（数据块 + 插件源码块 + Blob）。
2. **`local_seq`**：`u64 -> BlockHash`（本地到达流水，驱动响应式）。
3. **`entity_heads`**：`EntityId -> Vec<BlockHash>`（每个实体的当前分叉头列表）。
4. **`projections`**：`(ProjNamespace, EncodedKey) -> Value`。命名空间 = `ProjId` + 计算定义哈希（插件/extract/host API/输入 Trait 版本），定义一变即换新空间，旧空间待回收。所有常驻投影共享同一棵有序 B-Tree。
5. **`meta`**：各投影的 `(命名空间, local_seq 断点)`、插件信任分级表等系统状态。

内存侧同一套 Key 编码用 `BTreeMap` 顶住。前缀查找、范围查询、排序分页、双链关联全靠 Key 拼接。

store 提供跨表事务：投影写入与其 checkpoint 更新必须在同一事务里，否则崩溃后会重复应用或跳过 Delta。

## 3. codec 与无冲突同步

* 压缩（Zstd/LZ4）+ 加密是 Rust 原生管道，对 Lua 透明。
* 同步 = Merkle-DAG 集合并集：比对树根与分支找出缺失 `BlockHash` 互传。存储层绝对零冲突（语义冲突上浮为多 Head，归 runtime）。
* 同步逻辑写成 Sans-IO：只算差异和打包，不绑定具体网络库。全程同步阻塞接口。
* 同步协议需表达“我有骨架但没有 Payload”（为未来 Pruning 留位）；对端重传完整 Payload 时的填入策略以后再定。

## 4. 删除：v1 只有 Tombstone

* 逻辑删除 = 写 `Tombstone` 块，`parents` 指向旧块。同步后全端生效，无幽灵复活。
* Pruning 移出 v1：掏空 Payload 后原 `BlockHash` 无法自证，需要 Header 保留独立 `payload_hash` 承诺等一整套设计，现在不等，以后再议。
