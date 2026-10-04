//! Grant-backed external effects available only to workflow scripts.

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use reqwest::header::{AUTHORIZATION, HeaderValue};
use serde_json::Value;
use swage_protocol::client::surface::WriteSurfaceModuleRequest;
use swage_storage::Storage;
use swage_workflow::{EffectKind, EffectPayload, EffectReceipt};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{secret::SecretManager, state::AppState};

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
    state: Option<AppState>,
    topic_id: Uuid,
    storage: Arc<dyn Storage>,
    grant: Option<&Value>,
    requested_target: Option<&str>,
) -> Result<Arc<dyn DeliveryCapability>, String> {
    let targets = grant
        .and_then(Value::as_array)
        .ok_or_else(|| "DeliveryNotGrantedError: script has no delivery targets".to_string())?;
    // Untargeted workflow delivery historically selects the webhook capability.
    // A surface is therefore opt-in through its named grant, rather than changing
    // what an existing workflow does merely because a surface grant is added first.
    let target = targets
        .iter()
        .find(|target| match requested_target {
            Some(name) => target.get("name").and_then(Value::as_str) == Some(name),
            None => target.get("kind").and_then(Value::as_str) == Some("webhook"),
        })
        .ok_or_else(|| {
            "DeliveryNotGrantedError: requested delivery target is not granted".to_string()
        })?;
    match target.get("kind").and_then(Value::as_str) {
        Some("webhook") => Ok(Arc::new(
            WebhookDeliveryCapability::from_grant(storage, target).await?,
        )),
        Some("surface") => Ok(Arc::new(
            SurfaceDeliveryCapability::from_grant(
                state.ok_or_else(|| "surface delivery needs gateway state".to_string())?,
                topic_id,
                target,
            )
            .await?,
        )),
        _ => Err("DeliveryNotGrantedError: requested delivery target is not granted".into()),
    }
}

struct SurfaceDeliveryCapability {
    state: AppState,
    surface_id: Uuid,
}

impl SurfaceDeliveryCapability {
    async fn from_grant(state: AppState, topic_id: Uuid, target: &Value) -> Result<Self, String> {
        let surface_id = target
            .get("surface_id")
            .and_then(Value::as_str)
            .ok_or_else(|| "surface grant is missing surface_id".to_string())?
            .parse()
            .map_err(|_| "surface grant has an invalid surface_id".to_string())?;
        let surface = state
            .storage
            .get_page(surface_id)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("no surface with id {surface_id}"))?;
        // A surface belongs to a session; a run may deliver to one whose
        // session is in the run's topic — the run's own dispatching
        // conversation, or another session working the same topic.
        let owner = state
            .storage
            .get_session(surface.session_id)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("surface {surface_id} has no session"))?;
        if owner.topic_id != topic_id {
            return Err("surface delivery target is not in the workflow's topic".into());
        }
        Ok(Self { state, surface_id })
    }
}

