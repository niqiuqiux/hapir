use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use p256::ecdsa::{signature::Signer, Signature, SigningKey};
use serde::{Deserialize, Serialize};
use tracing::warn;
use url::Url;

use crate::config::settings::VapidKeys;
use crate::store::push_subscriptions;
use crate::store::Store;

/// Payload for a push notification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushPayload {
    pub title: String,
    pub body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<PushData>,
}

/// Additional data included in a push notification payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PushData {
    pub r#type: String,
    pub session_id: String,
    pub url: String,
}

/// Error type for push notification operations.
#[derive(Debug)]
pub enum PushError {
    HttpError(String),
    SubscriptionGone,
    JwtError(String),
    EncryptionError(String),
}

impl std::fmt::Display for PushError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PushError::HttpError(msg) => write!(f, "HTTP request failed: {msg}"),
            PushError::SubscriptionGone => write!(f, "subscription gone (410)"),
            PushError::JwtError(msg) => write!(f, "JWT signing failed: {msg}"),
            PushError::EncryptionError(msg) => write!(f, "encryption failed: {msg}"),
        }
    }
}

impl std::error::Error for PushError {}

/// Service for sending web push notifications.
pub struct PushService {
    vapid_keys: VapidKeys,
    subject: String,
    store: Arc<Store>,
}

impl PushService {
    pub fn new(vapid_keys: VapidKeys, subject: String, store: Arc<Store>) -> Self {
        Self {
            vapid_keys,
            subject,
            store,
        }
    }

    /// Send a push notification to all subscriptions in a namespace.
    ///
    /// On 410 Gone responses, the subscription is automatically removed from the store.
    pub async fn send_to_namespace(&self, namespace: &str, payload: &PushPayload) {
        let subscriptions = {
            let conn = self.store.conn();
            push_subscriptions::get_push_subscriptions_by_namespace(&conn, namespace)
        };

        if subscriptions.is_empty() {
            return;
        }

        let body = match serde_json::to_string(payload) {
            Ok(b) => b,
            Err(e) => {
                warn!(error = %e, "failed to serialize push payload");
                return;
            }
        };

        for subscription in &subscriptions {
            match self
                .send_web_push(
                    &subscription.endpoint,
                    &subscription.p256dh,
                    &subscription.auth,
                    &body,
                )
                .await
            {
                Ok(()) => {}
                Err(PushError::SubscriptionGone) => {
                    let conn = self.store.conn();
                    push_subscriptions::remove_push_subscription(
                        &conn,
                        namespace,
                        &subscription.endpoint,
                    );
                }
                Err(e) => {
                    warn!(
                        error = %e,
                        endpoint = %subscription.endpoint,
                        "failed to send push notification"
                    );
                }
            }
        }
    }

    /// Send a web push notification to a single subscription endpoint.
    ///
    /// Implements the Web Push protocol with VAPID authentication (RFC 8292)
    /// and payload encryption using aes128gcm content encoding (RFC 8291).
    async fn send_web_push(
        &self,
        endpoint: &str,
        p256dh: &str,
        auth: &str,
        body: &str,
    ) -> Result<(), PushError> {
        // --- 1. Build VAPID JWT (ES256) ---
        let jwt = self.build_vapid_jwt(endpoint)?;

        // --- 2. Encrypt payload (RFC 8291 aes128gcm) ---
        let p256dh_bytes = decode_base64_flexible(p256dh)
            .map_err(|e| PushError::EncryptionError(format!("bad p256dh: {e}")))?;
        let auth_bytes = decode_base64_flexible(auth)
            .map_err(|e| PushError::EncryptionError(format!("bad auth: {e}")))?;

        let encrypted = ece::encrypt(&p256dh_bytes, &auth_bytes, body.as_bytes())
            .map_err(|e| PushError::EncryptionError(e.to_string()))?;

        // --- 3. POST to push endpoint ---
        let client = reqwest::Client::new();
        let resp = client
            .post(endpoint)
            .header(
                "Authorization",
                format!("vapid t={jwt},k={}", self.vapid_keys.public_key),
            )
            .header("TTL", "2419200")
            .header("Content-Encoding", "aes128gcm")
            .header("Content-Type", "application/octet-stream")
            .body(encrypted)
            .send()
            .await
            .map_err(|e| PushError::HttpError(e.to_string()))?;

        match resp.status().as_u16() {
            200..=299 => Ok(()),
            410 => Err(PushError::SubscriptionGone),
            status => {
                let text = resp.text().await.unwrap_or_default();
                Err(PushError::HttpError(format!(
                    "push endpoint returned {status}: {text}"
                )))
            }
        }
    }

    /// Build a VAPID JWT signed with ES256 for the given endpoint.
    fn build_vapid_jwt(&self, endpoint: &str) -> Result<String, PushError> {
        let origin = extract_origin(endpoint)?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let header_b64 =
            URL_SAFE_NO_PAD.encode(br#"{"typ":"JWT","alg":"ES256"}"#);
        let claims = serde_json::json!({
            "aud": origin,
            "exp": now + 43200,
            "sub": &self.subject,
        });
        let claims_b64 =
            URL_SAFE_NO_PAD.encode(claims.to_string().as_bytes());
        let signing_input = format!("{header_b64}.{claims_b64}");

        // Decode VAPID private key and sign
        let key_bytes = URL_SAFE_NO_PAD
            .decode(&self.vapid_keys.private_key)
            .map_err(|e| PushError::JwtError(format!("bad private key: {e}")))?;
        let signing_key =
            SigningKey::from_bytes(key_bytes.as_slice().into())
                .map_err(|e| PushError::JwtError(e.to_string()))?;
        let sig: Signature = signing_key.sign(signing_input.as_bytes());
        let sig_b64 = URL_SAFE_NO_PAD.encode(sig.to_bytes());

        Ok(format!("{signing_input}.{sig_b64}"))
    }
}

/// Extract the origin (scheme + host) from a URL.
fn extract_origin(endpoint: &str) -> Result<String, PushError> {
    let parsed = Url::parse(endpoint)
        .map_err(|e| PushError::HttpError(format!("bad endpoint URL: {e}")))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| PushError::HttpError("endpoint URL has no host".into()))?;
    Ok(format!("{}://{}", parsed.scheme(), host))
}

/// Decode a base64 string that may be standard or URL-safe, with or without padding.
fn decode_base64_flexible(input: &str) -> Result<Vec<u8>, base64::DecodeError> {
    use base64::engine::general_purpose::{STANDARD, URL_SAFE};
    // Try URL-safe no-pad first (most common for Web Push), then others.
    URL_SAFE_NO_PAD
        .decode(input)
        .or_else(|_| URL_SAFE.decode(input))
        .or_else(|_| STANDARD.decode(input))
}