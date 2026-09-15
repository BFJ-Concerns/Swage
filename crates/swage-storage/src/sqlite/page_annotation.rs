//! SQLite implementation of the page-annotation half of PageStore.
//!
//! Annotations are host-owned notes keyed to page and module ids — the shared
//! reference a user and an agent both point at. Anchoring stops at module
//! grain: a note names the module it hangs on, never a position inside that
//! module's content. Rows carry current truth only; the audit trail is where
//! a superseded body survives.

use async_trait::async_trait;
use chrono::Utc;
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

use swage_core::StorageError;

use crate::model::{NewAuditEvent, NewPageAnnotation, PageAnnotation};
use crate::sqlite::SqliteStorage;
use crate::sqlite::page::{parse_dt, parse_uuid};
use crate::store::PageAnnotationStore;

/// Tags live in one column as a JSON array: they are read and written whole,
/// never queried across, so a side table would buy nothing.
fn encode_tags(tags: &[String]) -> String {
    serde_json::to_string(tags).unwrap_or_else(|_| "[]".to_string())
}

fn decode_tags(raw: &str) -> Vec<String> {
    serde_json::from_str(raw).unwrap_or_default()
}

fn row_to_annotation(row: &rusqlite::Row<'_>) -> rusqlite::Result<PageAnnotation> {
    let id: String = row.get(0)?;
    let page_id: String = row.get(1)?;
    let module_id: String = row.get(2)?;
    let tags: String = row.get(4)?;
    let created: String = row.get(6)?;
    let updated: String = row.get(7)?;
    Ok(PageAnnotation {
        id: parse_uuid(&id)?,
        page_id: parse_uuid(&page_id)?,
        module_id: parse_uuid(&module_id)?,
        body: row.get(3)?,
        tags: decode_tags(&tags),
        author: row.get(5)?,
        created_at: parse_dt(&created)?,
        updated_at: parse_dt(&updated)?,
    })
}

const COLUMNS: &str = "id, page_id, module_id, body, tags, author, created_at, updated_at";

