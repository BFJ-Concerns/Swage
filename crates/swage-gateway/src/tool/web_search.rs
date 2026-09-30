//! Web search tool — searches the web via a configurable search provider.
//!
//! Currently supports SearxNG (self-hosted meta-search engine). The provider
//! type and endpoint come from the resolved config's
//! `[tools.search]` section. Returns structured results as plain text.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use swage_config::SearchProviderConfig;
use swage_core::ToolError;
use swage_tool::context::ToolContext;

use crate::tool::Tool;

// ---------------------------------------------------------------------------
// Search provider abstraction
// ---------------------------------------------------------------------------

/// A single search result returned by any provider.
struct SearchResult {
    title: String,
    url: String,
    snippet: String,
}

/// Internal trait so additional providers can be added without touching the
/// tool's execute path.
#[async_trait]
trait SearchProvider: Send + Sync {
    async fn search(&self, query: &str, max_results: usize) -> Result<Vec<SearchResult>, String>;
}

// ---------------------------------------------------------------------------
// SearxNG provider
// ---------------------------------------------------------------------------

struct SearxNgProvider {
    base_url: String,
    client: reqwest::Client,
}

/// Wire format returned by the SearxNG JSON API.
#[derive(Deserialize)]
struct SearxNgResponse {
    results: Vec<SearxNgResult>,
}

#[derive(Deserialize)]
struct SearxNgResult {
    title: String,
    url: String,
    #[serde(default)]
    content: String,
}

#[async_trait]
impl SearchProvider for SearxNgProvider {
    async fn search(&self, query: &str, max_results: usize) -> Result<Vec<SearchResult>, String> {
        let url = format!("{}/search", self.base_url.trim_end_matches('/'));

        let response = self
            .client
            .get(&url)
            .query(&[("q", query), ("format", "json"), ("pageno", "1")])
            .send()
            .await
            .map_err(|e| format!("Failed to reach search provider: {e}"))?;

        if !response.status().is_success() {
            return Err(format!(
                "Search provider returned HTTP {}",
                response.status()
            ));
        }

        let data: SearxNgResponse = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse search response: {e}"))?;

        let results = data
            .results
            .into_iter()
            .take(max_results)
            .map(|r| SearchResult {
                title: r.title,
                url: r.url,
                snippet: r.content,
            })
            .collect();

        Ok(results)
    }
}

// ---------------------------------------------------------------------------
// Tool implementation
// ---------------------------------------------------------------------------

