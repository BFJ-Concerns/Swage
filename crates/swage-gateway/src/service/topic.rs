use std::{path::PathBuf, sync::Arc};

use uuid::Uuid;

use swage_core::GatewayError;
use swage_storage::{
    model::{NewTopic, TopicSlug},
    store::TopicStore,
};

pub use swage_protocol::client::topic::{
    CreateTopicRequest, TopicListResponse, TopicResponse, UpdateTopicRequest,
};

/// Derives a URL-safe slug from a topic name.
///
/// - Lowercases the name
/// - Replaces topics with hyphens
/// - Strips any character that is neither ASCII alphanumeric nor a hyphen
pub fn slugify(name: &str) -> String {
    name.to_lowercase()
        .replace(' ', "-")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect()
}

fn validate_topic_slug(slug: &str) -> Result<TopicSlug, GatewayError> {
    TopicSlug::new(slug).map_err(|_| {
        GatewayError::ValidationError("name must contain at least one ASCII letter or digit".into())
    })
}

/// Returns the base directory for all topic data under the canonical Swage root.
/// Mediates between HTTP topic handlers and storage.
#[derive(Clone)]
pub struct TopicService {
    pub(crate) topic_store: Arc<dyn TopicStore>,
    config_root: PathBuf,
    config: swage_config::ResolvedConfig,
}

impl TopicService {
    pub fn new(topic_store: Arc<dyn TopicStore>, config_root: PathBuf) -> Self {
        Self {
            topic_store,
            config_root,
            config: swage_config::ResolvedConfig::default(),
        }
    }

    pub fn with_config(mut self, config: swage_config::ResolvedConfig) -> Self {
        self.config = config;
        self
    }

    fn topic_to_response(
        &self,
        topic: swage_storage::model::Topic,
    ) -> Result<TopicResponse, GatewayError> {
        let config = crate::turn::load_session_config_from_root(
            &self.config_root,
            self.config.clone(),
            &topic.location(),
            "gateway",
        );
        let default_model = match config {
            Ok(config) => (!config.models.default.is_empty()).then_some(config.models.default),
            Err(error) => {
                tracing::warn!(topic_id = %topic.id, %error, "topic default model is unavailable");
                None
            }
        };
        Ok(TopicResponse {
            id: topic.id,
            name: topic.name,
            description: topic.description,
            slug: topic.slug.to_string(),
            tags: topic.tags,
            effective_default_model: default_model,
            created_at: topic.created_at,
        })
    }

    /// Create a new topic.
    ///
    /// Validates the name is non-empty, generates a slug, checks name
    /// uniqueness, creates the topic directory structure, and stores the topic.
    pub async fn create_topic(
        &self,
        req: CreateTopicRequest,
    ) -> Result<TopicResponse, GatewayError> {
        // Validate non-empty name.
        if req.name.trim().is_empty() {
            return Err(GatewayError::ValidationError(
                "name must not be empty".into(),
            ));
        }

        // Check name uniqueness.
        if self.topic_store.get_by_name(&req.name).await?.is_some() {
            return Err(GatewayError::TopicConflict(format!(
                "a topic named '{}' already exists",
                req.name
            )));
        }

        let slug = validate_topic_slug(&slugify(&req.name))?;

        // Create the topic directory under the canonical Swage root — the
        // scope's own location, never a path assembled by hand.
        let memory_dir = swage_core::ScopeLocation::topic(slug.as_str())
            .data_dir(&self.config_root)
            .join("memory");
        // Non-fatal: best-effort directory creation.
        if let Err(error) = std::fs::create_dir_all(&memory_dir) {
            tracing::warn!(
            %slug,
                path = %memory_dir.display(),
                %error,
                "failed to create topic memory directory"
            );
        }

        let topic = self
            .topic_store
            .create(NewTopic {
                name: req.name,
                slug,
            })
            .await?;
        if !req.description.trim().is_empty() {
            return self.topic_to_response(
                self.topic_store
                    .update(
                        topic.id,
                        None,
                        Some(req.description.trim().to_string()),
                        None,
                        None,
                    )
                    .await?,
            );
        }

        self.topic_to_response(topic)
    }

    /// List all topics.
    pub async fn list_topics(&self) -> Result<TopicListResponse, GatewayError> {
        let topics = self.topic_store.list_all().await?;
        Ok(TopicListResponse {
            topics: topics
                .into_iter()
                .map(|topic| self.topic_to_response(topic))
                .collect::<Result<_, _>>()?,
        })
    }

    /// Get a topic by ID.
    pub async fn get_topic(&self, id: Uuid) -> Result<TopicResponse, GatewayError> {
        self.topic_store
            .get(id)
            .await?
            .ok_or(GatewayError::TopicNotFound { id })
            .and_then(|topic| self.topic_to_response(topic))
    }

    /// Partially update a topic.
    ///
    /// Regenerates slug when name changes.
    pub async fn update_topic(
        &self,
        id: Uuid,
        req: UpdateTopicRequest,
    ) -> Result<TopicResponse, GatewayError> {
        // Validate name if provided.
        if let Some(ref name) = req.name
            && name.trim().is_empty()
        {
            return Err(GatewayError::ValidationError(
                "name must not be empty".into(),
            ));
        }

        // Derive the new slug from the new name, if name is being updated.
        let new_slug = match req.name.as_deref() {
            Some(name) => {
                let slug = slugify(name);
                Some(validate_topic_slug(&slug)?)
            }
            None => None,
        };

        // The one tag rule (`swage_core::tags`): trimmed, lower-cased, kept
        // once — the same form the library saves and the catalogue filters by.
        let tags = req.tags.map(swage_core::tags::canonical_tags);
        let topic = self
            .topic_store
            .update(id, req.name, req.description, new_slug, tags)
            .await
            .map_err(|e| match e {
                swage_core::StorageError::NotFound { id: ref eid, .. } => {
                    let parsed_id = eid.parse::<Uuid>().unwrap_or(id);
                    GatewayError::TopicNotFound { id: parsed_id }
                }
                other => GatewayError::Storage(other),
            })?;

        self.topic_to_response(topic)
    }

    /// Delete a topic.
    ///
    /// Returns 404 if not found. Returns a conflict error if called on the
    /// general scope. The topic directory on disk is preserved.
    pub async fn delete_topic(&self, id: Uuid) -> Result<(), GatewayError> {
        let found = self.topic_store.delete(id).await.map_err(|e| match e {
            swage_core::StorageError::ConstraintViolation(msg) => GatewayError::TopicConflict(msg),
            other => GatewayError::Storage(other),
        })?;

        if !found {
            return Err(GatewayError::TopicNotFound { id });
        }

        Ok(())
    }
}

#[cfg(test)]
mod topic_slug_tests {
    use super::{GatewayError, slugify, validate_topic_slug};

    #[test]
    fn punctuation_only_name_is_rejected_after_slugify() {
        let err = validate_topic_slug(&slugify("!!! /// ___"))
            .expect_err("punctuation-only names should be rejected");
        assert!(matches!(err, GatewayError::ValidationError(_)));
        assert!(
            err.to_string()
                .contains("at least one ASCII letter or digit")
        );
    }
}
