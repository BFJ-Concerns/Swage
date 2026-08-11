//! Knowledge-base results returned by model-facing tools.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct KnowledgeSearchResult {
    pub observation_id: Uuid,
    pub version: u32,
    pub source: Option<String>,
    pub retrieved_at: String,
    pub passage: String,
    /// Byte offset of `passage` when it can be located in the derivation.
    #[ts(optional)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct KnowledgeSearchResponse {
    pub results: Vec<KnowledgeSearchResult>,
    pub guidance: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct OrientationCardResult {
    pub card_id: Uuid,
    pub title: String,
    pub content: String,
    pub authored_by: String,
    pub observation_ids: Vec<Uuid>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct OrientationFindResponse {
    pub cards: Vec<OrientationCardResult>,
    pub guidance: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlocated_search_passage_omits_offset_instead_of_claiming_zero() {
        let response = KnowledgeSearchResponse {
            results: vec![KnowledgeSearchResult {
                observation_id: Uuid::nil(),
                version: 1,
                source: None,
                retrieved_at: "1970-01-01T00:00:00+00:00".into(),
                passage: "search-produced passage".into(),
                offset: None,
            }],
            guidance: "test".into(),
        };

        let value = serde_json::to_value(response).expect("search response serialises");
        assert_eq!(value["results"][0]["passage"], "search-produced passage");
        assert!(
            value["results"][0].get("offset").is_none(),
            "an unlocated passage must not assert any offset"
        );
    }
}
