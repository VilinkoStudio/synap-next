# 005 synap-store：字节层

`store` 只管字节怎么存、怎么合并、怎么被读出来。不解读 `payload`，不认业务语义，对外全程保持**纯同步阻塞接口**。数据结构定义见 [004 synap-core](004-synap-core.md)。

## 1. 逻辑 Block 与物理管道

CoW，只增不减，只有 CR 没有 UD：

* **逻辑层：** 接收 `synap-core` 的 `Block`，落盘前校验或计算 `BlockHash = BLAKE3(canonical(Block))`。v1 约定 `payload` 为**自包含逻辑快照**（读任一 Head 即得完整状态，无需回溯祖先）；大二进制走独立 `Blob`，`payload` 里只存 `BlobHash`。
* **创世约定：** `schema_version == [0; 32]`（且 `schema_id == sys/plugin`）表示该块 `payload` 是原生 UTF-8 Lua 插件源码，其 `BlockHash` 即后续数据块引用的 `schema_version`，天然寻址，无需额外注册表。
* **Schema 身份校验：** 落盘时校验新块的 `schema_id` 与其 `parents` 一致。一个实体的 `schema_id` 终身不变，这是 store 唯一读取的“语义”头部字段，不需要解读 payload。
* **物理帧管道（Codec）：** 落盘和网络导出前，统一经过 Rust 原生管道：`Block -> 压缩(Zstd/LZ4) -> 加密(AEAD: ChaCha20-Poly1305 / AES-GCM, 随机 Nonce) -> 物理帧(codec_flags + nonce + ciphertext)`。读取时逆向解包并校验 AEAD tag。全过程对 Lua 插件透明。

## 2. 硬盘上永远只有 5 张 redb 表

1. **`blocks`**：`[u8; 32] -> RawFrameBytes`。统一存放数据 Block、插件源码 Block 与 Blob（通过物理帧首字节区分 `Block / Blob` 种类），全部内容寻址。
2. **`local_seq`**：`u64 -> BlockHash`。本地到达流水，无论本机写入还是远端同步，每落盘一个新块即 `+1`，驱动 `runtime` 的响应式计算。
3. **`entity_heads`**：`EntityId -> Vec<BlockHash>`。每个实体的当前分叉头列表。写入新块时，从列表中移除其 `parents` 并加入新 `BlockHash`。
4. **`projections`**：`(ProjNamespace, EncodedKey, EntityId) -> Value`。所有常驻投影索引共享同一棵有序 B-Tree，靠命名空间隔离；计算定义一变即切新命名空间，新空间后台建好前旧空间继续服务，切换后旧空间待回收。前台的 `range` 就是在这棵树上按 key 定位后顺序遍历，所以 value 里存的是可直接显示的数据（覆盖索引），滚动时不回读 `blocks`。
5. **`meta`**：系统状态键值表，存放各投影的 `(ProjNamespace, local_seq 断点)`、插件代码三级信任表（可用/受信/激活）等。

临时（不落盘）的投影索引在内存侧使用完全相同的 `KeyCodec` 配合 `BTreeMap`，对 `range` 来说两者没有区别。所有读取都走 redb 的 MVCC 读事务，读不阻塞写，读到的永远是一致快照。`store` 暴露跨表原子事务：**常驻投影的 B-Tree 写入与其 `meta` 中的 `local_seq` 断点更新必须在同一事务内提交**，杜绝崩溃后重复应用或漏算 Delta。

## 3. Sans-IO 同步引擎

* **同步即并集：** 基于 Merkle Search Tree（或前缀桶摘要）比对树根与分支，定位双方缺失的块集合并互传。存储层绝对零冲突，并发修改自然沉淀为 `entity_heads` 中的多 Head，交由 `runtime` 处理。
* **同步只看不透明的身份与密文：** 对账的键是 `SyncId`（见 004）而不是 `BlockHash`，传输的是整帧密文——头部（`entity_id`、`parents`、`schema_id`）也在加密范围内。集合并集本来就不需要知道块之间的父子关系，所以同步协议不依赖任何明文；对端是否持有密钥，协议完全一样。这让“盲中继”成为同步引擎的自然特例，而不是另一套协议（见 008）。
* **收块时自证：** 收到一帧后，先用库密钥解密，按规范化内容重算 `BlockHash`，再核对 `SyncId` 是否与之对应，三者一致才落盘。转发者因此无法注入或篡改块，最多只能扣留或延迟。
* **彻底 Sans-IO：** `synap-store` 只提供同步状态机（输入对端摘要消息 → 输出待发送消息或缺失块列表；输入远端块批次 → 校验落盘并推进 `local_seq`），**不绑定 `tokio` 或任何具体网络协议**。真正的传输层交给外围的 `synap-net` 实现。
* **协议预留：** 同步协议在消息定义上预留“仅有骨架、无 Payload”的状态位（为未来 Pruning 留口）；对端重传完整 Payload 时的回填策略留待后续版本。

## 4. 删除：v1 只有 Tombstone

* **逻辑删除：** 写入 `is_tombstone = true` 的新块，`parents` 指向旧 Head。同步后全端生效，彻底避免离线设备将旧块重新同步回来的“幽灵复活”。
* **Pruning 移出 v1：** 掏空 Payload 会导致原 `BlockHash` 无法自证，需要在 Header 中引入独立 `payload_hash` 承诺与因果稳定窗口判定，v1 不提前背这套复杂度。

## 5. 引擎升级：真相不迁移，派生可重建

5 张表里只有 `blocks` 是真相，其余 4 张都能从块重新算出来。引擎升级因此不需要 Flyway 式的迁移脚本，只有三条规则：

| 数据 | 引擎升级时 |
| --- | --- |
| 块 | 永不改写。块头带 `format`（见 004），规范化编码一旦发布就按版本冻结，要改只能开新版本；引擎永远能读所有历史版本 |
| `local_seq`、`entity_heads`、`projections`、`meta` | 不迁移。`meta` 记一个布局版本号，与引擎不符就丢掉重建，重建期间旧数据继续服务读取 |
| 物理帧（压缩、加密） | 不参与哈希，可以随时更换。旧帧照样能解，下次写入用新格式 |

另外两处版本不在 store 里，但同样要声明：Lua 宿主 API 的版本（见 006 §11），以及设备之间同步协议的版本。

**原型期不承诺兼容。** `format` 固定为 0，引擎可以做破坏性修改，逃生口是明文导出与导入（见 006 §6.7）：用旧引擎导出，用新引擎导入。等到有第一个不是作者本人的用户，`format` 定为 1，从此按上面三条规则兼容。
