# 004 synap-core：共享原语与契约

`synap-core` 是整个 Workspace 的最底层，**零重度依赖**（不碰 `redb`、`mlua`、`tokio`、网络和 FFI）。它的唯一职责是定死全系统共用的数据结构、确定性编码规范和基础错误类型，防止上层各 Crate 发明同义词。概念解释见 [001 哲学](https://www.google.com/search?q=001-philosophy.md)。

## 1. 核心标识类型

全部采用 Newtype 包装，编译期杜绝混用：

* **`BlockHash([u8; 32])`**：BLAKE3 哈希，对 `Block` 的**规范化逻辑内容**计算得出。与压缩算法、加密密钥、随机 Nonce 完全无关。
* **`BlobHash([u8; 32])`**：大二进制内容块的 BLAKE3 哈希，仅在快照 Payload 中按值引用。
* **`EntityId([u8; 16])`**：实体全局唯一标识（UUIDv7，兼具时间局部性与无中心生成能力）。
* **`LocalSeq(u64)`**：本机单调自增到达流水号，永不跨端同步。
* **`Hlc`**：混合逻辑时钟 `{ physical_ms: u64, logical: u16, node_id: u32 }`，实现 `Ord`，用于跨端偏序收敛与 LWW 决断。
* **`ProjNamespace`**：`{ proj_id: String, def_hash: [u8; 32] }`，投影的完整物理身份，定义哈希涵盖插件版本、extract 逻辑、Host API 与输入 Trait 版本。

## 2. 逻辑 Block 与物理帧分离

为了保证 `BlockHash` 的跨端一致性，在 `core` 层严格区分**参与哈希的逻辑块**与**落盘/传输的物理帧**：

```rust
pub struct BlockHeader {
    pub entity_id: EntityId,
    /// 本 Entity 的祖先哈希；规范化编码前必须按字典序排序去重
    pub parents: Vec<BlockHash>,
    /// 全零 [0; 32] 表示创世约定：payload 为 UTF-8 Lua Data Plugin 源码
    pub schema_hash: BlockHash,
    pub hlc: Hlc,
    pub is_tombstone: bool,
}

pub struct Block {
    pub header: BlockHeader,
    /// v1 为自包含逻辑快照，内核不解读
    pub payload: Vec<u8>,
}

```

* **逻辑哈希计算：** `BlockHash = BLAKE3(canonical_encode(Block))`。
* **物理帧元数据（`codec_flags`、`nonce`、AEAD tag）：** 归 `synap-store` 的物理管道管理，不进入 `BlockHeader`，因此更换压缩级别或重新加密绝不会改变 `BlockHash`。

## 3. 确定性编码规范（硬约束）

以下三处必须满足**绝对确定性**（相同语义永远产出完全一致的字节序列）：

1. **`Block` 规范化编码：** `parents` 排序，采用确定性编码（如 RFC 8949 Deterministic CBOR 或固定二进制布局），保证不同设备对同一逻辑块算出完全相同的 `BlockHash`。
2. **`Trait` 输出值编码：** Lua table 遍历顺序不保证一致，进入计算图前必须统一转为规范化字节/有序结构（`CanonicalValue`），否则早停（Early Cutoff）和 `Delta` 撤销（`-1`）会失效。
3. **有序 Key 编码（`KeyCodec`）：** 供 `BTreeMap` 和 `redb` 共用的保序字节编码（大端整数、带转义分隔符的字符串拼接、反码降序等），保证前缀扫描、范围查询和排序分页的字节序与逻辑序严格一致。

## 4. 计算图与冲突原语

* **`Delta<K, V>`**：`{ key: K, value: V, weight: i8 }`，权重仅取 `+1`（新增）或 `-1`（撤销）。更新永远拆成一对 `(k, old, -1)` 与 `(k, new, +1)`。
* **`ConflictRecord`**：跨实体约束破坏的独立记录（不挂靠单一 Entity）：
`{ id, kind, participants: Vec<EntityId>, involved_heads: Vec<BlockHash>, detector_version, status }`。
* **`CoreError`**：统一底层错误枚举（校验失败、编码损坏、哈希不匹配、插件不可信、存储事务失败等），上层按需包装。
