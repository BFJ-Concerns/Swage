use std::{path::PathBuf, sync::Arc};

use uuid::Uuid;

use swage_core::GatewayError;
use swage_storage::{
    model::{NewSpace, SpaceSlug},
    store::SpaceStore,
};

pub use swage_protocol::client::space::{
    CreateSpaceRequest, SpaceListResponse, SpaceResponse, UpdateSpaceRequest,
};

/// Derives a URL-safe slug from a space name.
///
/// - Lowercases the name
/// - Replaces spaces with hyphens
/// - Strips any character that is neither ASCII alphanumeric nor a hyphen
pub fn slugify(name: &str) -> String {
    name.to_lowercase()
        .replace(' ', "-")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect()
}

fn validate_space_slug(slug: &str) -> Result<SpaceSlug, GatewayError> {
    SpaceSlug::new(slug).map_err(|_| {
        GatewayError::ValidationError("name must contain at least one ASCII letter or digit".into())
    })
}

/// Returns the base directory for all space data under the canonical Swage root.
/// Mediates between HTTP space handlers and storage.
#[derive(Clone)]
pub struct SpaceService {
    pub(crate) space_store: Arc<dyn SpaceStore>,
    config_root: PathBuf,
}

impl SpaceService {
    pub fn new(space_store: Arc<dyn SpaceStore>, config_root: PathBuf) -> Self {
        Self {
            space_store,
            config_root,
        }
    }

    /// Create a new space.
    ///
    /// Validates the name is non-empty, generates a slug, checks name
    /// uniqueness, creates the space directory structure, and stores the space.
    pub async fn create_space(
        &self,
        req: CreateSpaceRequest,
    ) -> Result<SpaceResponse, GatewayError> {
        // Validate non-empty name.
        if req.name.trim().is_empty() {
            return Err(GatewayError::ValidationError(
                "name must not be empty".into(),
            ));
        }

        // Check name uniqueness.
        if self.space_store.get_by_name(&req.name).await?.is_some() {
            return Err(GatewayError::SpaceConflict(format!(
                "a space named '{}' already exists",
                req.name
            )));
        }

        let slug = validate_space_slug(&slugify(&req.name))?;

        // Create the space directory under the canonical Swage root.
        let base = self.config_root.join("spaces");
        let space_dir = base.join(&slug);
        let memory_dir = space_dir.join("memory");
        // Non-fatal: best-effort directory creation.
        if let Err(error) = std::fs::create_dir_all(&memory_dir) {
            tracing::warn!(
            %slug,
                path = %memory_dir.display(),
                %error,
                "failed to create space memory directory"
            );
        }

        let space = self
            .space_store
            .create(NewSpace {
                name: req.name,
                slug,
            })
            .await?;
        if !req.description.trim().is_empty() {
            return Ok(space_to_response(
                self.space_store
                    .update(
                        space.id,
                        None,
                        Some(req.description.trim().to_string()),
                        None,
                    )
                    .await?,
            ));
        }

        Ok(space_to_response(space))
    }

    /// List all spaces.
    pub async fn list_spaces(&self) -> Result<SpaceListResponse, GatewayError> {
        let spaces = self.space_store.list_all().await?;
        Ok(SpaceListResponse {
            spaces: spaces.into_iter().map(space_to_response).collect(),
        })
    }

    /// Get a space by ID.
    pub async fn get_space(&self, id: Uuid) -> Result<SpaceResponse, GatewayError> {
        self.space_store
            .get(id)
            .await?
            .map(space_to_response)
            .ok_or(GatewayError::SpaceNotFound { id })
    }

    /// Partially update a space.
    ///
    /// Regenerates slug when name changes.
    pub async fn update_space(
        &self,
        id: Uuid,
        req: UpdateSpaceRequest,
    ) -> Result<SpaceResponse, GatewayError> {
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
                Some(validate_space_slug(&slug)?)
            }
            None => None,
        };

        let space = self
            .space_store
            .update(id, req.name, req.description, new_slug)
            .await
            .map_err(|e| match e {
                swage_core::StorageError::NotFound { id: ref eid, .. } => {
                    let parsed_id = eid.parse::<Uuid>().unwrap_or(id);
                    GatewayError::SpaceNotFound { id: parsed_id }
                }
                other => GatewayError::Storage(other),
            })?;

        Ok(space_to_response(space))
    }

    /// Delete a space.
    ///
    /// Returns 404 if not found. Returns a conflict error if called on the
    /// default space. The space directory on disk is preserved.
    pub async fn delete_space(&self, id: Uuid) -> Result<(), GatewayError> {
        let found = self.space_store.delete(id).await.map_err(|e| match e {
            swage_core::StorageError::ConstraintViolation(msg) => GatewayError::SpaceConflict(msg),
            other => GatewayError::Storage(other),
        })?;

        if !found {
            return Err(GatewayError::SpaceNotFound { id });
        }

        Ok(())
    }
}

fn space_to_response(p: swage_storage::model::Space) -> SpaceResponse {
    SpaceResponse {
        id: p.id,
        name: p.name,
        description: p.description,
        is_default: p.is_default,
        slug: p.slug.to_string(),
        created_at: p.created_at,
    }
}

#[cfg(test)]
mod space_slug_tests {
    use super::{GatewayError, slugify, validate_space_slug};

    #[test]
    fn punctuation_only_name_is_rejected_after_slugify() {
        let err = validate_space_slug(&slugify("!!! /// ___"))
            .expect_err("punctuation-only names should be rejected");
        assert!(matches!(err, GatewayError::ValidationError(_)));
        assert!(
            err.to_string()
                .contains("at least one ASCII letter or digit")
        );
    }
}
