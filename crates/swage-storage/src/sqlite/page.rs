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

use crate::model::{
    NewPage, NewPageModule, NewPageTemplate, NewPageTemplateModule, Page, PageModule, PageTemplate,
    PageTemplateModule,
};
use crate::sqlite::SqliteStorage;
use crate::store::PageStore;

/// Every page read selects the same column list, lineage included.
const PAGE_COLUMNS: &str =
    "id, space_id, title, created_at, updated_at, forked_from_template_id, forked_at";

fn row_to_page(row: &rusqlite::Row<'_>) -> rusqlite::Result<Page> {
    let id: String = row.get(0)?;
    let space_id: String = row.get(1)?;
    let created: String = row.get(3)?;
    let updated: String = row.get(4)?;
    let forked_from: Option<String> = row.get(5)?;
    let forked_at: Option<String> = row.get(6)?;
    Ok(Page {
        id: parse_uuid(&id)?,
        space_id: parse_uuid(&space_id)?,
        title: row.get(2)?,
        created_at: parse_dt(&created)?,
        updated_at: parse_dt(&updated)?,
        forked_from_template_id: forked_from.as_deref().map(parse_uuid).transpose()?,
        forked_at: forked_at.as_deref().map(parse_dt).transpose()?,
    })
}

fn row_to_template(row: &rusqlite::Row<'_>) -> rusqlite::Result<PageTemplate> {
    let id: String = row.get(0)?;
    let space_id: String = row.get(1)?;
    let created: String = row.get(3)?;
    let updated: String = row.get(4)?;
    Ok(PageTemplate {
        id: parse_uuid(&id)?,
        space_id: parse_uuid(&space_id)?,
        name: row.get(2)?,
        created_at: parse_dt(&created)?,
        updated_at: parse_dt(&updated)?,
    })
}

