# 005 synap-store：字节层

`store` 只管字节怎么存、怎么合并、怎么被读出来。不解读 `payload`，不认业务语义，对外全程保持**纯同步阻塞接口**。数据结构定义见 [004 synap-core](https://www.google.com/search?q=004-synap-core.md)。

## 1. 逻辑 Block 与物理管道

CoW，只增不减，只有 CR 没有 UD：

* **逻辑层：** 接收 `synap-core` 的 `Block`，落盘前校验或计算 `BlockHash = BLAKE3(canonical(Block))`。v1 约定 `payload` 为**自包含逻辑快照**（读任一 Head 即得完整状态，无需回溯祖先）；大二进制走独立 `Blob`，`payload` 里只存 `BlobHash`。
* **创世约定：** `schema_hash == [0; 32]` 表示该块 `payload` 是原生 UTF-8 Lua 插件源码，其 `BlockHash` 即 `plugin_hash`，天然寻址，无需额外注册表。
* **物理帧管道（Codec）：** 落盘和网络导出前，统一经过 Rust 原生管道：`Block -> 压缩(Zstd/LZ4) -> 加密(AEAD: ChaCha20-Poly1305 / AES-GCM, 随机 Nonce) -> 物理帧(codec_flags + nonce + ciphertext)`。读取时逆向解包并校验 AEAD tag。全过程对 Lua 插件透明。

## 2. 硬盘上永远只有 5 张 redb 表

1. **`blocks`**：`[u8; 32] -> RawFrameBytes`。统一存放数据 Block、插件源码 Block 与 Blob（通过物理帧首字节区分 `Block / Blob` 种类），全部内容寻址。
2. **`local_seq`**：`u64 -> BlockHash`。本地到达流水，无论本机写入还是远端同步，每落盘一个新块即 `+1`，驱动 `runtime` 的响应式计算。
3. **`entity_heads`**：`EntityId -> Vec<BlockHash>`。每个实体的当前分叉头列表。写入新块时，从列表中移除其 `parents` 并加入新 `BlockHash`。
4. **`projections`**：`(ProjNamespace, EncodedKey) -> Value`。所有常驻投影共享同一棵有序 B-Tree，靠命名空间隔离；计算定义一变即切新命名空间，旧空间待后台回收。
5. **`meta`**：系统状态键值表，存放各投影的 `(ProjNamespace, local_seq 断点)`、插件代码三级信任表（可用/受信/激活）等。

内存侧使用完全相同的 `KeyCodec` 配合 `BTreeMap`。`store` 暴露跨表原子事务：**常驻投影的 B-Tree 写入与其 `meta` 中的 `local_seq` 断点更新必须在同一事务内提交**，杜绝崩溃后重复应用或漏算 Delta。

## 3. Sans-IO 同步引擎

* **同步即并集：** 基于 Merkle-DAG（配合 Merkle Search Tree 或前缀桶摘要）比对树根与分支，定位双方缺失的 `BlockHash` 集合并互传。存储层绝对零冲突，并发修改自然沉淀为 `entity_heads` 中的多 Head，交由 `runtime` 处理。
* **彻底 Sans-IO：** `synap-store` 只提供同步状态机（输入对端摘要消息 → 输出待发送消息或缺失块列表；输入远端块批次 → 校验落盘并推进 `local_seq`），**不绑定 `tokio` 或任何具体网络协议**。真正的传输层交给外围的 `synap-net` 实现。
* **协议预留：** 同步协议在消息定义上预留“仅有骨架、无 Payload”的状态位（为未来 Pruning 留口）；对端重传完整 Payload 时的回填策略留待后续版本。

## 4. 删除：v1 只有 Tombstone

* **逻辑删除：** 写入 `is_tombstone = true` 的新块，`parents` 指向旧 Head。同步后全端生效，彻底避免离线设备将旧块重新同步回来的“幽灵复活”。
* **Pruning 移出 v1：** 掏空 Payload 会导致原 `BlockHash` 无法自证，需要在 Header 中引入独立 `payload_hash` 承诺与因果稳定窗口判定，v1 不提前背这套复杂度。
