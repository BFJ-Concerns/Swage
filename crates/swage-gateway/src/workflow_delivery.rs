//! Grant-backed external effects available only to workflow scripts.

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use reqwest::header::{AUTHORIZATION, HeaderValue};
use serde_json::Value;
use swage_storage::Storage;
use swage_workflow::{EffectKind, EffectPayload, EffectReceipt};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::secret::SecretManager;

#[async_trait]
pub(crate) trait DeliveryCapability: Send + Sync {
    fn kind(&self) -> &'static str;
    fn effect_kind(&self) -> EffectKind;

    async fn deliver(
        &self,
        key: Uuid,
        payload: EffectPayload,
        effect_seq: u64,
        cancellation: CancellationToken,
    ) -> Result<EffectReceipt, String>;

    async fn probe(
        &self,
        key: Uuid,
        payload: &EffectPayload,
    ) -> Result<Option<EffectReceipt>, String>;
}

pub(crate) async fn capability_from_grant(
    storage: Arc<dyn Storage>,
    grant: Option<&Value>,
    requested_target: Option<&str>,
) -> Result<Arc<dyn DeliveryCapability>, String> {
    let targets = grant
        .and_then(Value::as_array)
        .ok_or_else(|| "DeliveryNotGrantedError: script has no delivery targets".to_string())?;
    let target = targets
        .iter()
        .find(|target| {
            target.get("kind").and_then(Value::as_str) == Some("webhook")
                && requested_target
                    .is_none_or(|name| target.get("name").and_then(Value::as_str) == Some(name))
        })
        .ok_or_else(|| {
            "DeliveryNotGrantedError: requested delivery target is not granted".to_string()
        })?;
    Ok(Arc::new(
        WebhookDeliveryCapability::from_grant(storage, target).await?,
    ))
}

struct WebhookDeliveryCapability {
    client: reqwest::Client,
    url: String,
    authorization: Option<HeaderValue>,
}

impl WebhookDeliveryCapability {
    async fn from_grant(storage: Arc<dyn Storage>, target: &Value) -> Result<Self, String> {
        let url = target
            .get("url")
            .and_then(Value::as_str)
            .ok_or_else(|| "webhook grant is missing url".to_string())?
            .to_string();
        let authorization = match target.get("secret_ref").and_then(Value::as_str) {
            Some(reference) => {
                let secret = SecretManager::new(storage)
                    .resolve(reference)
                    .await
                    .map_err(|error| {
                        format!("webhook auth secret could not be resolved: {error}")
                    })?;
                Some(
                    HeaderValue::from_str(&format!("Bearer {}", secret.value)).map_err(|_| {
                        "webhook auth secret is not a valid header value".to_string()
                    })?,
                )
            }
            None => None,
        };
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|error| error.to_string())?;
        Ok(Self {
            client,
            url,
            authorization,
        })
    }
}

#[async_trait]
impl DeliveryCapability for WebhookDeliveryCapability {
    fn kind(&self) -> &'static str {
        "webhook"
    }

    fn effect_kind(&self) -> EffectKind {
        EffectKind::IdempotencyKey
    }

    async fn deliver(
        &self,
        key: Uuid,
        payload: EffectPayload,
        effect_seq: u64,
        cancellation: CancellationToken,
    ) -> Result<EffectReceipt, String> {
        let mut request = self
            .client
            .post(&self.url)
            .header("Idempotency-Key", key.to_string())
            .json(&serde_json::json!({
                "effect_id": key,
                "content": payload.content,
                "metadata": payload.metadata,
            }));
        if let Some(value) = &self.authorization {
            request = request.header(AUTHORIZATION, value.clone());
        }
        let response = tokio::select! {
            response = request.send() => response.map_err(|error| error.to_string())?,
            () = cancellation.cancelled() => return Err("WorkflowCancelledError: delivery cancelled".into()),
        };
        if !response.status().is_success() {
            return Err(format!("webhook returned {}", response.status()));
        }
        Ok(EffectReceipt {
            delivered: true,
            target: self.kind().into(),
            effect_seq,
        })
    }

    async fn probe(
        &self,
        _key: Uuid,
        _payload: &EffectPayload,
    ) -> Result<Option<EffectReceipt>, String> {
        // Idempotency-key capabilities never probe; the receiver owns duplicate
        // suppression for the persisted key.
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use axum::{Router, http::HeaderMap, routing::post};
    use swage_storage::{SqliteStorage, Storage};

    use super::*;
    use crate::secret::CreateSecretRequest;

    #[tokio::test(flavor = "multi_thread")]
    async fn webhook_resolves_bearer_auth_from_governed_secret_reference() {
        let variable = format!("SWAGE_WORKFLOW_WEBHOOK_TEST_{}", Uuid::new_v4());
        // The grant contains only the governed reference. The environment-backed
        // secret is resolved by SecretManager, never read by the capability.
        unsafe { std::env::set_var(&variable, "test-token") };
        let storage: Arc<dyn Storage> = Arc::new(SqliteStorage::in_memory().unwrap());
        let secret = SecretManager::new(Arc::clone(&storage))
            .create_secret(CreateSecretRequest {
                name: "Workflow webhook token".into(),
                material_kind: "provider_key".into(),
                owner_kind: "workflow".into(),
                backend_kind: "env".into(),
                scope_kind: "gateway".into(),
                scope_id: None,
                metadata: None,
                value: None,
                external_reference: Some(variable.clone()),
            })
            .await
            .unwrap();

        let seen = Arc::new(Mutex::new(None::<String>));
        let receiver_seen = Arc::clone(&seen);
        let app = Router::new().route(
            "/deliver",
            post(move |headers: HeaderMap| {
                let receiver_seen = Arc::clone(&receiver_seen);
                async move {
                    *receiver_seen.lock().unwrap() = headers
                        .get(AUTHORIZATION)
                        .and_then(|value| value.to_str().ok())
                        .map(str::to_string);
                    "ok"
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let grant = serde_json::json!([{
            "kind": "webhook",
            "name": "public",
            "url": format!("http://{address}/deliver"),
            "secret_ref": secret.reference,
        }]);
        let capability = capability_from_grant(storage, Some(&grant), Some("public"))
            .await
            .unwrap();
        capability
            .deliver(
                Uuid::new_v4(),
                EffectPayload {
                    content: "reviewed".into(),
                    metadata: None,
                    target: Some("public".into()),
                },
                0,
                CancellationToken::new(),
            )
            .await
            .unwrap();

        assert_eq!(seen.lock().unwrap().as_deref(), Some("Bearer test-token"));
        unsafe { std::env::remove_var(variable) };
    }
}