fn row_to_template_module(row: &rusqlite::Row<'_>) -> rusqlite::Result<PageTemplateModule> {
    let id: String = row.get(0)?;
    let template_id: String = row.get(1)?;
    let created: String = row.get(5)?;
    let updated: String = row.get(6)?;
    Ok(PageTemplateModule {
        id: parse_uuid(&id)?,
        template_id: parse_uuid(&template_id)?,
        kind: row.get(2)?,
        position: row.get(3)?,
        source: row.get(4)?,
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
                forked_from_template_id: None,
                forked_at: None,
            })
        })
        .await
    }

    async fn get_page(&self, id: Uuid) -> Result<Option<Page>, StorageError> {
        self.with_conn(move |conn| {
            conn.query_row(
                &format!("SELECT {PAGE_COLUMNS} FROM pages WHERE id = ?1"),
                params![id.to_string()],
                row_to_page,
            )
            .optional()
        })
        .await
    }

    async fn list_pages_by_space(&self, space_id: Uuid) -> Result<Vec<Page>, StorageError> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {PAGE_COLUMNS} FROM pages WHERE space_id = ?1 ORDER BY created_at ASC"
            ))?;
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

    async fn create_page_template(
        &self,
        template: NewPageTemplate,
    ) -> Result<PageTemplate, StorageError> {
        self.with_conn(move |conn| {
            let id = Uuid::new_v4();
            let now = Utc::now();
            let now_str = now.to_rfc3339();
            conn.execute(
                "INSERT INTO page_templates (id, space_id, name, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?4)",
                params![
                    id.to_string(),
                    template.space_id.to_string(),
                    &template.name,
                    &now_str
                ],
            )?;
            Ok(PageTemplate {
                id,
                space_id: template.space_id,
                name: template.name,
                created_at: now,
                updated_at: now,
            })
        })
        .await
    }

    async fn get_page_template(&self, id: Uuid) -> Result<Option<PageTemplate>, StorageError> {
        self.with_conn(move |conn| {
            conn.query_row(
                "SELECT id, space_id, name, created_at, updated_at
                 FROM page_templates WHERE id = ?1",
                params![id.to_string()],
                row_to_template,
            )
            .optional()
        })
        .await
    }

    async fn list_page_templates_by_space(
        &self,
        space_id: Uuid,
    ) -> Result<Vec<PageTemplate>, StorageError> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, space_id, name, created_at, updated_at
                 FROM page_templates WHERE space_id = ?1 ORDER BY created_at ASC",
            )?;
            let rows = stmt.query_map(params![space_id.to_string()], row_to_template)?;
            rows.collect()
        })
        .await
    }

    async fn create_page_template_module(
        &self,
        module: NewPageTemplateModule,
    ) -> Result<PageTemplateModule, StorageError> {
        self.with_conn(move |conn| {
            let id = Uuid::new_v4();
            let now = Utc::now();
            let now_str = now.to_rfc3339();
            conn.execute(
                "INSERT INTO page_template_modules
                     (id, template_id, kind, position, source, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
                params![
                    id.to_string(),
                    module.template_id.to_string(),
                    &module.kind,
                    module.position,
                    &module.source,
                    &now_str,
                ],
            )?;
            conn.execute(
                "UPDATE page_templates SET updated_at = ?1 WHERE id = ?2",
                params![&now_str, module.template_id.to_string()],
            )?;
            Ok(PageTemplateModule {
                id,
                template_id: module.template_id,
                kind: module.kind,
                position: module.position,
                source: module.source,
                created_at: now,
                updated_at: now,
            })
        })
        .await
    }

    async fn list_page_template_modules(
        &self,
        template_id: Uuid,
    ) -> Result<Vec<PageTemplateModule>, StorageError> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, template_id, kind, position, source, created_at, updated_at
                 FROM page_template_modules WHERE template_id = ?1
                 ORDER BY position ASC, created_at ASC",
            )?;
            let rows = stmt.query_map(params![template_id.to_string()], row_to_template_module)?;
            rows.collect()
        })
        .await
    }

    async fn create_page_from_template(
        &self,
        page: NewPage,
        template_id: Uuid,
    ) -> Result<Page, StorageError> {
        self.with_conn(move |conn| {
            let id = Uuid::new_v4();
            let now = Utc::now();
            let now_str = now.to_rfc3339();
            // One transaction: a page whose seeded modules are missing is
            // never observable, and a failed copy leaves no page behind.
            let tx = conn.unchecked_transaction()?;
            tx.execute(
                "INSERT INTO pages
                     (id, space_id, title, created_at, updated_at,
                      forked_from_template_id, forked_at)
                 VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?4)",
                params![
                    id.to_string(),
                    page.space_id.to_string(),
                    &page.title,
                    &now_str,
                    template_id.to_string(),
                ],
            )?;
            let seeds: Vec<PageTemplateModule> = {
                let mut stmt = tx.prepare(
                    "SELECT id, template_id, kind, position, source, created_at, updated_at
                     FROM page_template_modules WHERE template_id = ?1
                     ORDER BY position ASC, created_at ASC",
                )?;
                let rows =
                    stmt.query_map(params![template_id.to_string()], row_to_template_module)?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };
            // The seeds are copied into rows of the page's own: new ids, new
            // timestamps. Nothing links back but the lineage column.
            for (position, seed) in seeds.into_iter().enumerate() {
                tx.execute(
                    "INSERT INTO page_modules
                         (id, page_id, kind, position, source, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
                    params![
                        Uuid::new_v4().to_string(),
                        id.to_string(),
                        &seed.kind,
                        position as i32,
                        &seed.source,
                        &now_str,
                    ],
                )?;
            }
            tx.commit()?;
            Ok(Page {
                id,
                space_id: page.space_id,
                title: page.title,
                created_at: now,
                updated_at: now,
                forked_from_template_id: Some(template_id),
                forked_at: Some(now),
            })
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use crate::model::{NewPage, NewPageModule, NewPageTemplate, NewPageTemplateModule, NewSpace};
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

    /// A template with two module rows, out of insertion order so the seed
    /// path cannot pass by accident of insertion sequence.
    async fn seeded_template(storage: &SqliteStorage, space_id: Uuid) -> Uuid {
        let template = storage
            .create_page_template(NewPageTemplate {
                space_id,
                name: "Standup".into(),
            })
            .await
            .unwrap();
        storage
            .create_page_template_module(NewPageTemplateModule {
                template_id: template.id,
                kind: "document".into(),
                position: 1,
                source: "## Blockers".into(),
            })
            .await
            .unwrap();
        storage
            .create_page_template_module(NewPageTemplateModule {
                template_id: template.id,
                kind: "document".into(),
                position: 0,
                source: "## Yesterday".into(),
            })
            .await
            .unwrap();
        template.id
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn templates_and_their_modules_round_trip_in_position_order() {
        let (storage, space_id) = make_space().await;
        let template_id = seeded_template(&storage, space_id).await;

        let modules = storage
            .list_page_template_modules(template_id)
            .await
            .unwrap();
        assert_eq!(
            modules
                .iter()
                .map(|m| m.source.as_str())
                .collect::<Vec<_>>(),
            vec!["## Yesterday", "## Blockers"],
            "template modules list in position order"
        );

        let listed = storage
            .list_page_templates_by_space(space_id)
            .await
            .unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "Standup");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_page_forked_from_a_template_carries_its_modules_and_its_lineage() {
        let (storage, space_id) = make_space().await;
        let template_id = seeded_template(&storage, space_id).await;

        let page = storage
            .create_page_from_template(
                NewPage {
                    space_id,
                    title: "Monday standup".into(),
                },
                template_id,
            )
            .await
            .unwrap();

        let modules = storage.list_page_modules(page.id).await.unwrap();
        assert_eq!(
            modules
                .iter()
                .map(|m| (m.position, m.source.as_str()))
                .collect::<Vec<_>>(),
            vec![(0, "## Yesterday"), (1, "## Blockers")],
            "seeded modules copied in the template's position order"
        );

        let read = storage.get_page(page.id).await.unwrap().unwrap();
        assert_eq!(
            read.forked_from_template_id,
            Some(template_id),
            "lineage reads back from the stored row"
        );
        assert!(
            read.forked_at.is_some(),
            "the fork records when it happened"
        );

        let blank = storage
            .create_page(NewPage {
                space_id,
                title: "Scratch".into(),
            })
            .await
            .unwrap();
        let blank_read = storage.get_page(blank.id).await.unwrap().unwrap();
        assert_eq!(
            blank_read.forked_from_template_id, None,
            "a page created blank carries no lineage"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn editing_a_forked_page_leaves_the_template_untouched() {
        let (storage, space_id) = make_space().await;
        let template_id = seeded_template(&storage, space_id).await;
        let before = storage
            .list_page_template_modules(template_id)
            .await
            .unwrap();

        let page = storage
            .create_page_from_template(
                NewPage {
                    space_id,
                    title: "Monday standup".into(),
                },
                template_id,
            )
            .await
            .unwrap();
        let seeded = storage.list_page_modules(page.id).await.unwrap();
        storage
            .update_page_module_source(seeded[0].id, "## Yesterday: shipped the fork")
            .await
            .unwrap();

        let after = storage
            .list_page_template_modules(template_id)
            .await
            .unwrap();
        assert_eq!(
            after
                .iter()
                .map(|m| (m.id, m.source.as_str(), m.updated_at))
                .collect::<Vec<_>>(),
            before
                .iter()
                .map(|m| (m.id, m.source.as_str(), m.updated_at))
                .collect::<Vec<_>>(),
            "the template's rows are unchanged by an edit to a page forked from it"
        );
        assert!(
            seeded.iter().all(|module| !before
                .iter()
                .any(|template_module| template_module.id == module.id)),
            "the forked page's modules are rows of its own, not the template's"
        );
    }
}
