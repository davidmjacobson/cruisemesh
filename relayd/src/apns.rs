use std::collections::HashSet;
use std::env;
use std::fs;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::{PushWake, RelayStore};

const APNS_TOKEN_LIFETIME_SECS: u64 = 50 * 60;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApnsConfig {
    pub key_id: String,
    pub team_id: String,
    pub bundle_id: String,
    pub private_key_pem: Vec<u8>,
    pub sandbox: bool,
}

impl ApnsConfig {
    /// APNs is optional for self-hosted relays. Supplying none of the provider
    /// variables disables it; supplying only some is a startup error rather
    /// than a relay that quietly promises background wakes it cannot send.
    pub fn from_env() -> Result<Option<Self>, String> {
        let read = |name: &str| {
            env::var(name)
                .ok()
                .map(|item| item.trim().to_string())
                .filter(|item| !item.is_empty())
        };
        let key_id = read("CRUISEMESH_APNS_KEY_ID");
        let team_id = read("CRUISEMESH_APNS_TEAM_ID");
        let bundle_id = read("CRUISEMESH_APNS_BUNDLE_ID");
        let key_file = read("CRUISEMESH_APNS_PRIVATE_KEY_FILE");
        if key_id.is_none() && team_id.is_none() && bundle_id.is_none() && key_file.is_none() {
            return Ok(None);
        }
        let required = |name: &str, value: Option<String>| {
            value
                .map(|item| item.trim().to_string())
                .filter(|item| !item.is_empty())
                .ok_or_else(|| format!("{name} is required when APNs is configured"))
        };
        let key_file = required("CRUISEMESH_APNS_PRIVATE_KEY_FILE", key_file)?;
        let private_key_pem = fs::read(&key_file)
            .map_err(|error| format!("could not read APNs private key {key_file:?}: {error}"))?;
        let sandbox = match env::var("CRUISEMESH_APNS_ENVIRONMENT")
            .unwrap_or_else(|_| "production".to_string())
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "production" => false,
            "sandbox" | "development" => true,
            other => return Err(format!("invalid CRUISEMESH_APNS_ENVIRONMENT {other:?}")),
        };
        Ok(Some(Self {
            key_id: required("CRUISEMESH_APNS_KEY_ID", key_id)?,
            team_id: required("CRUISEMESH_APNS_TEAM_ID", team_id)?,
            bundle_id: required("CRUISEMESH_APNS_BUNDLE_ID", bundle_id)?,
            private_key_pem,
            sandbox,
        }))
    }

    fn endpoint(&self) -> &'static str {
        if self.sandbox {
            "https://api.sandbox.push.apple.com"
        } else {
            "https://api.push.apple.com"
        }
    }
}

#[derive(Serialize)]
struct ProviderClaims<'a> {
    iss: &'a str,
    iat: u64,
}

#[derive(Serialize)]
struct WakePayload {
    aps: WakeAps,
    cruisemesh_relay_wake: bool,
}

#[derive(Serialize)]
struct WakeAps {
    #[serde(rename = "content-available")]
    content_available: u8,
}

#[derive(Deserialize)]
struct ApnsErrorBody {
    reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ApnsSendOutcome {
    Accepted,
    InvalidToken,
    Rejected(String),
}

struct CachedProviderToken {
    value: String,
    issued_at: u64,
}

pub struct ApnsClient {
    config: ApnsConfig,
    encoding_key: EncodingKey,
    client: Client,
    provider_token: Mutex<Option<CachedProviderToken>>,
}

impl ApnsClient {
    pub fn new(config: ApnsConfig) -> Result<Self, String> {
        let encoding_key = EncodingKey::from_ec_pem(&config.private_key_pem)
            .map_err(|error| format!("invalid APNs EC private key: {error}"))?;
        let client = Client::builder()
            .http2_adaptive_window(true)
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|error| format!("could not build APNs HTTP client: {error}"))?;
        Ok(Self {
            config,
            encoding_key,
            client,
            provider_token: Mutex::new(None),
        })
    }

