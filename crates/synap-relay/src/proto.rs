//! 线上协议：REST JSON + WebSocket JSON 文本帧。
//!
//! `SyncId` 走 hex，密文帧走 base64。头部加密在帧内，中继只见不透明字节。

use base64::Engine;
use serde::{Deserialize, Serialize};
use synap_sync::{MerkleDigest, SyncId};

pub fn id_to_hex(id: &SyncId) -> String {
    hex::encode(id.as_bytes())
}

pub fn id_from_hex(s: &str) -> Result<SyncId, String> {
    let bytes = hex::decode(s).map_err(|e| format!("bad id hex: {e}"))?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| "bad id length, want 32 bytes".to_string())?;
    Ok(SyncId::from_bytes(arr))
}

fn b64e(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn b64d(s: &str) -> Result<Vec<u8>, String> {
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .map_err(|e| format!("bad base64: {e}"))
}

// ---- REST ----

#[derive(Debug, Serialize)]
pub struct DigestResponse {
    pub count: u64,
    pub root: String,
    pub buckets: Vec<String>,
}

impl From<&MerkleDigest> for DigestResponse {
    fn from(d: &MerkleDigest) -> Self {
        Self {
            count: d.count,
            root: hex::encode(d.root),
            buckets: d.buckets.iter().map(hex::encode).collect(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct DigestBody {
    pub count: u64,
    pub root: String,
    pub buckets: Vec<String>,
}

impl DigestBody {
    pub fn to_digest(&self) -> Result<MerkleDigest, String> {
        if self.buckets.len() != 256 {
            return Err("want 256 buckets".to_string());
        }
        let root = hex::decode(&self.root).map_err(|e| format!("bad root: {e}"))?;
        let root: [u8; 32] = root.try_into().map_err(|_| "bad root length".to_string())?;
        let mut buckets = Vec::with_capacity(256);
        for b in &self.buckets {
            let raw = hex::decode(b).map_err(|e| format!("bad bucket: {e}"))?;
            buckets.push(
                raw.try_into()
                    .map_err(|_| "bad bucket length".to_string())?,
            );
        }
        Ok(MerkleDigest {
            count: self.count,
            root,
            buckets,
        })
    }
}

#[derive(Debug, Serialize)]
pub struct DiffResponse {
    pub buckets: Vec<u8>,
}

#[derive(Debug, Deserialize)]
pub struct OfferBody {
    pub ids: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct WantResponse {
    pub want: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct IdsBody {
    pub buckets: Vec<u8>,
}

#[derive(Debug, Serialize)]
pub struct IdsResponse {
    pub ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct PutBody {
    pub frames: Vec<FrameJson>,
}

#[derive(Debug, Serialize)]
pub struct PutResponse {
    pub accepted: usize,
}

#[derive(Debug, Deserialize)]
pub struct FetchBody {
    pub ids: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct FetchResponse {
    pub frames: Vec<FrameJson>,
}

/// 一帧密文：id hex + frame base64。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrameJson {
    pub id: String,
    pub frame: String,
}

impl FrameJson {
    pub fn encode(id: &SyncId, frame: &[u8]) -> Self {
        Self {
            id: id_to_hex(id),
            frame: b64e(frame),
        }
    }

    pub fn decode(&self) -> Result<(SyncId, Vec<u8>), String> {
        Ok((id_from_hex(&self.id)?, b64d(&self.frame)?))
    }
}

// ---- WebSocket：对称双向，对端（设备/可信节点）跑同一套状态机 ----

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum WsMsg {
    /// 你好，这是我的摘要。
    Hello {
        count: u64,
        root: String,
        buckets: Vec<String>,
    },
    /// 咱俩摘要不同的桶。
    Diff { buckets: Vec<u8> },
    /// 这些 ID 我有（限某次 Diff 的桶，或全量）。
    Offer { ids: Vec<String> },
    /// 这些 ID 我缺，给我。
    WantReq { ids: Vec<String> },
    /// 这些 ID 你缺，拿去。
    Want { want: Vec<String> },
    /// 帧批次（幂等，重复无害）。
    Batch { frames: Vec<FrameJson> },
    /// 收到一批。
    Ack { accepted: usize },
    /// 一轮结束。
    Done {},
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_json_round_trip() {
        let id = SyncId::from_bytes([7; 32]);
        let f = FrameJson::encode(&id, b"opaque");
        let (rid, raw) = f.decode().unwrap();
        assert_eq!(rid, id);
        assert_eq!(raw, b"opaque");
    }

    #[test]
    fn digest_json_round_trip() {
        let mut set = synap_sync::SyncIdSet::new();
        set.insert(SyncId::from_bytes([1; 32]));
        let d = set.digest();
        let resp = DigestResponse::from(&d);
        let body = DigestBody {
            count: resp.count,
            root: resp.root,
            buckets: resp.buckets,
        };
        assert_eq!(body.to_digest().unwrap(), d);
    }
}
