//! Grant-backed external effects available only to workflow scripts.

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use reqwest::header::{AUTHORIZATION, HeaderValue};
use serde_json::Value;
use swage_protocol::client::page::WritePageModuleRequest;
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
    space_id: Uuid,
    storage: Arc<dyn Storage>,
    grant: Option<&Value>,
    requested_target: Option<&str>,
) -> Result<Arc<dyn DeliveryCapability>, String> {
    let targets = grant
        .and_then(Value::as_array)
        .ok_or_else(|| "DeliveryNotGrantedError: script has no delivery targets".to_string())?;
    // Untargeted workflow delivery historically selects the webhook capability.
    // A page is therefore opt-in through its named grant, rather than changing
    // what an existing workflow does merely because a page grant is added first.
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
        Some("page") => Ok(Arc::new(
            PageDeliveryCapability::from_grant(
                state.ok_or_else(|| "page delivery needs gateway state".to_string())?,
                space_id,
                target,
            )
            .await?,
        )),
        _ => Err("DeliveryNotGrantedError: requested delivery target is not granted".into()),
    }
}

struct PageDeliveryCapability {
    state: AppState,
    page_id: Uuid,
}

impl PageDeliveryCapability {
    async fn from_grant(state: AppState, space_id: Uuid, target: &Value) -> Result<Self, String> {
        let page_id = target
            .get("page_id")
            .and_then(Value::as_str)
            .ok_or_else(|| "page grant is missing page_id".to_string())?
            .parse()
            .map_err(|_| "page grant has an invalid page_id".to_string())?;
        let page = state
            .storage
            .get_page(page_id)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("no page with id {page_id}"))?;
        if page.space_id != space_id {
            return Err("page delivery target is not in the workflow's space".into());
        }
        Ok(Self { state, page_id })
    }
}

#[async_trait]
impl DeliveryCapability for PageDeliveryCapability {
    fn kind(&self) -> &'static str {
        "page"
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
            result = crate::page::write_page_module(
                &self.state,
                self.page_id,
                WritePageModuleRequest {
                    module_id: None,
                    kind: Some("document".into()),
                    source: payload.content,
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
    use swage_protocol::client::{page::CreatePageRequest, websocket::ServerEvent};
    use swage_storage::{NewSpace, PageStore, SpaceStore, SqliteStorage, Storage};

    use super::*;
    use crate::secret::CreateSecretRequest;

    fn state(storage: Arc<SqliteStorage>) -> AppState {
        AppState::new(
            Arc::clone(&storage) as Arc<dyn Storage>,
            storage as Arc<dyn SpaceStore>,
            Arc::new(crate::turn::tests::MockProvider::new(vec![])),
            swage_config::ResolvedConfig::default(),
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
        )
    }

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
    async fn page_delivery_writes_a_page_module_and_announces_its_location() {
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let space = SpaceStore::create(
            storage.as_ref(),
            NewSpace {
                name: "Delivery".into(),
                slug: "delivery".into(),
            },
        )
        .await
        .unwrap();
        let state = state(Arc::clone(&storage));
        let page = crate::page::create_page(
            &state,
            space.id,
            CreatePageRequest {
                title: "Daily delivery".into(),
                template_id: None,
            },
        )
        .await
        .unwrap();
        let mut events = state.broadcast.subscribe();
        let grant = serde_json::json!([{
            "kind": "page",
            "name": "daily-page",
            "page_id": page.id,
        }]);
        let capability = capability_from_grant(
            Some(state.clone()),
            space.id,
            Arc::clone(&storage) as Arc<dyn Storage>,
            Some(&grant),
            Some("daily-page"),
        )
        .await
        .unwrap();

        assert_eq!(capability.kind(), "page");
        assert_eq!(capability.effect_kind(), EffectKind::CheckBeforeRedo);
        let receipt = capability
            .deliver(
                Uuid::new_v4(),
                EffectPayload {
                    content: "# Delivered\n\nThe scheduled result".into(),
                    metadata: None,
                    target: Some("daily-page".into()),
                },
                7,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(receipt.target, "page");
        assert_eq!(receipt.effect_seq, 7);

        let modules = storage.list_page_modules(page.id).await.unwrap();
        assert_eq!(modules.len(), 1);
        assert_eq!(modules[0].source, "# Delivered\n\nThe scheduled result");
        let event = events
            .recv()
            .await
            .expect("page delivery broadcasts a refetch signal");
        assert!(matches!(
            event,
            ServerEvent::PageUpdated {
                page_id,
                module_id: Some(_),
            } if page_id == page.id
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn page_delivery_refuses_a_page_outside_the_workflow_space() {
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let workflow_space = SpaceStore::create(
            storage.as_ref(),
            NewSpace {
                name: "Workflow".into(),
                slug: "workflow".into(),
            },
        )
        .await
        .unwrap();
        let other_space = SpaceStore::create(
            storage.as_ref(),
            NewSpace {
                name: "Other".into(),
                slug: "other".into(),
            },
        )
        .await
        .unwrap();
        let state = state(Arc::clone(&storage));
        let page = crate::page::create_page(
            &state,
            other_space.id,
            CreatePageRequest {
                title: "Other page".into(),
                template_id: None,
            },
        )
        .await
        .unwrap();
        let grant = serde_json::json!([{
            "kind": "page",
            "name": "other-page",
            "page_id": page.id,
        }]);

        let result = capability_from_grant(
            Some(state),
            workflow_space.id,
            Arc::clone(&storage) as Arc<dyn Storage>,
            Some(&grant),
            Some("other-page"),
        )
        .await;
        let error = match result {
            Ok(_) => panic!("a workflow cannot deliver to another space's page"),
            Err(error) => error,
        };
        assert!(error.contains("workflow's space"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn untargeted_delivery_keeps_selecting_the_webhook_grant() {
        let storage: Arc<dyn Storage> = Arc::new(SqliteStorage::in_memory().unwrap());
        let grant = serde_json::json!([
            {
                "kind": "page",
                "name": "first-page",
                "page_id": Uuid::new_v4(),
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
