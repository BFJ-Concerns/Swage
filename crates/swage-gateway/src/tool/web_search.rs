//! Web search tool — searches the web via a configurable search provider.
//!
//! Currently supports SearxNG (self-hosted meta-search engine). The provider
//! type, endpoint, and optional API key come from the resolved config's
//! `[tools.search]` section. Returns structured results as plain text.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use swage_config::SearchProviderConfig;
use crate::tool::{Tool, ToolExecutionResult};

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
    async fn search(
        &self,
        query: &str,
        max_results: usize,
    ) -> Result<Vec<SearchResult>, String>;
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
    async fn search(
        &self,
        query: &str,
        max_results: usize,
    ) -> Result<Vec<SearchResult>, String> {
        let url = format!("{}/search", self.base_url.trim_end_matches('/'));

        let response = self
            .client
            .get(&url)
            .query(&[
                ("q", query),
                ("format", "json"),
                ("pageno", "1"),
            ])
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

    fn description(&self) -> &str {
        "Search the web for information. Returns titles, URLs, and snippets."
    }

    fn parameters(&self) -> serde_json::Value {
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

    async fn execute(&self, arguments: serde_json::Value) -> ToolExecutionResult {
        // Require search to be configured.
        let config = match &self.provider_config {
            Some(c) => c,
            None => {
                return ToolExecutionResult {
                    content: "Web search is not configured. Set [tools.search] in your project config.".into(),
                    is_error: true,
                    truncated: false,
                    total_bytes: None,
                }
            }
        };

        let query = match arguments["query"].as_str() {
            Some(q) => q.to_string(),
            None => {
                return ToolExecutionResult {
                    content: "Missing required argument: query".into(),
                    is_error: true,
                    truncated: false,
                    total_bytes: None,
                }
            }
        };

        let max_results = arguments["max_results"]
            .as_u64()
            .unwrap_or(10) as usize;

        // Build a provider from config. Only SearxNG is supported for now.
        let provider: Box<dyn SearchProvider> = match config.provider_type.as_str() {
            "searxng" | _ => Box::new(SearxNgProvider {
                base_url: config.base_url.clone(),
                client: reqwest::Client::new(),
            }),
        };

        let results = match provider.search(&query, max_results).await {
            Ok(r) => r,
            Err(msg) => {
                return ToolExecutionResult {
                    content: msg,
                    is_error: true,
                    truncated: false,
                    total_bytes: None,
                }
            }
        };

        if results.is_empty() {
            return ToolExecutionResult {
                content: format!("No results found for: {query}"),
                is_error: false,
                truncated: false,
                total_bytes: None,
            };
        }

        // Format results as structured plain text.
        let content = results
            .iter()
            .map(|r| format!("## {}\n{}\n{}\n", r.title, r.url, r.snippet))
            .collect::<Vec<_>>()
            .join("\n");

        ToolExecutionResult {
            content,
            is_error: false,
            truncated: false,
            total_bytes: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn unconfigured_tool() -> WebSearchTool {
        WebSearchTool { provider_config: None }
    }

    fn configured_tool() -> WebSearchTool {
        WebSearchTool {
            provider_config: Some(SearchProviderConfig {
                provider_type: "searxng".to_string(),
                base_url: "http://localhost:8080".to_string(),
                api_key: None,
            }),
        }
    }

    #[tokio::test]
    async fn returns_config_error_when_unconfigured() {
        let result = unconfigured_tool()
            .execute(json!({ "query": "rust programming" }))
            .await;
        assert!(result.is_error);
        assert!(
            result.content.contains("[tools.search]"),
            "got: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn returns_error_for_missing_query() {
        let result = configured_tool().execute(json!({})).await;
        assert!(result.is_error);
        assert!(result.content.contains("query"), "got: {}", result.content);
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
        // Test the formatting logic by simulating what a provider would return.
        // We can't call the real SearxNG in unit tests, but we can verify the
        // format directly via the result formatting code.
        let results = vec![
            SearchResult {
                title: "Rust Programming Language".to_string(),
                url: "https://www.rust-lang.org".to_string(),
                snippet: "A systems programming language".to_string(),
            },
        ];

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
        // Point at a port nothing is listening on — should fail gracefully, not panic.
        let tool = WebSearchTool {
            provider_config: Some(SearchProviderConfig {
                provider_type: "searxng".to_string(),
                base_url: "http://127.0.0.1:19999".to_string(),
                api_key: None,
            }),
        };
        let result = tool.execute(json!({ "query": "test" })).await;
        assert!(result.is_error);
        // Should report a connection error, not panic.
        assert!(!result.content.is_empty());
    }
}
