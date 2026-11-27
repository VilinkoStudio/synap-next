//! 访问认证：照搬老 relay。
//!
//! * 全局可选 API Key（`x-api-key`），按库/按设备授权的第一道门；
//! * 邮箱路由额外 Ed25519 签名 + 时间戳：取件/确认由邮箱主人签，投件由发送方签（`x-synap-sender-ed25519` 标发送方公钥）。
//!
//! 同步路由只走 API Key——帧本身端到端加密，中继验签只为限流与归属。

use axum::http::HeaderMap;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};

pub const AUTH_TIMESTAMP_HEADER: &str = "x-synap-timestamp";
pub const AUTH_SIGNATURE_HEADER: &str = "x-synap-signature";
pub const MESSAGE_SENDER_HEADER: &str = "x-synap-sender-ed25519";
pub const API_KEY_HEADER: &str = "x-api-key";

/// 允许的时钟偏差：5 分钟。
const SKEW_MS: u64 = 5 * 60 * 1000;

#[derive(Debug, Clone)]
pub enum RelayAuth {
    Disabled,
    ApiKey(String),
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("missing header {0}")]
    Missing(&'static str),
    #[error("invalid header {0}: {1}")]
    Invalid(&'static str, String),
    #[error("unauthorized: {0}")]
    Unauthorized(&'static str),
}

impl AuthError {
    pub fn code(&self) -> &'static str {
        match self {
            AuthError::Missing(_) => "missing_header",
            AuthError::Invalid(_, _) => "invalid_header",
            AuthError::Unauthorized(_) => "unauthorized",
        }
    }
}

pub fn check_api_key(auth: &RelayAuth, headers: &HeaderMap) -> Result<(), AuthError> {
    match auth {
        RelayAuth::Disabled => Ok(()),
        RelayAuth::ApiKey(expected) => {
            let got = headers
                .get(API_KEY_HEADER)
                .and_then(|v| v.to_str().ok())
                .ok_or(AuthError::Missing(API_KEY_HEADER))?;
            if got != expected {
                return Err(AuthError::Unauthorized("invalid_api_key"));
            }
            Ok(())
        }
    }
}

fn header_str<'a>(headers: &'a HeaderMap, name: &'static str) -> Result<&'a str, AuthError> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .ok_or(AuthError::Missing(name))
}

/// 校验邮箱请求签名。`method` 为 GET/POST，`path` 为完整路由路径。
/// POST 投件用发送方公钥验，GET 取件 / ACK 用邮箱公钥验。
pub fn verify_mailbox_request(
    mailbox_public_key_hex: &str,
    headers: &HeaderMap,
    method: &str,
    path: &str,
    now_ms: u64,
) -> Result<(), AuthError> {
    let timestamp = header_str(headers, AUTH_TIMESTAMP_HEADER)?;
    let signature_hex = header_str(headers, AUTH_SIGNATURE_HEADER)?;
    let ts: u64 = timestamp
        .parse()
        .map_err(|_| AuthError::Invalid(AUTH_TIMESTAMP_HEADER, "not a number".into()))?;
    if ts.abs_diff(now_ms) > SKEW_MS {
        return Err(AuthError::Unauthorized("stale_timestamp"));
    }
    let signature: [u8; 64] = hex::decode(signature_hex)
        .map_err(|e| AuthError::Invalid(AUTH_SIGNATURE_HEADER, e.to_string()))?
        .try_into()
        .map_err(|_| AuthError::Invalid(AUTH_SIGNATURE_HEADER, "want 64 bytes".into()))?;
    let signature = Signature::from_bytes(&signature);

    let signer_hex = if method == "POST" && path.ends_with("/acks") {
        mailbox_public_key_hex
    } else if method == "POST" {
        header_str(headers, MESSAGE_SENDER_HEADER)?
    } else {
        mailbox_public_key_hex
    };
    let signer: [u8; 32] = hex::decode(signer_hex)
        .map_err(|e| AuthError::Invalid("signer", e.to_string()))?
        .try_into()
        .map_err(|_| AuthError::Invalid("signer", "want 32 bytes".into()))?;
    let key = VerifyingKey::from_bytes(&signer)
        .map_err(|e| AuthError::Invalid("signer", e.to_string()))?;
    let payload = format!("{method}\n{path}\n{timestamp}");
    key.verify(payload.as_bytes(), &signature)
        .map_err(|_| AuthError::Unauthorized("bad_signature"))
}

pub fn normalize_mailbox_key(value: &str) -> Result<String, AuthError> {
    let lower = value.to_lowercase();
    let raw = hex::decode(&lower).map_err(|e| AuthError::Invalid("mailbox", e.to_string()))?;
    if raw.len() != 32 {
        return Err(AuthError::Invalid("mailbox", "want 32 bytes".into()));
    }
    Ok(lower)
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::Signer;
    use ed25519_dalek::SigningKey;

    fn headers_for(
        signer: &SigningKey,
        sender_hex: Option<String>,
        ts: u64,
        method: &str,
        path: &str,
    ) -> HeaderMap {
        let payload = format!("{method}\n{path}\n{ts}");
        let sig = signer.sign(payload.as_bytes());
        let mut h = HeaderMap::new();
        h.insert(AUTH_TIMESTAMP_HEADER, ts.to_string().parse().unwrap());
        h.insert(
            AUTH_SIGNATURE_HEADER,
            hex::encode(sig.to_bytes()).parse().unwrap(),
        );
        if let Some(s) = sender_hex {
            h.insert(MESSAGE_SENDER_HEADER, s.parse().unwrap());
        }
        h
    }

    #[test]
    fn mailbox_post_verify_ok() {
        let sender = SigningKey::from_bytes(&[3; 32]);
        let sender_hex = hex::encode(sender.verifying_key().to_bytes());
        let ts = now_ms();
        let h = headers_for(&sender, Some(sender_hex), ts, "POST", "/v1/mailboxes/ab");
        assert!(verify_mailbox_request("ab", &h, "POST", "/v1/mailboxes/ab", ts).is_ok());
    }

    #[test]
    fn stale_timestamp_rejected() {
        let k = SigningKey::from_bytes(&[3; 32]);
        let h = headers_for(&k, None, 1, "GET", "/v1/mailboxes/ab");
        assert!(verify_mailbox_request("ab", &h, "GET", "/v1/mailboxes/ab", now_ms()).is_err());
    }
}
