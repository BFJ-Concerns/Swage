//! Knowledge-base body files — content-addressed storage for observation
//! raw bodies.
//!
//! The knowledge base's SQLite rows hold metadata; the captured bytes live
//! here, addressed by their SHA-256 under the space data root:
//! `spaces/<slug>/.swage/knowledge/bodies/<hash[0..2]>/<hash>`. Files are
//! write-once and never deleted: raw bodies are retained indefinitely so a
//! better extractor (or eventual OCR) can re-process old captures — lossy
//! text never becomes the only copy.

use std::path::PathBuf;

use sha2::{Digest, Sha256};
use swage_config::discovery::resolve_root;
use swage_core::GatewayError;

/// A stored body's address and size, as the observation row records them.
#[derive(Debug, Clone)]
pub struct StoredBody {
    /// Lowercase hex SHA-256 of the bytes — the file's address.
    pub sha256: String,
    pub byte_count: u64,
}

fn bodies_root(space_slug: &str) -> Result<PathBuf, GatewayError> {
    Ok(resolve_root()
        .map_err(|problem| GatewayError::ValidationError(problem.to_string()))?
        .join("spaces")
        .join(space_slug)
        .join(".swage")
        .join("knowledge")
        .join("bodies"))
}

fn body_path(space_slug: &str, sha256: &str) -> Result<PathBuf, GatewayError> {
    // Addresses come from our own hasher or a validated observation row, but
    // the path is built defensively anyway: exactly 64 lowercase hex chars.
    if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(GatewayError::ValidationError(format!(
            "not a body address: {sha256:?}"
        )));
    }
    let sha256 = sha256.to_ascii_lowercase();
    Ok(bodies_root(space_slug)?.join(&sha256[..2]).join(sha256))
}

/// Store raw bytes, returning their address. Idempotent: identical bytes
/// hash to the same path, and an existing file is left untouched.
pub async fn write_body(space_slug: &str, bytes: &[u8]) -> Result<StoredBody, GatewayError> {
    let sha256: String = Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let path = body_path(space_slug, &sha256)?;

    if tokio::fs::try_exists(&path)
        .await
        .map_err(|error| GatewayError::ValidationError(error.to_string()))?
    {
        return Ok(StoredBody {
            sha256,
            byte_count: bytes.len() as u64,
        });
    }

    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| GatewayError::ValidationError(error.to_string()))?;
    }

    // Write via a uniquely named temp file and rename: a crash mid-write can
    // never leave a half body at a valid address, and concurrent writers of
    // the same content (parallel gather workers are the normal case) each
    // rename their own file — last rename wins with identical bytes.
    let tmp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    tokio::fs::write(&tmp, bytes)
        .await
        .map_err(|error| GatewayError::ValidationError(error.to_string()))?;
    tokio::fs::rename(&tmp, &path)
        .await
        .map_err(|error| GatewayError::ValidationError(error.to_string()))?;

    Ok(StoredBody {
        sha256,
        byte_count: bytes.len() as u64,
    })
}

/// Read a stored body back by address.
pub async fn read_body(space_slug: &str, sha256: &str) -> Result<Vec<u8>, GatewayError> {
    let path = body_path(space_slug, sha256)?;
    tokio::fs::read(&path).await.map_err(|error| {
        GatewayError::ValidationError(format!("body {sha256} is not readable: {error}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_support::SwageHomeGuard;

    // resolve_root reads the config hierarchy, so these tests pin it to a
    // temp directory. SWAGE_HOME is process-wide state and cargo runs tests
    // on parallel threads, so the pin must be the crate's serialising guard —
    // never a bare set_var.
    fn pin_root() -> SwageHomeGuard {
        SwageHomeGuard::new()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn write_is_idempotent_and_read_round_trips() {
        let _root = pin_root();

        let first = write_body("research", b"captured page").await.unwrap();
        let second = write_body("research", b"captured page").await.unwrap();
        assert_eq!(first.sha256, second.sha256);

        let bytes = read_body("research", &first.sha256).await.unwrap();
        assert_eq!(bytes, b"captured page");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn invalid_address_is_rejected_not_resolved() {
        let _root = pin_root();

        let result = read_body("research", "../../../etc/passwd").await;
        assert!(matches!(result, Err(GatewayError::ValidationError(_))));
    }
}
