use std::{path::PathBuf, sync::Arc};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use swage_core::GatewayError;
use swage_storage::{
    model::{BenchSlug, NewBench},
    store::BenchStore,
};

// ---------------------------------------------------------------------------
/// Request body for creating a bench.
#[derive(Debug, Deserialize)]
pub struct CreateBenchRequest {
    pub name: String,
    #[serde(default)]
    pub description: String,
}

/// Request body for partially updating a bench.
#[derive(Debug, Deserialize)]
pub struct UpdateBenchRequest {
    pub name: Option<String>,
    pub description: Option<String>,
}

/// API response for a single bench.
#[derive(Debug, Serialize)]
pub struct BenchResponse {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub is_default: bool,
    pub slug: String,
    pub created_at: DateTime<Utc>,
}

/// API response for the list-benches endpoint.
#[derive(Debug, Serialize)]
pub struct BenchListResponse {
    pub benches: Vec<BenchResponse>,
}

/// Derives a URL-safe slug from a bench name.
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

fn validate_bench_slug(slug: &str) -> Result<BenchSlug, GatewayError> {
    BenchSlug::new(slug).map_err(|_| {
        GatewayError::ValidationError("name must contain at least one ASCII letter or digit".into())
    })
}

/// Returns the base directory for all bench data under the canonical Swage root.
fn benches_base_dir() -> Result<PathBuf, GatewayError> {
    swage_config::discovery::resolve_root()
        .map(|root| root.join("benches"))
        .map_err(|problem| GatewayError::ValidationError(problem.to_string()))
}

/// Mediates between HTTP bench handlers and storage.
#[derive(Clone)]
pub struct BenchService {
    pub(crate) bench_store: Arc<dyn BenchStore>,
}

impl BenchService {
    pub fn new(bench_store: Arc<dyn BenchStore>) -> Self {
        Self { bench_store }
    }

    /// Create a new bench.
    ///
    /// Validates the name is non-empty, generates a slug, checks name
    /// uniqueness, creates the bench directory structure, and stores the bench.
    pub async fn create_bench(
        &self,
        req: CreateBenchRequest,
    ) -> Result<BenchResponse, GatewayError> {
        // Validate non-empty name.
        if req.name.trim().is_empty() {
            return Err(GatewayError::ValidationError(
                "name must not be empty".into(),
            ));
        }

        // Check name uniqueness.
        if self.bench_store.get_by_name(&req.name).await?.is_some() {
            return Err(GatewayError::BenchConflict(format!(
                "a bench named '{}' already exists",
                req.name
            )));
        }

        let slug = validate_bench_slug(&slugify(&req.name))?;

        // Create the bench directory under the canonical Swage root.
        let base = benches_base_dir()?;
        let bench_dir = base.join(&slug);
        let memory_dir = bench_dir.join("memory");
        // Non-fatal: best-effort directory creation.
        if let Err(error) = std::fs::create_dir_all(&memory_dir) {
            tracing::warn!(
            %slug,
                path = %memory_dir.display(),
                %error,
                "failed to create bench memory directory"
            );
        }

        let bench = self
            .bench_store
            .create(NewBench {
                name: req.name,
                slug,
            })
            .await?;
        if !req.description.trim().is_empty() {
            return Ok(bench_to_response(
                self.bench_store
                    .update(
                        bench.id,
                        None,
                        Some(req.description.trim().to_string()),
                        None,
                    )
                    .await?,
            ));
        }

        Ok(bench_to_response(bench))
    }

    /// List all benches.
    pub async fn list_benches(&self) -> Result<BenchListResponse, GatewayError> {
        let benches = self.bench_store.list_all().await?;
        Ok(BenchListResponse {
            benches: benches.into_iter().map(bench_to_response).collect(),
        })
    }

    /// Get a bench by ID.
    pub async fn get_bench(&self, id: Uuid) -> Result<BenchResponse, GatewayError> {
        self.bench_store
            .get(id)
            .await?
            .map(bench_to_response)
            .ok_or(GatewayError::BenchNotFound { id })
    }

    /// Partially update a bench.
    ///
    /// Regenerates slug when name changes.
    pub async fn update_bench(
        &self,
        id: Uuid,
        req: UpdateBenchRequest,
    ) -> Result<BenchResponse, GatewayError> {
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
                Some(validate_bench_slug(&slug)?)
            }
            None => None,
        };

        let bench = self
            .bench_store
            .update(id, req.name, req.description, new_slug)
            .await
            .map_err(|e| match e {
                swage_core::StorageError::NotFound { id: ref eid, .. } => {
                    let parsed_id = eid.parse::<Uuid>().unwrap_or(id);
                    GatewayError::BenchNotFound { id: parsed_id }
                }
                other => GatewayError::Storage(other),
            })?;

        Ok(bench_to_response(bench))
    }

    /// Delete a bench.
    ///
    /// Returns 404 if not found. Returns a conflict error if called on the
    /// default bench. The bench directory on disk is preserved.
    pub async fn delete_bench(&self, id: Uuid) -> Result<(), GatewayError> {
        let found = self.bench_store.delete(id).await.map_err(|e| match e {
            swage_core::StorageError::ConstraintViolation(msg) => GatewayError::BenchConflict(msg),
            other => GatewayError::Storage(other),
        })?;

        if !found {
            return Err(GatewayError::BenchNotFound { id });
        }

        Ok(())
    }
}

fn bench_to_response(p: swage_storage::model::Bench) -> BenchResponse {
    BenchResponse {
        id: p.id,
        name: p.name,
        description: p.description,
        is_default: p.is_default,
        slug: p.slug.to_string(),
        created_at: p.created_at,
    }
}

#[cfg(test)]
mod bench_slug_tests {
    use super::{GatewayError, slugify, validate_bench_slug};

    #[test]
    fn punctuation_only_name_is_rejected_after_slugify() {
        let err = validate_bench_slug(&slugify("!!! /// ___"))
            .expect_err("punctuation-only names should be rejected");
        assert!(matches!(err, GatewayError::ValidationError(_)));
        assert!(
            err.to_string()
                .contains("at least one ASCII letter or digit")
        );
    }
}
