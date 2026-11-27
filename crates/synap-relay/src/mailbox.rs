//! 邮箱：同步之外的信令通道（一次性消息）。
//!
//! 用途只剩两件：新设备配对时交接库密钥、“有新数据了”的唤醒提示。
//! 语义照搬老 relay：每个“发送方 → 接收方”只保留最新一格，取件租约 60s，
//! 确认后删除，过期租约自动释放，超 7 天无人领的格子清理。
//! 同步本身不再走这里（集合并集幂等，无需租约/确认）。

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

const LEASE_TTL_MS: u64 = 60 * 1000;
const RETENTION_MS: u64 = 7 * 24 * 60 * 60 * 1000;

#[derive(Debug, Clone)]
pub struct LeasedSignal {
    pub sender: String,
    pub lease_id: String,
    pub leased_until_ms: u64,
    pub body: Vec<u8>,
}

#[derive(Debug, Default)]
pub struct MailboxStore {
    /// mailbox -> sender -> slot
    boxes: BTreeMap<String, BTreeMap<String, Slot>>,
    lease_seq: AtomicU64,
    pub total_post: u64,
    pub total_ack: u64,
}

#[derive(Debug, Clone)]
struct Slot {
    updated_at_ms: u64,
    body: Vec<u8>,
    lease: Option<Lease>,
}

#[derive(Debug, Clone)]
struct Lease {
    id: String,
    until_ms: u64,
}

impl MailboxStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// 投件：每个发送方只保留最新一格。
    pub fn post(&mut self, mailbox: &str, sender: &str, body: Vec<u8>, now_ms: u64) {
        let entry = self.boxes.entry(mailbox.to_string()).or_default();
        entry.insert(
            sender.to_string(),
            Slot {
                updated_at_ms: now_ms,
                body,
                lease: None,
            },
        );
        self.total_post += 1;
    }
    /// 取件：挑最早的未租约（或租约已过期）格子，授租约。
    pub fn take(&mut self, mailbox: &str, now_ms: u64) -> Option<LeasedSignal> {
        let senders = self.boxes.get_mut(mailbox)?;
        let mut oldest_key: Option<String> = None;
        let mut oldest_ts = u64::MAX;
        for (sender, slot) in senders.iter() {
            let free = slot
                .lease
                .as_ref()
                .map(|l| l.until_ms <= now_ms)
                .unwrap_or(true);
            if free && slot.updated_at_ms < oldest_ts {
                oldest_ts = slot.updated_at_ms;
                oldest_key = Some(sender.clone());
            }
        }
        let sender = oldest_key?;
        let seq = self.lease_seq.fetch_add(1, Ordering::Relaxed);
        let slot = senders.get_mut(&sender)?;
        let lease = Lease {
            id: format!("{now_ms:x}-{seq:x}"),
            until_ms: now_ms + LEASE_TTL_MS,
        };
        let out = LeasedSignal {
            sender: sender.clone(),
            lease_id: lease.id.clone(),
            leased_until_ms: lease.until_ms,
            body: slot.body.clone(),
        };
        slot.lease = Some(lease);
        Some(out)
    }

    /// 确认：租约 ID 对上才删。
    pub fn ack(&mut self, mailbox: &str, lease_id: &str, now_ms: u64) -> bool {
        let Some(senders) = self.boxes.get_mut(mailbox) else {
            return false;
        };
        let mut done_sender: Option<String> = None;
        for (sender, slot) in senders.iter() {
            if slot
                .lease
                .as_ref()
                .map(|l| l.id == lease_id)
                .unwrap_or(false)
            {
                let _ = now_ms;
                done_sender = Some(sender.clone());
                break;
            }
        }
        if let Some(sender) = done_sender {
            senders.remove(&sender);
            if senders.is_empty() {
                self.boxes.remove(mailbox);
            }
            self.total_ack += 1;
            true
        } else {
            false
        }
    }

    /// 过期租约释放 + 超期格子清理。每次取/确认后顺手调一次即可。
    pub fn cleanup(&mut self, now_ms: u64) {
        let mut empty_boxes = Vec::new();
        for (mailbox, senders) in self.boxes.iter_mut() {
            senders.retain(|_, slot| {
                if let Some(lease) = &slot.lease {
                    if lease.until_ms > now_ms {
                        return true;
                    }
                    slot.lease = None;
                }
                now_ms.saturating_sub(slot.updated_at_ms) <= RETENTION_MS
            });
            if senders.is_empty() {
                empty_boxes.push(mailbox.clone());
            }
        }
        for mailbox in empty_boxes {
            self.boxes.remove(&mailbox);
        }
    }

    pub fn mailbox_count(&self) -> usize {
        self.boxes.len()
    }

    pub fn buffered_slots(&self) -> usize {
        self.boxes.values().map(BTreeMap::len).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn post_take_ack_flow() {
        let mut m = MailboxStore::new();
        m.post("mb", "a", b"hello".to_vec(), 1000);
        // 同一发送方覆盖
        m.post("mb", "a", b"hello2".to_vec(), 1001);
        let got = m.take("mb", 1002).unwrap();
        assert_eq!(got.body, b"hello2");
        // 租约期内重复取拿不到
        assert!(m.take("mb", 1003).is_none());
        assert!(m.ack("mb", &got.lease_id, 1004));
        assert!(m.take("mb", 1005).is_none());
    }

    #[test]
    fn expired_lease_released() {
        let mut m = MailboxStore::new();
        m.post("mb", "a", b"x".to_vec(), 0);
        let got = m.take("mb", 1).unwrap();
        // 租约过期后可重新取
        let got2 = m.take("mb", got.leased_until_ms + 1).unwrap();
        assert_ne!(got.lease_id, got2.lease_id);
    }

    #[test]
    fn retention_purges() {
        let mut m = MailboxStore::new();
        m.post("mb", "a", b"x".to_vec(), 0);
        m.cleanup(RETENTION_MS + 1);
        assert_eq!(m.mailbox_count(), 0);
    }
}
