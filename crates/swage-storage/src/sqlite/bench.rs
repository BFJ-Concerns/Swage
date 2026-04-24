use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

use swage_core::StorageError;

use crate::model::{Bench, NewBench};
use crate::sqlite::SqliteStorage;
use crate::store::BenchStore;

// ---------------------------------------------------------------------------
// Row mapping
// ---------------------------------------------------------------------------

fn row_to_bench(row: &rusqlite::Row<'_>) -> rusqlite::Result<Bench> {
    let id: String = row.get(0)?;
    // Column order:
    // id(0), name(1), is_default(2), slug(3), created_at(4), updated_at(5)
    let created: String = row.get(4)?;
    let updated: String = row.get(5)?;

    Ok(Bench {
        id: parse_uuid(&id)?,
        name: row.get(1)?,
        is_default: row.get::<_, i64>(2)? != 0,
        slug: row.get(3)?,
        created_at: parse_dt(&created)?,
        updated_at: parse_dt(&updated)?,
    })
}

fn parse_uuid(s: &str) -> rusqlite::Result<Uuid> {
    s.parse::<Uuid>().map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

fn parse_dt(s: &str) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })
}

// ---------------------------------------------------------------------------
// BenchStore
// ---------------------------------------------------------------------------

#[async_trait]
impl BenchStore for SqliteStorage {
    async fn create(&self, bench: NewBench) -> Result<Bench, StorageError> {
        self.with_conn(|conn| {
            let id = Uuid::new_v4();
            let now = Utc::now();
            let now_str = now.to_rfc3339();

            conn.execute(
                "INSERT INTO benches (id, name, is_default, slug, created_at, updated_at)
                 VALUES (?1, ?2, 0, ?3, ?4, ?4)",
                params![id.to_string(), &bench.name, &bench.slug, &now_str],
            )?;

            Ok(Bench {
                id,
                name: bench.name,
                is_default: false,
                slug: bench.slug,
                created_at: now,
                updated_at: now,
            })
        })
        .await
    }

