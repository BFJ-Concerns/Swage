//! SQLite implementation of PageStore.
//!
//! Pages are agent-run tabs on a space, assembled from module instances.
//! The database is authoritative for page content (the knowledge base's
//! posture): file renderings are derived projections, never the source.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

use swage_core::StorageError;

use crate::model::{NewPage, NewPageModule, Page, PageModule};
use crate::sqlite::SqliteStorage;
use crate::store::PageStore;

fn row_to_page(row: &rusqlite::Row<'_>) -> rusqlite::Result<Page> {
    let id: String = row.get(0)?;
    let space_id: String = row.get(1)?;
    let created: String = row.get(3)?;
    let updated: String = row.get(4)?;
    Ok(Page {
        id: parse_uuid(&id)?,
        space_id: parse_uuid(&space_id)?,
        title: row.get(2)?,
        created_at: parse_dt(&created)?,
        updated_at: parse_dt(&updated)?,
    })
}

fn row_to_module(row: &rusqlite::Row<'_>) -> rusqlite::Result<PageModule> {
    let id: String = row.get(0)?;
    let page_id: String = row.get(1)?;
    let created: String = row.get(5)?;
    let updated: String = row.get(6)?;
    Ok(PageModule {
        id: parse_uuid(&id)?,
        page_id: parse_uuid(&page_id)?,
        kind: row.get(2)?,
        position: row.get(3)?,
        source: row.get(4)?,
        created_at: parse_dt(&created)?,
        updated_at: parse_dt(&updated)?,
    })
}

pub(crate) fn parse_uuid(s: &str) -> rusqlite::Result<Uuid> {
    s.parse::<Uuid>().map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

pub(crate) fn parse_dt(s: &str) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })
}

#[async_trait]
impl PageStore for SqliteStorage {
    async fn create_page(&self, page: NewPage) -> Result<Page, StorageError> {
        self.with_conn(move |conn| {
            let id = Uuid::new_v4();
            let now = Utc::now();
            let now_str = now.to_rfc3339();
            conn.execute(
                "INSERT INTO pages (id, space_id, title, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?4)",
                params![
                    id.to_string(),
                    page.space_id.to_string(),
                    &page.title,
                    &now_str
                ],
            )?;
            Ok(Page {
                id,
                space_id: page.space_id,
                title: page.title,
                created_at: now,
                updated_at: now,
            })
        })
        .await
    }

    async fn get_page(&self, id: Uuid) -> Result<Option<Page>, StorageError> {
        self.with_conn(move |conn| {
            conn.query_row(
                "SELECT id, space_id, title, created_at, updated_at FROM pages WHERE id = ?1",
                params![id.to_string()],
                row_to_page,
            )
            .optional()
        })
        .await
    }

    async fn list_pages_by_space(&self, space_id: Uuid) -> Result<Vec<Page>, StorageError> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, space_id, title, created_at, updated_at
                 FROM pages WHERE space_id = ?1 ORDER BY created_at ASC",
            )?;
            let rows = stmt.query_map(params![space_id.to_string()], row_to_page)?;
            rows.collect()
        })
        .await
    }

    async fn create_page_module(&self, module: NewPageModule) -> Result<PageModule, StorageError> {
        self.with_conn(move |conn| {
            let id = Uuid::new_v4();
            let now = Utc::now();
            let now_str = now.to_rfc3339();
            conn.execute(
                "INSERT INTO page_modules (id, page_id, kind, position, source, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
                params![
                    id.to_string(),
                    module.page_id.to_string(),
                    &module.kind,
                    module.position,
                    &module.source,
                    &now_str,
                ],
            )?;
            conn.execute(
                "UPDATE pages SET updated_at = ?1 WHERE id = ?2",
                params![&now_str, module.page_id.to_string()],
            )?;
            Ok(PageModule {
                id,
                page_id: module.page_id,
                kind: module.kind,
                position: module.position,
                source: module.source,
                created_at: now,
                updated_at: now,
            })
        })
        .await
    }

    async fn get_page_module(&self, id: Uuid) -> Result<Option<PageModule>, StorageError> {
        self.with_conn(move |conn| {
            conn.query_row(
                "SELECT id, page_id, kind, position, source, created_at, updated_at
                 FROM page_modules WHERE id = ?1",
                params![id.to_string()],
                row_to_module,
            )
            .optional()
        })
        .await
    }

    async fn list_page_modules(&self, page_id: Uuid) -> Result<Vec<PageModule>, StorageError> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, page_id, kind, position, source, created_at, updated_at
                 FROM page_modules WHERE page_id = ?1 ORDER BY position ASC, created_at ASC",
            )?;
            let rows = stmt.query_map(params![page_id.to_string()], row_to_module)?;
            rows.collect()
        })
        .await
    }

    async fn update_page_module_source(&self, id: Uuid, source: &str) -> Result<(), StorageError> {
        let source = source.to_owned();
        self.with_conn(move |conn| {
            let now_str = Utc::now().to_rfc3339();
            conn.execute(
                "UPDATE page_modules SET source = ?1, updated_at = ?2 WHERE id = ?3",
                params![&source, &now_str, id.to_string()],
            )?;
            conn.execute(
                "UPDATE pages SET updated_at = ?1
                 WHERE id = (SELECT page_id FROM page_modules WHERE id = ?2)",
                params![&now_str, id.to_string()],
            )?;
            Ok(())
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use crate::model::{NewPage, NewPageModule, NewSpace};
    use crate::store::SpaceStore;

    use super::*;

    async fn make_space() -> (SqliteStorage, Uuid) {
        let storage = SqliteStorage::in_memory().unwrap();
        let space = SpaceStore::create(
            &storage,
            NewSpace {
                name: "p".into(),
                slug: "p".into(),
            },
        )
        .await
        .unwrap();
        (storage, space.id)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn pages_and_modules_round_trip_in_position_order() {
        let (storage, space_id) = make_space().await;
        let page = storage
            .create_page(NewPage {
                space_id,
                title: "Planning".into(),
            })
            .await
            .unwrap();

        let second = storage
            .create_page_module(NewPageModule {
                page_id: page.id,
                kind: "document".into(),
                position: 1,
                source: "## Later".into(),
            })
            .await
            .unwrap();
        let first = storage
            .create_page_module(NewPageModule {
                page_id: page.id,
                kind: "document".into(),
                position: 0,
                source: "# First".into(),
            })
            .await
            .unwrap();

        let modules = storage.list_page_modules(page.id).await.unwrap();
        assert_eq!(
            modules.iter().map(|m| m.id).collect::<Vec<_>>(),
            vec![first.id, second.id],
            "modules list in position order"
        );

        let listed = storage.list_pages_by_space(space_id).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].title, "Planning");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn writing_a_module_touches_its_page_updated_at() {
        let (storage, space_id) = make_space().await;
        let page = storage
            .create_page(NewPage {
                space_id,
                title: "Notes".into(),
            })
            .await
            .unwrap();
        let module = storage
            .create_page_module(NewPageModule {
                page_id: page.id,
                kind: "document".into(),
                position: 0,
                source: "before".into(),
            })
            .await
            .unwrap();

        storage
            .update_page_module_source(module.id, "after")
            .await
            .unwrap();

        let read = storage.get_page_module(module.id).await.unwrap().unwrap();
        assert_eq!(read.source, "after");
        let page_read = storage.get_page(page.id).await.unwrap().unwrap();
        assert!(
            page_read.updated_at >= read.created_at,
            "page carries the mutation's timestamp"
        );
    }
}
