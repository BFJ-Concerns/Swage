use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::Serialize;
use uuid::Uuid;

use crate::{
    error::ApiError,
    service::{
        ConversationDetail, ConversationResponse, ConversationService, ConversationSummary,
        CreateProjectRequest, ProjectListResponse, ProjectResponse, ProjectService,
        UpdateProjectRequest,
    },
    state::AppState,
};

// ---------------------------------------------------------------------------
// Health check
// ---------------------------------------------------------------------------

/// `GET /api/health` — lightweight startup and readiness check.
///
/// Returns `{"status": "ok"}` with no database queries or provider calls.
/// Used by process managers (systemd, wrapper scripts) to verify the gateway
/// is up and accepting connections before routing traffic to it.
pub async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({"status": "ok"}))
}

// ---------------------------------------------------------------------------
// Wrapper type for list endpoint
// ---------------------------------------------------------------------------

/// Top-level response body for the list-conversations endpoint.
#[derive(Debug, Serialize)]
pub struct ConversationListResponse {
    pub conversations: Vec<ConversationSummary>,
}

// ---------------------------------------------------------------------------
// Route handlers
// ---------------------------------------------------------------------------

/// `POST /api/conversations` — create a new conversation.
///
/// Returns 201 Created with the new conversation's metadata.
pub async fn create_conversation(
    State(state): State<AppState>,
) -> Result<impl IntoResponse, ApiError> {
    let svc = ConversationService::new(state.storage, state.project_store);
    let resp: ConversationResponse = svc.create_conversation().await?;
    Ok((StatusCode::CREATED, Json(resp)))
}

/// `GET /api/conversations` — list all conversations.
///
/// Returns 200 with `{ conversations: [...] }`.
pub async fn list_conversations(
    State(state): State<AppState>,
) -> Result<Json<ConversationListResponse>, ApiError> {
    let svc = ConversationService::new(state.storage, state.project_store);
    let conversations = svc.list_conversations().await?;
    Ok(Json(ConversationListResponse { conversations }))
}