#[async_trait]
impl DeliveryCapability for SurfaceDeliveryCapability {
    fn kind(&self) -> &'static str {
        "surface"
    }

    fn effect_kind(&self) -> EffectKind {
        EffectKind::CheckBeforeRedo
    }

    async fn deliver(
        &self,
        _key: Uuid,
        payload: EffectPayload,
        effect_seq: u64,
        cancellation: CancellationToken,
    ) -> Result<EffectReceipt, String> {
        tokio::select! {
            result = crate::surface::write_surface_module(
                &self.state,
                self.surface_id,
                WriteSurfaceModuleRequest {
                    module_id: None,
                    kind: Some("document".into()),
                    source: payload.content,
                    expected_revision: None,
                },
                "workflow delivery",
            ) => result.map_err(|error| error.to_string())?,
            () = cancellation.cancelled() => return Err("WorkflowCancelledError: delivery cancelled".into()),
        };
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
        Ok(None)
    }
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
    use swage_protocol::client::{surface::CreateSurfaceRequest, websocket::ServerEvent};
    use swage_storage::{
        NewSession, NewTopic, SessionStore, SqliteStorage, Storage, SurfaceStore, TopicStore,
    };

    async fn session_in(storage: &SqliteStorage, topic_id: Uuid) -> Uuid {
        storage
            .create_session(NewSession {
                ..swage_storage::fixtures::new_session(topic_id)
            })
            .await
            .unwrap()
            .id
    }

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
        let capability =
            capability_from_grant(None, Uuid::new_v4(), storage, Some(&grant), Some("public"))
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

    #[tokio::test(flavor = "multi_thread")]
    async fn surface_delivery_writes_a_surface_module_and_announces_its_location() {
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let topic = TopicStore::create(
            storage.as_ref(),
            NewTopic {
                name: "Delivery".into(),
                slug: "delivery".into(),
            },
        )
        .await
        .unwrap();
        let state = crate::test_support::state(Arc::clone(&storage));
        let session = session_in(&storage, topic.id).await;
        let surface = crate::surface::create_surface(
            &state,
            session,
            CreateSurfaceRequest {
                title: "Daily delivery".into(),
                library_entry_id: None,
            },
        )
        .await
        .unwrap();
        let mut events = state.broadcast.subscribe();
        let grant = serde_json::json!([{
            "kind": "surface",
            "name": "daily-surface",
            "surface_id": surface.id,
        }]);
        let capability = capability_from_grant(
            Some(state.clone()),
            topic.id,
            Arc::clone(&storage) as Arc<dyn Storage>,
            Some(&grant),
            Some("daily-surface"),
        )
        .await
        .unwrap();

        assert_eq!(capability.kind(), "surface");
        assert_eq!(capability.effect_kind(), EffectKind::CheckBeforeRedo);
        let receipt = capability
            .deliver(
                Uuid::new_v4(),
                EffectPayload {
                    content: "# Delivered\n\nThe scheduled result".into(),
                    metadata: None,
                    target: Some("daily-surface".into()),
                },
                7,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(receipt.target, "surface");
        assert_eq!(receipt.effect_seq, 7);

        let modules = storage.list_surface_modules(surface.id).await.unwrap();
        assert_eq!(modules.len(), 1);
        assert_eq!(modules[0].source, "# Delivered\n\nThe scheduled result");
        let event = events
            .recv()
            .await
            .expect("surface delivery broadcasts a refetch signal");
        assert!(matches!(
            event,
            ServerEvent::SurfaceUpdated {
                surface_id,
                module_id: Some(_),
            } if surface_id == surface.id
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn surface_delivery_refuses_a_surface_outside_the_workflow_topic() {
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let workflow_topic = TopicStore::create(
            storage.as_ref(),
            NewTopic {
                name: "Workflow".into(),
                slug: "workflow".into(),
            },
        )
        .await
        .unwrap();
        let other_topic = TopicStore::create(
            storage.as_ref(),
            NewTopic {
                name: "Other".into(),
                slug: "other".into(),
            },
        )
        .await
        .unwrap();
        let state = crate::test_support::state(Arc::clone(&storage));
        let session = session_in(&storage, other_topic.id).await;
        let surface = crate::surface::create_surface(
            &state,
            session,
            CreateSurfaceRequest {
                title: "Other surface".into(),
                library_entry_id: None,
            },
        )
        .await
        .unwrap();
        let grant = serde_json::json!([{
            "kind": "surface",
            "name": "other-surface",
            "surface_id": surface.id,
        }]);

        let result = capability_from_grant(
            Some(state),
            workflow_topic.id,
            Arc::clone(&storage) as Arc<dyn Storage>,
            Some(&grant),
            Some("other-surface"),
        )
        .await;
        let error = match result {
            Ok(_) => panic!("a workflow cannot deliver to another topic's surface"),
            Err(error) => error,
        };
        assert!(error.contains("workflow's topic"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn untargeted_delivery_keeps_selecting_the_webhook_grant() {
        let storage: Arc<dyn Storage> = Arc::new(SqliteStorage::in_memory().unwrap());
        let grant = serde_json::json!([
            {
                "kind": "surface",
                "name": "first-surface",
                "surface_id": Uuid::new_v4(),
            },
            {
                "kind": "webhook",
                "name": "existing-webhook",
                "url": "http://127.0.0.1:1/deliver",
            },
        ]);

        let capability = capability_from_grant(None, Uuid::new_v4(), storage, Some(&grant), None)
            .await
            .unwrap();
        assert_eq!(capability.kind(), "webhook");
    }
}
