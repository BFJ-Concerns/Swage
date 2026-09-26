#![allow(clippy::unwrap_used)]
//! The gateway hosts the platform's own documentation: the compiled-in
//! agent-facing set is listed and read over the live HTTP surface, and the
//! hosted bytes are the repository's.

use std::sync::Arc;

use axum::Router;
use reqwest::Method;
use swage_gateway::server::build_router;
use swage_gateway::state::AppState;
use swage_storage::SqliteStorage;

use super::support::{CapturingProvider, live_http_json, start_live_server};

fn router() -> Router {
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let (provider, _requests) = CapturingProvider::new("stub", vec![]);
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        storage as Arc<dyn swage_storage::store::TopicStore>,
        Arc::new(provider),
        swage_config::ResolvedConfig::default(),
        swage_gateway::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    build_router(state)
}

fn repository_document_names() -> Vec<String> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../agent-docs");
    let mut names = std::fs::read_dir(root)
        .unwrap()
        .map(|entry| {
            entry
                .unwrap()
                .path()
                .file_stem()
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned()
        })
        .collect::<Vec<_>>();
    names.sort();
    names
}

#[tokio::test(flavor = "multi_thread")]
async fn the_gateway_hosts_every_agent_facing_document_over_http() {
    let (address, _server) = start_live_server(router()).await;

    let (status, body) = live_http_json(address, Method::GET, "/api/docs", None).await;
    assert_eq!(status, reqwest::StatusCode::OK);
    let mut listed = body["documents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["name"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    listed.sort();
    assert_eq!(listed, repository_document_names());
    let first_session = body["documents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["name"] == "first-session")
        .unwrap();
    assert_eq!(first_session["title"], "First session");
    assert!(
        !first_session["description"].as_str().unwrap().is_empty(),
        "the listing carries the document's description"
    );

    let (status, body) =
        live_http_json(address, Method::GET, "/api/docs/first-session", None).await;
    assert_eq!(status, reqwest::StatusCode::OK);
    let repository_bytes = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../agent-docs/first-session.md"),
    )
    .unwrap();
    assert_eq!(body["content"].as_str().unwrap(), repository_bytes);
    assert_eq!(body["title"], "First session");

    let (status, body) = live_http_json(address, Method::GET, "/api/docs/no-such-doc", None).await;
    assert_eq!(status, reqwest::StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "not_found");
    assert!(body["message"].as_str().unwrap().contains("no-such-doc"));
}
