use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

use swage_core::StorageError;

use crate::model::{NewTopic, Topic, TopicSlug};
use crate::sqlite::SqliteStorage;
use crate::store::TopicStore;

// ---------------------------------------------------------------------------
// Row mapping
// ---------------------------------------------------------------------------

fn row_to_topic(row: &rusqlite::Row<'_>) -> rusqlite::Result<Topic> {
    let id: String = row.get(0)?;
    // Column order:
    // id(0), name(1), description(2), is_default(3), slug(4), created_at(5), updated_at(6)
    let created: String = row.get(5)?;
    let updated: String = row.get(6)?;
    let slug: String = row.get(4)?;

    Ok(Topic {
        id: parse_uuid(&id)?,
        name: row.get(1)?,
        description: row.get(2)?,
        is_default: row.get::<_, i64>(3)? != 0,
        slug: parse_slug(&slug)?,
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

fn parse_slug(s: &str) -> rusqlite::Result<TopicSlug> {
    TopicSlug::new(s).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, e.into())
    })
}

// ---------------------------------------------------------------------------
// TopicStore
// ---------------------------------------------------------------------------

#[async_trait]
impl TopicStore for SqliteStorage {
    async fn create(&self, topic: NewTopic) -> Result<Topic, StorageError> {
        self.with_conn(|conn| {
            let id = Uuid::new_v4();
            let now = Utc::now();
            let now_str = now.to_rfc3339();

            conn.execute(
                "INSERT INTO topics (id, name, description, is_default, slug, created_at, updated_at)
                 VALUES (?1, ?2, '', 0, ?3, ?4, ?4)",
                params![id.to_string(), &topic.name, topic.slug.as_str(), &now_str],
            )?;

            Ok(Topic {
                id,
                name: topic.name,
                description: String::new(),
                is_default: false,
                slug: topic.slug,
                created_at: now,
                updated_at: now,
            })
        })
        .await
    }

