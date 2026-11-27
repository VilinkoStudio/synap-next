//! 五张表的名字。块和头列表的值编码在各自模块里，投影键在 `synap_core` 的保序编码里。

use redb::TableDefinition;

/// 块哈希 → 帧字节。
pub const BLOCKS: TableDefinition<[u8; 32], &[u8]> = TableDefinition::new("blocks");

/// 本地序号 → 块哈希。序号只在本机递增。
pub const LOCAL_SEQ: TableDefinition<u64, [u8; 32]> = TableDefinition::new("local_seq");

/// 实体标识 → 当前头哈希。值是小端 `u32` 个数，随后每个头 32 字节，严格递增。
pub const ENTITY_HEADS: TableDefinition<[u8; 16], &[u8]> = TableDefinition::new("entity_heads");

/// 投影索引。键是保序编码，值是覆盖索引的展示字节，范围扫描不必再读块。
pub const PROJECTIONS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("projections");

/// 库内的小状态。
///
/// 投影水位的键是 `0x01` 接投影身份，值是小端 `u64` 序号。
/// 同步索引用 `0x02` 加桶号、`0x03` 加同步身份、`0x04` 记个数。
pub const META: TableDefinition<&[u8], &[u8]> = TableDefinition::new("meta");