    fn provider_token(&self, now_secs: u64) -> Result<String, String> {
        let mut cached = self
            .provider_token
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(token) = cached.as_ref() {
            if now_secs.saturating_sub(token.issued_at) < APNS_TOKEN_LIFETIME_SECS {
                return Ok(token.value.clone());
            }
        }
        let mut header = Header::new(Algorithm::ES256);
        header.kid = Some(self.config.key_id.clone());
        let value = encode(
            &header,
            &ProviderClaims {
                iss: &self.config.team_id,
                iat: now_secs,
            },
            &self.encoding_key,
        )
        .map_err(|error| format!("could not sign APNs provider token: {error}"))?;
        *cached = Some(CachedProviderToken {
            value: value.clone(),
            issued_at: now_secs,
        });
        Ok(value)
    }

    async fn send_wake(&self, device_token: &str) -> Result<ApnsSendOutcome, String> {
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "system clock is before Unix epoch".to_string())?
            .as_secs();
        let authorization = format!("bearer {}", self.provider_token(now_secs)?);
        let response = self
            .client
            .post(format!(
                "{}/3/device/{device_token}",
                self.config.endpoint()
            ))
            .header("authorization", authorization)
            .header("apns-topic", &self.config.bundle_id)
            .header("apns-push-type", "background")
            .header("apns-priority", "5")
            .header("apns-collapse-id", "cruisemesh-relay-wake")
            .json(&WakePayload {
                aps: WakeAps {
                    content_available: 1,
                },
                cruisemesh_relay_wake: true,
            })
            .send()
            .await
            .map_err(|error| format!("APNs request failed: {error}"))?;
        if response.status().is_success() {
            return Ok(ApnsSendOutcome::Accepted);
        }
        let status = response.status();
        let reason = response
            .json::<ApnsErrorBody>()
            .await
            .map(|body| body.reason)
            .unwrap_or_else(|_| format!("HTTP {status}"));
        if status == StatusCode::GONE
            || matches!(
                reason.as_str(),
                "BadDeviceToken" | "DeviceTokenNotForTopic" | "Unregistered"
            )
        {
            Ok(ApnsSendOutcome::InvalidToken)
        } else {
            Ok(ApnsSendOutcome::Rejected(reason))
        }
    }
}

pub fn spawn_apns_worker(
    store: RelayStore,
    config: ApnsConfig,
    mut receiver: mpsc::Receiver<PushWake>,
) -> Result<tokio::task::JoinHandle<()>, String> {
    let client = ApnsClient::new(config)?;
    Ok(tokio::spawn(async move {
        while let Some(wake) = receiver.recv().await {
            let mut unique = HashSet::new();
            for device_token in wake.device_tokens {
                if !unique.insert(device_token.clone()) {
                    continue;
                }
                match client.send_wake(&device_token).await {
                    Ok(ApnsSendOutcome::Accepted) => {}
                    Ok(ApnsSendOutcome::InvalidToken) => {
                        let cleanup_store = store.clone();
                        let cleanup_token = device_token.clone();
                        match tokio::task::spawn_blocking(move || {
                            cleanup_store.remove_push_device_token(&cleanup_token)
                        })
                        .await
                        {
                            Ok(Ok(_)) => {}
                            Ok(Err(error)) => warn!(%error, "could not remove invalid APNs token"),
                            Err(error) => warn!(%error, "APNs invalid-token cleanup task failed"),
                        }
                    }
                    Ok(ApnsSendOutcome::Rejected(reason)) => {
                        warn!(%reason, "APNs rejected a relay wake");
                    }
                    Err(error) => warn!(%error, "APNs relay wake failed"),
                }
            }
        }
        info!("APNs relay wake worker stopped");
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_and_sandbox_endpoints_are_distinct() {
        let base = ApnsConfig {
            key_id: "KID".into(),
            team_id: "TEAM".into(),
            bundle_id: "com.cruisemesh.app".into(),
            private_key_pem: vec![],
            sandbox: false,
        };
        assert_eq!(base.endpoint(), "https://api.push.apple.com");
        assert_eq!(
            ApnsConfig {
                sandbox: true,
                ..base
            }
            .endpoint(),
            "https://api.sandbox.push.apple.com"
        );
    }

    #[test]
    fn wake_payload_contains_only_the_doorbell() {
        let value = serde_json::to_value(WakePayload {
            aps: WakeAps {
                content_available: 1,
            },
            cruisemesh_relay_wake: true,
        })
        .unwrap();
        assert_eq!(value["aps"]["content-available"], 1);
        assert_eq!(value["cruisemesh_relay_wake"], true);
        assert_eq!(value.as_object().unwrap().len(), 2);
    }
}