/// `GET /api/conversations/:id` — get a conversation with full message history.
///
/// Returns 200 with the conversation detail, or 404 if not found.
pub async fn get_conversation(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<ConversationDetail>, ApiError> {
    let svc = ConversationService::new(state.storage, state.project_store);
    let detail = svc.get_conversation(id).await?;
    Ok(Json(detail))
}

/// `GET /api/projects/:project_id/sessions` — list sessions for a project.
///
/// Returns 200 with `{ conversations: [...] }`, or 404 if the project is not found.
pub async fn list_project_sessions(
    State(state): State<AppState>,
    Path(project_id): Path<Uuid>,
) -> Result<Json<ConversationListResponse>, ApiError> {
    let svc = ConversationService::new(state.storage, state.project_store);
    let conversations = svc.list_sessions_by_project(project_id).await?;
    Ok(Json(ConversationListResponse { conversations }))
}

/// `POST /api/projects/:project_id/sessions` — create a session in a project.
///
/// Returns 201 Created with the new session's metadata, or 404 if the project is not found.
pub async fn create_project_session(
    State(state): State<AppState>,
    Path(project_id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let svc = ConversationService::new(state.storage, state.project_store);
    let resp = svc.create_session_in_project(project_id).await?;
    Ok((StatusCode::CREATED, Json(resp)))
}

/// `DELETE /api/conversations/:id` — delete a conversation.
///
/// Returns 204 No Content on success, or 404 if not found.
pub async fn delete_conversation(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let svc = ConversationService::new(state.storage, state.project_store);
    svc.delete_conversation(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Project handlers
// ---------------------------------------------------------------------------

/// `POST /api/projects` — create a new project.
///
/// Returns 201 Created with the project's metadata, or 409/422 on error.
pub async fn create_project(
    State(state): State<AppState>,
    Json(req): Json<CreateProjectRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let svc = ProjectService::new(state.project_store);
    let resp = svc.create_project(req).await?;
    Ok((StatusCode::CREATED, Json(resp)))
}

/// `GET /api/projects` — list all projects.
///
/// Returns 200 with `{ projects: [...] }`.
pub async fn list_projects(
    State(state): State<AppState>,
) -> Result<Json<ProjectListResponse>, ApiError> {
    let svc = ProjectService::new(state.project_store);
    let resp = svc.list_projects().await?;
    Ok(Json(resp))
}

/// `GET /api/projects/:id` — get a project by ID.
///
/// Returns 200 with the project, or 404 if not found.
pub async fn get_project(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<ProjectResponse>, ApiError> {
    let svc = ProjectService::new(state.project_store);
    let resp = svc.get_project(id).await?;
    Ok(Json(resp))
}

/// `PUT /api/projects/:id` — partially update a project.
///
/// Returns 200 with the updated project, 404 if not found, or 422/409 on error.
pub async fn update_project(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateProjectRequest>,
) -> Result<Json<ProjectResponse>, ApiError> {
    let svc = ProjectService::new(state.project_store);
    let resp = svc.update_project(id, req).await?;
    Ok(Json(resp))
}

/// `DELETE /api/projects/:id` — delete a project.
///
/// Returns 204 No Content on success, 404 if not found, or 409 if refusing
/// to delete the default project.
pub async fn delete_project(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let svc = ProjectService::new(state.project_store);
    svc.delete_project(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Integration tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Arc};

    use axum::{
        body::to_bytes,
        http::Request,
        Router,
    };
    use tokio::sync::Mutex;
    use tower::ServiceExt;
    use uuid::Uuid;

    use swage_storage::store::{ProjectStore, SessionStore, Storage};

    use super::*;
    use crate::{
        server::build_router,
        state::AppState,
        turn::tests::{MockProvider, MockStorage},
    };

    // -----------------------------------------------------------------------
    // Test helpers
    // -----------------------------------------------------------------------

    /// Build an axum `Router` backed by an empty `MockStorage`.
    ///
    /// Returns the router and the storage handle so tests can inspect state.
    fn make_router() -> (Router, Arc<MockStorage>) {
        let storage = Arc::new(MockStorage::new());
        let provider = Arc::new(MockProvider::new(vec![]));
        let state = AppState {
            storage: Arc::clone(&storage) as Arc<dyn Storage>,
            project_store: Arc::clone(&storage) as Arc<dyn ProjectStore>,
            provider: provider as Arc<dyn swage_provider::LlmProvider>,
            config: swage_config::ResolvedConfig::default(),
            active_turns: Arc::new(Mutex::new(HashMap::new())),
            active_connections: Arc::new(Mutex::new(std::collections::HashSet::new())),
            tool_dispatcher: Arc::new(crate::tool::ToolDispatcher::allow_all(vec![])),
            sandbox: None,
            activity_tracker: Arc::new(crate::lifecycle::ActivityTracker::new()),
        };
        (build_router(state), storage)
    }

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    // -----------------------------------------------------------------------
    // POST /api/conversations
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn post_conversations_returns_201_with_json() {
        let (router, _storage) = make_router();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/conversations")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = body_json(resp).await;
        assert!(body["id"].is_string(), "response should have a string id");
        assert!(body["project_id"].is_string());
        assert!(body["created_at"].is_string());
        assert!(body["last_activity"].is_string());
    }

    // -----------------------------------------------------------------------
    // GET /api/conversations
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn get_conversations_returns_200_with_array() {
        let (router, _) = make_router();

        // Create two conversations first.
        router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/conversations")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/conversations")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/conversations")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        let list = body["conversations"].as_array().unwrap();
        assert_eq!(list.len(), 2);
    }

    // -----------------------------------------------------------------------
    // GET /api/conversations/:id
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn get_conversation_by_id_returns_200_with_messages() {
        let (router, _) = make_router();

        // Create a conversation.
        let create_resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/conversations")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let create_body = body_json(create_resp).await;
        let id = create_body["id"].as_str().unwrap();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/conversations/{id}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["id"].as_str().unwrap(), id);
        assert!(body["messages"].is_array());
    }

    #[tokio::test]
    async fn get_non_existent_conversation_returns_404() {
        let (router, _) = make_router();
        let missing = Uuid::new_v4();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/conversations/{missing}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body = body_json(resp).await;
        assert_eq!(body["code"].as_str().unwrap(), "not_found");
    }

    // -----------------------------------------------------------------------
    // DELETE /api/conversations/:id
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn delete_conversation_returns_204() {
        let (router, _) = make_router();

        let create_resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/conversations")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let create_body = body_json(create_resp).await;
        let id = create_body["id"].as_str().unwrap();

        let del_resp = router
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/conversations/{id}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(del_resp.status(), StatusCode::NO_CONTENT);
    }

    // -----------------------------------------------------------------------
    // POST /api/projects
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn post_projects_returns_201_with_json() {
        let (router, _) = make_router();
        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/projects")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        r#"{"name":"My Project"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = body_json(resp).await;
        assert!(body["id"].is_string(), "response should have a string id");
        assert!(body["slug"].is_string(), "response should have a slug");
        assert_eq!(body["name"].as_str().unwrap(), "My Project");
    }

    #[tokio::test]
    async fn post_projects_empty_name_returns_422() {
        let (router, _) = make_router();
        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/projects")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(r#"{"name":""}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn post_projects_duplicate_name_returns_409() {
        let (router, _) = make_router();

        // First creation succeeds.
        router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/projects")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(r#"{"name":"Dup"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        // Second with same name must conflict.
        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/projects")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(r#"{"name":"Dup"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    // -----------------------------------------------------------------------
    // GET /api/projects
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn get_projects_returns_200_with_list() {
        let (router, _) = make_router();
        let resp = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/projects")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        // At least the seeded default project.
        assert!(body["projects"].is_array());
    }

    // -----------------------------------------------------------------------
    // GET /api/projects/:id
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn get_project_by_id_returns_200() {
        let (router, _) = make_router();

        let create_resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/projects")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(r#"{"name":"Fetched"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        let created = body_json(create_resp).await;
        let id = created["id"].as_str().unwrap();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/projects/{id}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["id"].as_str().unwrap(), id);
    }

    #[tokio::test]
    async fn get_project_nonexistent_returns_404() {
        let (router, _) = make_router();
        let missing = Uuid::new_v4();
        let resp = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/projects/{missing}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    // -----------------------------------------------------------------------
    // PUT /api/projects/:id
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn put_project_returns_200_with_updated_fields() {
        let (router, _) = make_router();

        let create_resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/projects")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(r#"{"name":"Original"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        let created = body_json(create_resp).await;
        let id = created["id"].as_str().unwrap();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/api/projects/{id}"))
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(r#"{"name":"Renamed"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["name"].as_str().unwrap(), "Renamed");
    }

    #[tokio::test]
    async fn put_project_nonexistent_returns_404() {
        let (router, _) = make_router();
        let missing = Uuid::new_v4();
        let resp = router
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/api/projects/{missing}"))
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(r#"{"name":"X"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    // -----------------------------------------------------------------------
    // DELETE /api/projects/:id
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn delete_project_returns_204() {
        let (router, _) = make_router();

        let create_resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/projects")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(r#"{"name":"Deletable"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        let created = body_json(create_resp).await;
        let id = created["id"].as_str().unwrap();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/projects/{id}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn delete_default_project_returns_409() {
        use crate::service::ProjectService;

        let (router, storage) = make_router();
        // Get the default project's ID.
        let svc = ProjectService::new(Arc::clone(&storage) as Arc<dyn ProjectStore>);
        let default_project = svc.project_store.get_or_create_default().await.unwrap();
        let id = default_project.id;

        let resp = router
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/projects/{id}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    // -----------------------------------------------------------------------
    // GET /api/projects/:project_id/sessions
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn get_project_sessions_returns_200_with_array() {
        let (router, storage) = make_router();

        // Create a named project directly in storage.
        let project = storage
            .create(swage_storage::model::NewProject {
                name: "my-project".into(),
                slug: "my-project".into(),
                workspace_path: None,
            })
            .await
            .unwrap();

        // Add two sessions to that project.
        storage
            .create_session(swage_storage::model::NewSession { project_id: project.id, agent_id: "gateway".to_string() })
            .await
            .unwrap();
        storage
            .create_session(swage_storage::model::NewSession { project_id: project.id, agent_id: "gateway".to_string() })
            .await
            .unwrap();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/projects/{}/sessions", project.id))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        // Gateway wraps sessions in a "conversations" key (frontend-compatible).
        let list = body["conversations"].as_array().unwrap();
        assert_eq!(list.len(), 2);
    }

    #[tokio::test]
    async fn get_project_sessions_scopes_to_project() {
        let (router, storage) = make_router();

        let project_a = storage
            .create(swage_storage::model::NewProject {
                name: "project-a".into(),
                slug: "project-a".into(),
                workspace_path: None,
            })
            .await
            .unwrap();
        let project_b = storage
            .create(swage_storage::model::NewProject {
                name: "project-b".into(),
                slug: "project-b".into(),
                workspace_path: None,
            })
            .await
            .unwrap();

        // One session in A, two in B.
        storage
            .create_session(swage_storage::model::NewSession { project_id: project_a.id, agent_id: "gateway".to_string() })
            .await
            .unwrap();
        storage
            .create_session(swage_storage::model::NewSession { project_id: project_b.id, agent_id: "gateway".to_string() })
            .await
            .unwrap();
        storage
            .create_session(swage_storage::model::NewSession { project_id: project_b.id, agent_id: "gateway".to_string() })
            .await
            .unwrap();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/projects/{}/sessions", project_a.id))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let body = body_json(resp).await;
        let list = body["conversations"].as_array().unwrap();
        // Only sessions belonging to project A.
        assert_eq!(list.len(), 1);
    }

    #[tokio::test]
    async fn get_project_sessions_returns_404_for_unknown_project() {
        let (router, _) = make_router();
        let missing = Uuid::new_v4();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/projects/{missing}/sessions"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body = body_json(resp).await;
        assert_eq!(body["code"].as_str().unwrap(), "not_found");
    }

    // -----------------------------------------------------------------------
    // POST /api/projects/:project_id/sessions
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn post_project_sessions_returns_201_with_json() {
        let (router, storage) = make_router();

        let project = storage
            .create(swage_storage::model::NewProject {
                name: "my-project".into(),
                slug: "my-project".into(),
                workspace_path: None,
            })
            .await
            .unwrap();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/projects/{}/sessions", project.id))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = body_json(resp).await;
        assert!(body["id"].is_string());
        // Session should be associated with the specified project.
        assert_eq!(body["project_id"].as_str().unwrap(), project.id.to_string());
    }

    #[tokio::test]
    async fn post_project_sessions_returns_404_for_unknown_project() {
        let (router, _) = make_router();
        let missing = Uuid::new_v4();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/projects/{missing}/sessions"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body = body_json(resp).await;
        assert_eq!(body["code"].as_str().unwrap(), "not_found");
    }

    #[tokio::test]
    async fn delete_non_existent_conversation_returns_404() {
        let (router, _) = make_router();
        let missing = Uuid::new_v4();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/conversations/{missing}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body = body_json(resp).await;
        assert_eq!(body["code"].as_str().unwrap(), "not_found");
    }
}