    async fn get(&self, id: Uuid) -> Result<Option<Bench>, StorageError> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT id, name, is_default, slug, created_at, updated_at
                 FROM benches WHERE id = ?1",
                params![id.to_string()],
                row_to_bench,
            )
            .optional()
        })
        .await
    }

    async fn get_by_name(&self, name: &str) -> Result<Option<Bench>, StorageError> {
        // Capture as owned String to satisfy 'static bound on with_conn closure.
        let name = name.to_owned();
        self.with_conn(move |conn| {
            conn.query_row(
                "SELECT id, name, is_default, slug, created_at, updated_at
                 FROM benches WHERE name = ?1",
                params![&name],
                row_to_bench,
            )
            .optional()
        })
        .await
    }

    async fn get_or_create_default(&self) -> Result<Bench, StorageError> {
        if let Some(bench) = self
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT id, name, is_default, slug, created_at, updated_at
                     FROM benches
                     WHERE is_default = 1",
                    [],
                    row_to_bench,
                )
                .optional()
            })
            .await?
        {
            return Ok(bench);
        }

        self.with_conn(|conn| {
            let id = Uuid::new_v4();
            let now = Utc::now();
            let now_str = now.to_rfc3339();

            conn.execute(
                "INSERT INTO benches (id, name, is_default, slug, created_at, updated_at)
                 VALUES (?1, 'default', 1, 'default', ?2, ?2)",
                params![id.to_string(), &now_str],
            )?;

            Ok(Bench {
                id,
                name: "default".into(),
                is_default: true,
                slug: "default".into(),
                created_at: now,
                updated_at: now,
            })
        })
        .await
    }

    async fn list_all(&self) -> Result<Vec<Bench>, StorageError> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, name, is_default, slug, created_at, updated_at
                 FROM benches ORDER BY created_at ASC",
            )?;
            let rows = stmt.query_map([], row_to_bench)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
        .await
    }

    async fn get_by_slug(&self, slug: &str) -> Result<Option<Bench>, StorageError> {
        let slug = slug.to_owned();
        self.with_conn(move |conn| {
            conn.query_row(
                "SELECT id, name, is_default, slug, created_at, updated_at
                 FROM benches WHERE slug = ?1",
                params![&slug],
                row_to_bench,
            )
            .optional()
        })
        .await
    }

    async fn update(
        &self,
        id: Uuid,
        name: Option<String>,
        slug: Option<String>,
    ) -> Result<Bench, StorageError> {
        // Pre-flight: verify the bench exists.
        if self.get(id).await?.is_none() {
            return Err(StorageError::NotFound {
                entity: "Bench".into(),
                id: id.to_string(),
            });
        }

        self.with_conn(move |conn| {
            let now = Utc::now().to_rfc3339();
            let id_str = id.to_string();

            // COALESCE preserves the existing value when the parameter is NULL,
            // so None fields stay unchanged while Some fields are overwritten.
            conn.execute(
                "UPDATE benches
                 SET name           = COALESCE(?2, name),
                     slug           = COALESCE(?3, slug),
                     updated_at     = ?4
                 WHERE id = ?1",
                params![id_str, name, slug, now],
            )?;

            conn.query_row(
                "SELECT id, name, is_default, slug, created_at, updated_at
                 FROM benches WHERE id = ?1",
                params![id_str],
                row_to_bench,
            )
        })
        .await
    }

    async fn delete(&self, id: Uuid) -> Result<bool, StorageError> {
        // Pre-flight: fetch the bench to check existence and guard the default.
        match self.get(id).await? {
            None => return Ok(false),
            Some(ref p) if p.is_default => {
                return Err(StorageError::ConstraintViolation(
                    "cannot delete the default bench".into(),
                ));
            }
            Some(_) => {}
        }

        // Sessions cascade via the FK ON DELETE CASCADE defined in the schema.
        self.with_conn(move |conn| {
            let affected =
                conn.execute("DELETE FROM benches WHERE id = ?1", params![id.to_string()])?;
            Ok(affected > 0)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlite::SqliteStorage;

    #[tokio::test(flavor = "multi_thread")]
    async fn create_and_get_bench() {
        let storage = SqliteStorage::in_memory().unwrap();
        let new = NewBench {
            name: "my-bench".into(),
            slug: "my-bench".into(),
        };
        let created = storage.create(new).await.unwrap();

        assert_eq!(created.name, "my-bench");
        assert_eq!(created.slug, "my-bench");

        let fetched = storage
            .get(created.id)
            .await
            .unwrap()
            .expect("should exist");
        assert_eq!(fetched.id, created.id);
        assert_eq!(fetched.name, "my-bench");
        assert_eq!(fetched.slug, "my-bench");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_by_name_returns_none_for_unknown() {
        let storage = SqliteStorage::in_memory().unwrap();
        let result = storage.get_by_name("no-such-bench").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_by_name_finds_existing() {
        let storage = SqliteStorage::in_memory().unwrap();
        storage
            .create(NewBench {
                name: "find-me".into(),
                slug: "find-me".into(),
            })
            .await
            .unwrap();

        let found = storage.get_by_name("find-me").await.unwrap();
        assert!(found.is_some());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_or_create_default_is_idempotent() {
        let storage = SqliteStorage::in_memory().unwrap();
        let p1 = storage.get_or_create_default().await.unwrap();
        let p2 = storage.get_or_create_default().await.unwrap();
        // Same bench returned both times — not duplicated.
        assert_eq!(p1.id, p2.id);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn slug_is_stored_and_retrieved() {
        let storage = SqliteStorage::in_memory().unwrap();
        let new = NewBench {
            name: "slugged-bench".into(),
            slug: "slugged-bench".into(),
        };
        let created = storage.create(new).await.unwrap();
        assert_eq!(created.slug, "slugged-bench");

        let fetched = storage
            .get(created.id)
            .await
            .unwrap()
            .expect("should exist");
        assert_eq!(fetched.slug, "slugged-bench");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_or_create_default_returns_default_slug() {
        let storage = SqliteStorage::in_memory().unwrap();
        let bench = storage.get_or_create_default().await.unwrap();
        assert!(bench.is_default);
        assert_eq!(bench.slug, "default");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn renamed_default_bench_remains_structurally_default() {
        let storage = SqliteStorage::in_memory().unwrap();
        let default_bench = storage.get_or_create_default().await.unwrap();

        let renamed = storage
            .update(
                default_bench.id,
                Some("Operations".into()),
                Some("operations".into()),
            )
            .await
            .unwrap();

        assert!(renamed.is_default);
        assert_eq!(renamed.name, "Operations");

        let fetched_default = storage.get_or_create_default().await.unwrap();
        assert_eq!(fetched_default.id, default_bench.id);

        let delete_error = storage.delete(default_bench.id).await.unwrap_err();
        assert!(
            matches!(delete_error, StorageError::ConstraintViolation(_)),
            "renamed default bench should still be protected"
        );
    }

    // ---------------------------------------------------------------------------
    // list_all
    // ---------------------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn list_all_returns_benches_ordered_by_created_at() {
        let storage = SqliteStorage::in_memory().unwrap();
        // The seeded default bench already exists; add two more.
        storage
            .create(NewBench {
                name: "alpha".into(),
                slug: "alpha".into(),
            })
            .await
            .unwrap();
        storage
            .create(NewBench {
                name: "beta".into(),
                slug: "beta".into(),
            })
            .await
            .unwrap();

        let all = storage.list_all().await.unwrap();

        // At least 3 benches (default + alpha + beta).
        assert!(
            all.len() >= 3,
            "expected at least 3 benches, got {}",
            all.len()
        );

        // Must be ordered by created_at ASC.
        let is_sorted = all.windows(2).all(|w| w[0].created_at <= w[1].created_at);
        assert!(is_sorted, "benches should be ordered by created_at ASC");
    }

    // ---------------------------------------------------------------------------
    // get_by_slug
    // ---------------------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn get_by_slug_returns_matching_bench() {
        let storage = SqliteStorage::in_memory().unwrap();
        storage
            .create(NewBench {
                name: "Slug Test".into(),
                slug: "slug-test".into(),
            })
            .await
            .unwrap();

        let found = storage.get_by_slug("slug-test").await.unwrap();
        assert!(found.is_some(), "get_by_slug should find the bench");
        assert_eq!(found.unwrap().slug, "slug-test");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_by_slug_returns_none_for_missing() {
        let storage = SqliteStorage::in_memory().unwrap();
        let result = storage.get_by_slug("no-such-slug").await.unwrap();
        assert!(result.is_none());
    }

    // ---------------------------------------------------------------------------
    // update
    // ---------------------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn update_changes_provided_fields() {
        let storage = SqliteStorage::in_memory().unwrap();
        let created = storage
            .create(NewBench {
                name: "Old Name".into(),
                slug: "old-name".into(),
            })
            .await
            .unwrap();

        let updated = storage
            .update(created.id, Some("New Name".into()), Some("new-name".into()))
            .await
            .unwrap();

        assert_eq!(updated.name, "New Name");
        assert_eq!(updated.slug, "new-name");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn update_partial_leaves_unspecified_fields_unchanged() {
        let storage = SqliteStorage::in_memory().unwrap();
        let created = storage
            .create(NewBench {
                name: "Stable".into(),
                slug: "stable".into(),
            })
            .await
            .unwrap();

        let updated = storage.update(created.id, None, None).await.unwrap();

        assert_eq!(updated.name, "Stable");
        assert_eq!(updated.slug, "stable");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn update_errors_for_unknown_id() {
        let storage = SqliteStorage::in_memory().unwrap();
        let nonexistent = Uuid::new_v4();
        let result = storage.update(nonexistent, Some("X".into()), None).await;
        assert!(
            result.is_err(),
            "update on unknown id should return an error"
        );
    }

    // ---------------------------------------------------------------------------
    // delete
    // ---------------------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn delete_removes_bench_and_returns_true() {
        let storage = SqliteStorage::in_memory().unwrap();
        let created = storage
            .create(NewBench {
                name: "Gone".into(),
                slug: "gone".into(),
            })
            .await
            .unwrap();

        let deleted = storage.delete(created.id).await.unwrap();
        assert!(deleted, "delete should return true for an existing bench");

        let fetched = storage.get(created.id).await.unwrap();
        assert!(
            fetched.is_none(),
            "bench should no longer exist after delete"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn delete_returns_false_for_unknown_id() {
        let storage = SqliteStorage::in_memory().unwrap();
        let nonexistent = Uuid::new_v4();
        let result = storage.delete(nonexistent).await.unwrap();
        assert!(!result, "delete on unknown id should return false");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn delete_refuses_default_bench() {
        let storage = SqliteStorage::in_memory().unwrap();
        let default_bench = storage.get_or_create_default().await.unwrap();

        let result = storage.delete(default_bench.id).await;
        assert!(
            result.is_err(),
            "deleting the default bench should return an error"
        );
    }
}