pub struct WebSearchTool {
    /// None means web search is not configured — the tool returns an error.
    pub provider_config: Option<SearchProviderConfig>,
}

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }

    fn display_hints(&self) -> swage_tool::tool::ToolDisplayHints {
        swage_tool::tool::ToolDisplayHints {
            summary_param: Some("query"),
        }
    }

    fn description(&self) -> &str {
        "Search the web for information. Returns titles, URLs, and snippets."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query."
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum number of results to return (default 10).",
                    "default": 10
                }
            },
            "required": ["query"]
        })
    }

    fn requires_device_execution(&self) -> bool {
        true
    }

    async fn execute(&self, params: Value, _ctx: &dyn ToolContext) -> Result<Value, ToolError> {
        // Require search to be configured.
        let config = self
            .provider_config
            .as_ref()
            .ok_or_else(|| ToolError::ExecutionFailed {
                name: self.name().to_owned(),
                message: "Web search is not configured. Set [tools.search] in your topic config."
                    .to_owned(),
            })?;

        let query = params["query"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidParameters {
                name: self.name().to_owned(),
                message: "Missing required argument: query".to_owned(),
            })?
            .to_string();

        let max_results = params["max_results"].as_u64().unwrap_or(10) as usize;

        // Build a provider from config. Only SearxNG is supported for now.
        let provider: Box<dyn SearchProvider> = match config.provider_type.as_str() {
            "searxng" => Box::new(SearxNgProvider {
                base_url: config.base_url.clone(),
                client: reqwest::Client::new(),
            }),
            unsupported => {
                return Err(ToolError::ExecutionFailed {
                    name: self.name().to_owned(),
                    message: swage_config::validation::unsupported_search_provider_message(
                        unsupported,
                    ),
                });
            }
        };

        let results = provider.search(&query, max_results).await.map_err(|msg| {
            ToolError::ExecutionFailed {
                name: self.name().to_owned(),
                message: msg,
            }
        })?;

        if results.is_empty() {
            return Ok(json!({"output": format!("No results found for: {query}")}));
        }

        // Format results as structured plain text.
        let content = results
            .iter()
            .map(|r| format!("## {}\n{}\n{}\n", r.title, r.url, r.snippet))
            .collect::<Vec<_>>()
            .join("\n");

        Ok(json!({"output": content}))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use swage_tool::context::MockToolContext;

    fn unconfigured_tool() -> WebSearchTool {
        WebSearchTool {
            provider_config: None,
        }
    }

    fn configured_tool() -> WebSearchTool {
        WebSearchTool {
            provider_config: Some(SearchProviderConfig {
                provider_type: "searxng".to_string(),
                base_url: "http://localhost:8080".to_string(),
            }),
        }
    }

    fn ctx() -> MockToolContext {
        MockToolContext::new("agent-test", "sess-test")
    }

    #[tokio::test]
    async fn search_provider_selects_only_searxng() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let requests = Arc::new(AtomicUsize::new(0));
        let seen = requests.clone();
        let app = axum::Router::new().route("/search", axum::routing::get(move || {
            let seen = seen.clone();
            async move {
                seen.fetch_add(1, Ordering::SeqCst);
                axum::Json(json!({"results": [{"title": "Backend result", "url": "https://result.invalid", "content": "Search response"}]}))
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let mut tool = WebSearchTool {
            provider_config: Some(
                serde_json::from_value(json!({
                    "provider_type": "brave", "base_url": base_url
                }))
                .unwrap(),
            ),
        };
        let result = tool.execute(json!({"query": "test"}), &ctx()).await;
        let error = result.expect_err("unsupported provider must be refused before HTTP");
        assert!(error.to_string().contains("brave"));
        assert!(error.to_string().contains("searxng"));
        assert_eq!(requests.load(Ordering::SeqCst), 0);
        tool.provider_config.as_mut().unwrap().provider_type = "searxng".into();
        let result = tool.execute(json!({"query": "test"}), &ctx()).await;
        server.abort();
        assert!(
            result.unwrap()["output"]
                .as_str()
                .unwrap()
                .contains("Backend result")
        );
        assert_eq!(requests.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn returns_config_error_when_unconfigured() {
        let err = unconfigured_tool()
            .execute(json!({ "query": "rust programming" }), &ctx())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("[tools.search]"), "got: {err}");
    }

    #[tokio::test]
    async fn returns_error_for_missing_query() {
        let err = configured_tool()
            .execute(json!({}), &ctx())
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidParameters { .. }));
        assert!(err.to_string().contains("query"), "got: {err}");
    }

    #[tokio::test]
    async fn tool_metadata() {
        let tool = configured_tool();
        assert_eq!(tool.name(), "web_search");
        assert!(tool.description().contains("web"));
        let params = tool.parameters();
        assert!(params["properties"]["query"].is_object());
        assert!(params["properties"]["max_results"].is_object());
    }

    #[tokio::test]
    async fn formats_results_correctly() {
        // Test the formatting logic directly.
        let results = [SearchResult {
            title: "Rust Programming Language".to_string(),
            url: "https://www.rust-lang.org".to_string(),
            snippet: "A systems programming language".to_string(),
        }];

        let content = results
            .iter()
            .map(|r| format!("## {}\n{}\n{}\n", r.title, r.url, r.snippet))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(content.contains("## Rust Programming Language"));
        assert!(content.contains("https://www.rust-lang.org"));
        assert!(content.contains("A systems programming language"));
    }

    #[tokio::test]
    async fn gracefully_fails_when_provider_unreachable() {
        let tool = WebSearchTool {
            provider_config: Some(SearchProviderConfig {
                provider_type: "searxng".to_string(),
                base_url: "http://127.0.0.1:19999".to_string(),
            }),
        };
        let err = tool
            .execute(json!({ "query": "test" }), &ctx())
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
    }
}