#[async_trait]
impl PageAnnotationStore for SqliteStorage {
    async fn create_page_annotation(
        &self,
        annotation: NewPageAnnotation,
    ) -> Result<PageAnnotation, StorageError> {
        self.with_conn(move |conn| {
            let id = Uuid::new_v4();
            let now = Utc::now();
            let now_str = now.to_rfc3339();
            let tags = encode_tags(&annotation.tags);
            conn.execute(
                "INSERT INTO page_annotations
                     (id, page_id, module_id, body, tags, author, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
                params![
                    id.to_string(),
                    annotation.page_id.to_string(),
                    annotation.module_id.to_string(),
                    &annotation.body,
                    &tags,
                    &annotation.author,
                    &now_str,
                ],
            )?;
            // An annotation is a change to the page: the surface's
            // "has this page moved?" read must see it.
            conn.execute(
                "UPDATE pages SET updated_at = ?1 WHERE id = ?2",
                params![&now_str, annotation.page_id.to_string()],
            )?;
            Ok(PageAnnotation {
                id,
                page_id: annotation.page_id,
                module_id: annotation.module_id,
                body: annotation.body,
                tags: annotation.tags,
                author: annotation.author,
                created_at: now,
                updated_at: now,
            })
        })
        .await
    }

    async fn get_page_annotation(&self, id: Uuid) -> Result<Option<PageAnnotation>, StorageError> {
        self.with_conn(move |conn| {
            conn.query_row(
                &format!("SELECT {COLUMNS} FROM page_annotations WHERE id = ?1"),
                params![id.to_string()],
                row_to_annotation,
            )
            .optional()
        })
        .await
    }

    async fn list_page_annotations(
        &self,
        page_id: Uuid,
    ) -> Result<Vec<PageAnnotation>, StorageError> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {COLUMNS} FROM page_annotations
                 WHERE page_id = ?1 ORDER BY created_at ASC, id ASC"
            ))?;
            let rows = stmt.query_map(params![page_id.to_string()], row_to_annotation)?;
            rows.collect()
        })
        .await
    }

    async fn update_page_annotation(
        &self,
        id: Uuid,
        body: &str,
        tags: &[String],
    ) -> Result<(), StorageError> {
        let body = body.to_owned();
        let tags = encode_tags(tags);
        self.with_conn(move |conn| {
            let now_str = Utc::now().to_rfc3339();
            conn.execute(
                "UPDATE page_annotations SET body = ?1, tags = ?2, updated_at = ?3 WHERE id = ?4",
                params![&body, &tags, &now_str, id.to_string()],
            )?;
            conn.execute(
                "UPDATE pages SET updated_at = ?1
                 WHERE id = (SELECT page_id FROM page_annotations WHERE id = ?2)",
                params![&now_str, id.to_string()],
            )?;
            Ok(())
        })
        .await
    }

    async fn delete_page_annotation_with_audit(
        &self,
        annotation: PageAnnotation,
        audit_event: NewAuditEvent,
    ) -> Result<bool, StorageError> {
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let deleted = tx.execute(
                "DELETE FROM page_annotations WHERE id = ?1",
                params![annotation.id.to_string()],
            )?;
            if deleted == 0 {
                return Ok(false);
            }

            let now_str = Utc::now().to_rfc3339();
            tx.execute(
                "UPDATE pages SET updated_at = ?1 WHERE id = ?2",
                params![&now_str, annotation.page_id.to_string()],
            )?;
            let metadata_json = serde_json::to_string(&audit_event.metadata)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
            tx.execute(
                "INSERT INTO audit_events (
                    id, session_id, entity_kind, entity_id, event_kind, summary, metadata_json, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    audit_event.id,
                    audit_event.session_id.map(|id| id.to_string()),
                    audit_event.entity_kind,
                    audit_event.entity_id,
                    audit_event.event_kind,
                    audit_event.summary,
                    metadata_json,
                    now_str,
                ],
            )?;
            tx.commit()?;
            Ok(true)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use crate::model::{NewPage, NewPageModule, NewTopic};
    use crate::store::{PageStore, TopicStore};

    use super::*;

    async fn add_module(storage: &SqliteStorage, page_id: Uuid, position: i32) -> Uuid {
        storage
            .create_page_module(NewPageModule {
                page_id,
                kind: "document".into(),
                position,
                source: "# Draft".into(),
            })
            .await
            .unwrap()
            .id
    }

    async fn seeded() -> (SqliteStorage, Uuid, Uuid) {
        let storage = SqliteStorage::in_memory().unwrap();
        let topic = TopicStore::create(
            &storage,
            NewTopic {
                name: "p".into(),
                slug: "p".into(),
            },
        )
        .await
        .unwrap();
        let page = storage
            .create_page(NewPage {
                topic_id: topic.id,
                title: "Planning".into(),
            })
            .await
            .unwrap();
        let module = add_module(&storage, page.id, 0).await;
        (storage, page.id, module)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn annotations_round_trip_addressed_to_their_module() {
        let (storage, page_id, module_id) = seeded().await;

        let second_module = add_module(&storage, page_id, 1).await;

        let first = storage
            .create_page_annotation(NewPageAnnotation {
                page_id,
                module_id,
                body: "this paragraph contradicts the summary".into(),
                tags: vec!["question".into(), "blocking".into()],
                author: "user".into(),
            })
            .await
            .unwrap();
        let second = storage
            .create_page_annotation(NewPageAnnotation {
                page_id,
                module_id: second_module,
                body: "needs a second pass".into(),
                tags: Vec::new(),
                author: "assistant".into(),
            })
            .await
            .unwrap();

        let listed = storage.list_page_annotations(page_id).await.unwrap();
        assert_eq!(
            listed.iter().map(|a| a.id).collect::<Vec<_>>(),
            vec![first.id, second.id],
            "annotations list oldest first"
        );
        let read = storage
            .get_page_annotation(first.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            read.module_id, module_id,
            "the note is addressed to the module it hangs on"
        );
        assert_eq!(read.tags, vec!["question", "blocking"]);
        assert_eq!(read.author, "user");
        assert_eq!(
            listed[1].module_id, second_module,
            "each note names its own module — the page as a whole is not an anchor"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn updating_an_annotation_replaces_body_and_tags() {
        let (storage, page_id, module_id) = seeded().await;
        let annotation = storage
            .create_page_annotation(NewPageAnnotation {
                page_id,
                module_id,
                body: "before".into(),
                tags: vec!["draft".into()],
                author: "user".into(),
            })
            .await
            .unwrap();

        storage
            .update_page_annotation(annotation.id, "after", &["settled".to_string()])
            .await
            .unwrap();

        let read = storage
            .get_page_annotation(annotation.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(read.body, "after");
        assert_eq!(read.tags, vec!["settled"], "tags are replaced, not merged");
        assert_eq!(read.created_at, annotation.created_at, "identity survives");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn deleting_a_module_takes_its_annotations_with_it() {
        let (storage, page_id, module_id) = seeded().await;
        let survivor = add_module(&storage, page_id, 1).await;
        storage
            .create_page_annotation(NewPageAnnotation {
                page_id,
                module_id,
                body: "hangs on the deleted module".into(),
                tags: Vec::new(),
                author: "user".into(),
            })
            .await
            .unwrap();
        storage
            .create_page_annotation(NewPageAnnotation {
                page_id,
                module_id: survivor,
                body: "hangs on the module that stays".into(),
                tags: Vec::new(),
                author: "user".into(),
            })
            .await
            .unwrap();

        storage
            .with_conn(move |conn| {
                conn.execute(
                    "DELETE FROM page_modules WHERE id = ?1",
                    params![module_id.to_string()],
                )?;
                Ok(())
            })
            .await
            .unwrap();

        let remaining = storage.list_page_annotations(page_id).await.unwrap();
        assert_eq!(
            remaining.len(),
            1,
            "a note goes with the module it hangs on, and only that one"
        );
        assert_eq!(remaining[0].body, "hangs on the module that stays");
    }
}