    async fn get(&self, id: Uuid) -> Result<Option<Topic>, StorageError> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT id, name, description, is_default, slug, created_at, updated_at
                 FROM topics WHERE id = ?1",
                params![id.to_string()],
                row_to_topic,
            )
            .optional()
        })
        .await
    }

    async fn get_by_name(&self, name: &str) -> Result<Option<Topic>, StorageError> {
        // Capture as owned String to satisfy 'static bound on with_conn closure.
        let name = name.to_owned();
        self.with_conn(move |conn| {
            conn.query_row(
                "SELECT id, name, description, is_default, slug, created_at, updated_at
                 FROM topics WHERE name = ?1",
                params![&name],
                row_to_topic,
            )
            .optional()
        })
        .await
    }

    async fn get_or_create_default(&self) -> Result<Topic, StorageError> {
        if let Some(topic) = self
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT id, name, description, is_default, slug, created_at, updated_at
                     FROM topics
                     WHERE is_default = 1",
                    [],
                    row_to_topic,
                )
                .optional()
            })
            .await?
        {
            return Ok(topic);
        }

        self.with_conn(|conn| {
            let id = Uuid::new_v4();
            let now = Utc::now();
            let now_str = now.to_rfc3339();

            conn.execute(
                "INSERT INTO topics (id, name, description, is_default, slug, created_at, updated_at)
                 VALUES (?1, 'default', '', 1, 'default', ?2, ?2)",
                params![id.to_string(), &now_str],
            )?;

            Ok(Topic {
                id,
                name: "default".into(),
                description: String::new(),
                is_default: true,
                slug: "default".into(),
                created_at: now,
                updated_at: now,
            })
        })
        .await
    }

    async fn list_all(&self) -> Result<Vec<Topic>, StorageError> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, name, description, is_default, slug, created_at, updated_at
                 FROM topics ORDER BY created_at ASC",
            )?;
            let rows = stmt.query_map([], row_to_topic)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
        .await
    }

    async fn get_by_slug(&self, slug: &str) -> Result<Option<Topic>, StorageError> {
        let slug = slug.to_owned();
        self.with_conn(move |conn| {
            conn.query_row(
                "SELECT id, name, description, is_default, slug, created_at, updated_at
                 FROM topics WHERE slug = ?1",
                params![&slug],
                row_to_topic,
            )
            .optional()
        })
        .await
    }

    async fn update(
        &self,
        id: Uuid,
        name: Option<String>,
        description: Option<String>,
        slug: Option<TopicSlug>,
    ) -> Result<Topic, StorageError> {
        // Pre-flight: verify the topic exists.
        if self.get(id).await?.is_none() {
            return Err(StorageError::NotFound {
                entity: "Topic".into(),
                id: id.to_string(),
            });
        }

        self.with_conn(move |conn| {
            let now = Utc::now().to_rfc3339();
            let id_str = id.to_string();

            // COALESCE preserves the existing value when the parameter is NULL,
            // so None fields stay unchanged while Some fields are overwritten.
            conn.execute(
                "UPDATE topics
                 SET name           = COALESCE(?2, name),
                     description    = COALESCE(?3, description),
                     slug           = COALESCE(?4, slug),
                     updated_at     = ?5
                 WHERE id = ?1",
                params![
                    id_str,
                    name,
                    description,
                    slug.as_ref().map(TopicSlug::as_str),
                    now
                ],
            )?;

            conn.query_row(
                "SELECT id, name, description, is_default, slug, created_at, updated_at
                 FROM topics WHERE id = ?1",
                params![id_str],
                row_to_topic,
            )
        })
        .await
    }

    async fn delete(&self, id: Uuid) -> Result<bool, StorageError> {
        // Pre-flight: fetch the topic to check existence and guard the default.
        match self.get(id).await? {
            None => return Ok(false),
            Some(ref p) if p.is_default => {
                return Err(StorageError::ConstraintViolation(
                    "cannot delete the default topic".into(),
                ));
            }
            Some(_) => {}
        }

        // Sessions cascade via the FK ON DELETE CASCADE defined in the schema.
        self.with_conn(move |conn| {
            let affected =
                conn.execute("DELETE FROM topics WHERE id = ?1", params![id.to_string()])?;
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
    async fn create_and_get_topic() {
        let storage = SqliteStorage::in_memory().unwrap();
        let new = NewTopic {
            name: "my-topic".into(),
            slug: "my-topic".into(),
        };
        let created = storage.create(new).await.unwrap();

        assert_eq!(created.name, "my-topic");
        assert_eq!(created.slug, "my-topic");

        let fetched = storage
            .get(created.id)
            .await
            .unwrap()
            .expect("should exist");
        assert_eq!(fetched.id, created.id);
        assert_eq!(fetched.name, "my-topic");
        assert_eq!(fetched.slug, "my-topic");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_by_name_returns_none_for_unknown() {
        let storage = SqliteStorage::in_memory().unwrap();
        let result = storage.get_by_name("no-such-topic").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_by_name_finds_existing() {
        let storage = SqliteStorage::in_memory().unwrap();
        storage
            .create(NewTopic {
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
        // Same topic returned both times — not duplicated.
        assert_eq!(p1.id, p2.id);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn slug_is_stored_and_retrieved() {
        let storage = SqliteStorage::in_memory().unwrap();
        let new = NewTopic {
            name: "slugged-topic".into(),
            slug: "slugged-topic".into(),
        };
        let created = storage.create(new).await.unwrap();
        assert_eq!(created.slug, "slugged-topic");

        let fetched = storage
            .get(created.id)
            .await
            .unwrap()
            .expect("should exist");
        assert_eq!(fetched.slug, "slugged-topic");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_or_create_default_returns_default_slug() {
        let storage = SqliteStorage::in_memory().unwrap();
        let topic = storage.get_or_create_default().await.unwrap();
        assert!(topic.is_default);
        assert_eq!(topic.slug, "default");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn renamed_default_topic_remains_structurally_default() {
        let storage = SqliteStorage::in_memory().unwrap();
        let default_topic = storage.get_or_create_default().await.unwrap();

        let renamed = storage
            .update(
                default_topic.id,
                Some("Operations".into()),
                None,
                Some("operations".into()),
            )
            .await
            .unwrap();

        assert!(renamed.is_default);
        assert_eq!(renamed.name, "Operations");

        let fetched_default = storage.get_or_create_default().await.unwrap();
        assert_eq!(fetched_default.id, default_topic.id);

        let delete_error = storage.delete(default_topic.id).await.unwrap_err();
        assert!(
            matches!(delete_error, StorageError::ConstraintViolation(_)),
            "renamed default topic should still be protected"
        );
    }

    // ---------------------------------------------------------------------------
    // list_all
    // ---------------------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn list_all_returns_spaces_ordered_by_created_at() {
        let storage = SqliteStorage::in_memory().unwrap();
        // The seeded default topic already exists; add two more.
        storage
            .create(NewTopic {
                name: "alpha".into(),
                slug: "alpha".into(),
            })
            .await
            .unwrap();
        storage
            .create(NewTopic {
                name: "beta".into(),
                slug: "beta".into(),
            })
            .await
            .unwrap();

        let all = storage.list_all().await.unwrap();

        // At least 3 topics (default + alpha + beta).
        assert!(
            all.len() >= 3,
            "expected at least 3 topics, got {}",
            all.len()
        );

        // Must be ordered by created_at ASC.
        let is_sorted = all.windows(2).all(|w| w[0].created_at <= w[1].created_at);
        assert!(is_sorted, "topics should be ordered by created_at ASC");
    }

    // ---------------------------------------------------------------------------
    // get_by_slug
    // ---------------------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn get_by_slug_returns_matching_topic() {
        let storage = SqliteStorage::in_memory().unwrap();
        storage
            .create(NewTopic {
                name: "Slug Test".into(),
                slug: "slug-test".into(),
            })
            .await
            .unwrap();

        let found = storage.get_by_slug("slug-test").await.unwrap();
        assert!(found.is_some(), "get_by_slug should find the topic");
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
            .create(NewTopic {
                name: "Old Name".into(),
                slug: "old-name".into(),
            })
            .await
            .unwrap();

        let updated = storage
            .update(
                created.id,
                Some("New Name".into()),
                None,
                Some("new-name".into()),
            )
            .await
            .unwrap();

        assert_eq!(updated.name, "New Name");
        assert_eq!(updated.slug, "new-name");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn update_changes_description() {
        let storage = SqliteStorage::in_memory().unwrap();
        let created = storage
            .create(NewTopic {
                name: "Documented".into(),
                slug: "documented".into(),
            })
            .await
            .unwrap();

        let updated = storage
            .update(created.id, None, Some("Operations topic".into()), None)
            .await
            .unwrap();

        assert_eq!(updated.description, "Operations topic");
        let fetched = storage.get(created.id).await.unwrap().expect("topic");
        assert_eq!(fetched.description, "Operations topic");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn update_partial_leaves_unspecified_fields_unchanged() {
        let storage = SqliteStorage::in_memory().unwrap();
        let created = storage
            .create(NewTopic {
                name: "Stable".into(),
                slug: "stable".into(),
            })
            .await
            .unwrap();

        let updated = storage.update(created.id, None, None, None).await.unwrap();

        assert_eq!(updated.name, "Stable");
        assert_eq!(updated.slug, "stable");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn update_errors_for_unknown_id() {
        let storage = SqliteStorage::in_memory().unwrap();
        let nonexistent = Uuid::new_v4();
        let result = storage
            .update(nonexistent, Some("X".into()), None, None)
            .await;
        assert!(
            result.is_err(),
            "update on unknown id should return an error"
        );
    }

    // ---------------------------------------------------------------------------
    // delete
    // ---------------------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn delete_removes_topic_and_returns_true() {
        let storage = SqliteStorage::in_memory().unwrap();
        let created = storage
            .create(NewTopic {
                name: "Gone".into(),
                slug: "gone".into(),
            })
            .await
            .unwrap();

        let deleted = storage.delete(created.id).await.unwrap();
        assert!(deleted, "delete should return true for an existing topic");

        let fetched = storage.get(created.id).await.unwrap();
        assert!(
            fetched.is_none(),
            "topic should no longer exist after delete"
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
    async fn delete_refuses_default_topic() {
        let storage = SqliteStorage::in_memory().unwrap();
        let default_topic = storage.get_or_create_default().await.unwrap();

        let result = storage.delete(default_topic.id).await;
        assert!(
            result.is_err(),
            "deleting the default topic should return an error"
        );
    }
}
