//! Sending one encrypted push through a push service.
//!
//! `web-push` builds the VAPID JWT (RFC 8292) and encrypts the payload
//! (RFC 8291 aes128gcm); the POST itself goes through reqwest 0.12 like the
//! rest of the backend.

use web_push::{
    ContentEncoding, SubscriptionInfo, Urgency, VapidSignatureBuilder, WebPushMessageBuilder,
};

use super::store::StoredSubscription;

const TTL_SECS: u32 = 3600;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SendOutcome {
    Delivered,
    /// 404/410 — the subscription is dead and must be deleted.
    Gone,
    /// 401/403 — VAPID rejected; operator misconfiguration.
    Unauthorized,
    Failed(String),
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum SendBuildError {
    #[error("push message build failed: {0}")]
    Build(#[from] web_push::WebPushError),
}

/// Build + POST one push. `vapid_subject` is the VAPID `sub` contact claim
/// (e.g. `mailto:admin@example.com`).
pub(crate) async fn send_push(
    client: &reqwest::Client,
    vapid_pem: &str,
    vapid_subject: &str,
    sub: &StoredSubscription,
    payload_json: &str,
) -> Result<SendOutcome, SendBuildError> {
    let info = SubscriptionInfo::new(&sub.endpoint, &sub.keys.p256dh, &sub.keys.auth);

    let mut sig_builder = VapidSignatureBuilder::from_pem(vapid_pem.as_bytes(), &info)?;
    sig_builder.add_claim("sub", vapid_subject);
    let signature = sig_builder.build()?;

    let mut builder = WebPushMessageBuilder::new(&info);
    builder.set_ttl(TTL_SECS);
    builder.set_urgency(Urgency::Normal);
    builder.set_vapid_signature(signature);
    builder.set_payload(ContentEncoding::Aes128Gcm, payload_json.as_bytes());
    let message = builder.build()?;

    let mut req = client
        .post(message.endpoint.to_string())
        .header("TTL", message.ttl.to_string());
    if let Some(urgency) = message.urgency {
        req = req.header("Urgency", urgency.to_string());
    }
    if let Some(topic) = &message.topic {
        req = req.header("Topic", topic);
    }
    if let Some(payload) = message.payload {
        req = req
            .header("Content-Encoding", payload.content_encoding.to_str())
            .header("Content-Type", "application/octet-stream");
        for (name, value) in payload.crypto_headers {
            req = req.header(name, value);
        }
        req = req.body(payload.content);
    }

    let outcome = match req.send().await {
        Ok(resp) => match resp.status().as_u16() {
            200..=299 => SendOutcome::Delivered,
            404 | 410 => SendOutcome::Gone,
            401 | 403 => SendOutcome::Unauthorized,
            status => SendOutcome::Failed(format!("push service returned {status}")),
        },
        Err(e) => SendOutcome::Failed(format!("push service unreachable: {e}")),
    };
    Ok(outcome)
}
