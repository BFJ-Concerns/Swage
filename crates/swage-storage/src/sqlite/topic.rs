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

/// Every topic read selects these columns, in `row_to_topic` order.
const TOPIC_COLUMNS: &str = "id, name, description, is_general, slug, created_at, updated_at, tags";

fn row_to_topic(row: &rusqlite::Row<'_>) -> rusqlite::Result<Topic> {
    let id: String = row.get(0)?;
    let created: String = row.get(5)?;
    let updated: String = row.get(6)?;
    let slug: String = row.get(4)?;
    let tags: String = row.get(7)?;

    Ok(Topic {
        id: parse_uuid(&id)?,
        name: row.get(1)?,
        description: row.get(2)?,
        is_general: row.get::<_, i64>(3)? != 0,
        slug: parse_slug(&slug)?,
        tags: decode_tags(&tags),
        created_at: parse_dt(&created)?,
        updated_at: parse_dt(&updated)?,
    })
}

/// Tags live in one column as a JSON array: read and written whole.
pub(crate) fn encode_tags(tags: &[String]) -> String {
    serde_json::to_string(tags).unwrap_or_else(|_| "[]".to_string())
}

pub(crate) fn decode_tags(raw: &str) -> Vec<String> {
    serde_json::from_str(raw).unwrap_or_default()
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
                "INSERT INTO topics (id, name, description, is_general, slug, created_at, updated_at)
                 VALUES (?1, ?2, '', 0, ?3, ?4, ?4)",
                params![id.to_string(), &topic.name, topic.slug.as_str(), &now_str],
            )?;

            Ok(Topic {
                id,
                name: topic.name,
                description: String::new(),
                is_general: false,
                slug: topic.slug,
                tags: Vec::new(),
                created_at: now,
                updated_at: now,
            })
        })
        .await
    }

    async fn get(&self, id: Uuid) -> Result<Option<Topic>, StorageError> {
        self.with_conn(|conn| {
            conn.query_row(
                &format!(
                    "SELECT {TOPIC_COLUMNS}
                 FROM topics WHERE id = ?1"
                ),
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
                &format!(
                    "SELECT {TOPIC_COLUMNS}
                 FROM topics WHERE name = ?1"
                ),
                params![&name],
                row_to_topic,
            )
            .optional()
        })
        .await
    }

    async fn general_scope(&self) -> Result<Topic, StorageError> {
        if let Some(topic) = self
            .with_conn(|conn| {
                conn.query_row(
                    &format!(
                        "SELECT {TOPIC_COLUMNS}
                     FROM topics
                     WHERE is_general = 1"
                    ),
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
                "INSERT INTO topics (id, name, description, is_general, slug, created_at, updated_at)
                 VALUES (?1, 'general', '', 1, 'general', ?2, ?2)",
                params![id.to_string(), &now_str],
            )?;

            Ok(Topic {
                id,
                name: "general".into(),
                description: String::new(),
                is_general: true,
                slug: "general".into(),
                tags: Vec::new(),
                created_at: now,
                updated_at: now,
            })
        })
        .await
    }

    async fn list_all(&self) -> Result<Vec<Topic>, StorageError> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {TOPIC_COLUMNS}
                 FROM topics WHERE is_general = 0 ORDER BY created_at ASC"
            ))?;
            let rows = stmt.query_map([], row_to_topic)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
        .await
    }

    async fn list_scopes(&self) -> Result<Vec<Topic>, StorageError> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {TOPIC_COLUMNS}
                 FROM topics ORDER BY is_general DESC, created_at ASC"
            ))?;
            let rows = stmt.query_map([], row_to_topic)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
        .await
    }

    async fn get_by_slug(&self, slug: &str) -> Result<Option<Topic>, StorageError> {
        let slug = slug.to_owned();
        self.with_conn(move |conn| {
            conn.query_row(
                &format!(
                    "SELECT {TOPIC_COLUMNS}
                 FROM topics WHERE slug = ?1"
                ),
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
        tags: Option<Vec<String>>,
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
                     tags           = COALESCE(?6, tags),
                     updated_at     = ?5
                 WHERE id = ?1",
                params![
                    id_str,
                    name,
                    description,
                    slug.as_ref().map(TopicSlug::as_str),
                    now,
                    tags.as_deref().map(encode_tags),
                ],
            )?;

            conn.query_row(
                &format!(
                    "SELECT {TOPIC_COLUMNS}
                 FROM topics WHERE id = ?1"
                ),
                params![id_str],
                row_to_topic,
            )
        })
        .await
    }

    async fn delete(&self, id: Uuid) -> Result<bool, StorageError> {
        // Pre-flight: fetch the topic to check existence and guard the general
        // scope, which every topicless session belongs to.
        match self.get(id).await? {
            None => return Ok(false),
            Some(ref p) if p.is_general => {
                return Err(StorageError::ConstraintViolation(
                    "the general scope is not a topic and cannot be deleted".into(),
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
    async fn topic_tags_round_trip_and_survive_other_updates() {
        let storage = SqliteStorage::in_memory().unwrap();
        let created = storage
            .create(NewTopic {
                name: "Ops".into(),
                slug: "ops".into(),
            })
            .await
            .unwrap();
        assert!(created.tags.is_empty());

        let tagged = storage
            .update(
                created.id,
                None,
                None,
                None,
                Some(vec!["ops".into(), "status".into()]),
            )
            .await
            .unwrap();
        assert_eq!(tagged.tags, vec!["ops", "status"]);

        // A later update that says nothing about tags leaves them alone.
        let renamed = storage
            .update(created.id, Some("Operations".into()), None, None, None)
            .await
            .unwrap();
        assert_eq!(renamed.tags, vec!["ops", "status"]);
        let listed = storage.list_all().await.unwrap();
        assert_eq!(listed[0].tags, vec!["ops", "status"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn general_scope_is_idempotent() {
        let storage = SqliteStorage::in_memory().unwrap();
        let p1 = storage.general_scope().await.unwrap();
        let p2 = storage.general_scope().await.unwrap();
        // Same row returned both times — not duplicated.
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
    async fn general_scope_is_seeded_hidden_and_located_beside_the_topics() {
        let storage = SqliteStorage::in_memory().unwrap();
        let general = storage.general_scope().await.unwrap();
        assert!(general.is_general);
        assert_eq!(general.slug, "general");
        assert_eq!(general.location(), swage_core::ScopeLocation::General);
        assert_eq!(general.client_topic_id(), None);

        // Never a topic the user can choose: absent from the topic list,
        // first in the scope enumeration.
        let topics = storage.list_all().await.unwrap();
        assert!(topics.iter().all(|topic| !topic.is_general));
        let scopes = storage.list_scopes().await.unwrap();
        assert_eq!(scopes[0].id, general.id);

        let delete_error = storage.delete(general.id).await.unwrap_err();
        assert!(
            matches!(delete_error, StorageError::ConstraintViolation(_)),
            "the general scope is protected"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_topic_is_located_under_the_topics_tree_and_projects_its_id() {
        let storage = SqliteStorage::in_memory().unwrap();
        let topic = storage
            .create(NewTopic {
                name: "Alloys".into(),
                slug: "alloys".into(),
            })
            .await
            .unwrap();
        assert!(!topic.is_general);
        assert_eq!(topic.location(), swage_core::ScopeLocation::topic("alloys"));
        assert_eq!(topic.client_topic_id(), Some(topic.id));
    }

    // ---------------------------------------------------------------------------
    // list_all
    // ---------------------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn list_all_returns_topics_ordered_by_created_at() {
        let storage = SqliteStorage::in_memory().unwrap();
        // The general scope is seeded but never listed; add two topics.
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

        assert_eq!(all.len(), 2, "the general scope is not a topic");

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
                None,
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
            .update(
                created.id,
                None,
                Some("Operations topic".into()),
                None,
                None,
            )
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

        let updated = storage
            .update(created.id, None, None, None, None)
            .await
            .unwrap();

        assert_eq!(updated.name, "Stable");
        assert_eq!(updated.slug, "stable");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn update_errors_for_unknown_id() {
        let storage = SqliteStorage::in_memory().unwrap();
        let nonexistent = Uuid::new_v4();
        let result = storage
            .update(nonexistent, Some("X".into()), None, None, None)
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
    async fn delete_refuses_the_general_scope() {
        let storage = SqliteStorage::in_memory().unwrap();
        let general = storage.general_scope().await.unwrap();

        let result = storage.delete(general.id).await;
        assert!(
            result.is_err(),
            "deleting the general scope should return an error"
        );
    }
}
