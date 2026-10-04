//! SQLite implementation of SurfaceStore.
//!
//! Surfaces are a session's work surfaces, assembled from module instances,
//! and the library is where a surface is preserved for other sessions to add.
//! The database is authoritative for surface content (the knowledge base's
//! posture): file renderings are derived projections, never the source.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

use swage_core::StorageError;

use crate::model::{
    LibraryEntry, LibraryEntryModule, LibrarySeeding, ModuleSourceReplacement, NewAuditEvent,
    NewLibraryEntry, NewLibraryEntryModule, NewSurface, NewSurfaceModule, RemovedSurfaceModule,
    Surface, SurfaceModule,
};
use crate::sqlite::SqliteStorage;
use crate::sqlite::surface_annotation::{COLUMNS as ANNOTATION_COLUMNS, row_to_annotation};
use crate::sqlite::topic::{decode_tags, encode_tags};
use crate::store::SurfaceStore;

/// Every surface read selects the same column list, lineage included.
const SURFACE_COLUMNS: &str =
    "id, session_id, title, created_at, updated_at, library_entry_id, seeded_at, seeded_from_name";
const ENTRY_COLUMNS: &str = "id, name, description, tags, origin_topic_id, saved_from_surface_id, \
    created_at, updated_at";
const MODULE_COLUMNS: &str =
    "id, surface_id, kind, position, source, created_at, updated_at, revision";
const ENTRY_MODULE_COLUMNS: &str = "id, entry_id, kind, position, source, created_at, updated_at";

fn row_to_surface(row: &rusqlite::Row<'_>) -> rusqlite::Result<Surface> {
    let id: String = row.get(0)?;
    let session_id: String = row.get(1)?;
    let created: String = row.get(3)?;
    let updated: String = row.get(4)?;
    let entry: Option<String> = row.get(5)?;
    let seeded_at: Option<String> = row.get(6)?;
    Ok(Surface {
        id: parse_uuid(&id)?,
        session_id: parse_uuid(&session_id)?,
        title: row.get(2)?,
        created_at: parse_dt(&created)?,
        updated_at: parse_dt(&updated)?,
        library_entry_id: entry.as_deref().map(parse_uuid).transpose()?,
        seeded_from_name: row.get(7)?,
        seeded_at: seeded_at.as_deref().map(parse_dt).transpose()?,
    })
}

fn row_to_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<LibraryEntry> {
    let id: String = row.get(0)?;
    let tags: String = row.get(3)?;
    let origin: String = row.get(4)?;
    let saved_from: Option<String> = row.get(5)?;
    let created: String = row.get(6)?;
    let updated: String = row.get(7)?;
    Ok(LibraryEntry {
        id: parse_uuid(&id)?,
        name: row.get(1)?,
        description: row.get(2)?,
        tags: decode_tags(&tags),
        origin_topic_id: parse_uuid(&origin)?,
        saved_from_surface_id: saved_from.as_deref().map(parse_uuid).transpose()?,
        created_at: parse_dt(&created)?,
        updated_at: parse_dt(&updated)?,
    })
}

fn row_to_entry_module(row: &rusqlite::Row<'_>) -> rusqlite::Result<LibraryEntryModule> {
    let id: String = row.get(0)?;
    let entry_id: String = row.get(1)?;
    let created: String = row.get(5)?;
    let updated: String = row.get(6)?;
    Ok(LibraryEntryModule {
        id: parse_uuid(&id)?,
        entry_id: parse_uuid(&entry_id)?,
        kind: row.get(2)?,
        position: row.get(3)?,
        source: row.get(4)?,
        created_at: parse_dt(&created)?,
        updated_at: parse_dt(&updated)?,
    })
}

fn row_to_module(row: &rusqlite::Row<'_>) -> rusqlite::Result<SurfaceModule> {
    let id: String = row.get(0)?;
    let surface_id: String = row.get(1)?;
    let created: String = row.get(5)?;
    let updated: String = row.get(6)?;
    Ok(SurfaceModule {
        id: parse_uuid(&id)?,
        surface_id: parse_uuid(&surface_id)?,
        kind: row.get(2)?,
        position: row.get(3)?,
        source: row.get(4)?,
        revision: row.get(7)?,
        created_at: parse_dt(&created)?,
        updated_at: parse_dt(&updated)?,
    })
}

fn module_by_id(conn: &rusqlite::Connection, id: Uuid) -> rusqlite::Result<Option<SurfaceModule>> {
    conn.query_row(
        &format!("SELECT {MODULE_COLUMNS} FROM surface_modules WHERE id = ?1"),
        params![id.to_string()],
        row_to_module,
    )
    .optional()
}

fn modules_in_order(
    conn: &rusqlite::Connection,
    surface_id: Uuid,
) -> rusqlite::Result<Vec<SurfaceModule>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {MODULE_COLUMNS} FROM surface_modules WHERE surface_id = ?1
         ORDER BY position ASC, created_at ASC"
    ))?;
    let rows = stmt.query_map(params![surface_id.to_string()], row_to_module)?;
    rows.collect()
}

/// Write `ordered` back as the surface's module order — positions 0..n — and
/// stamp the surface. Every removal and move passes through here, so the
/// position column stays contiguous and a position is always an index.
fn write_module_order(
    conn: &rusqlite::Connection,
    surface_id: Uuid,
    ordered: Vec<SurfaceModule>,
) -> rusqlite::Result<Vec<SurfaceModule>> {
    let now_str = Utc::now().to_rfc3339();
    let mut written = Vec::with_capacity(ordered.len());
    for (index, module) in ordered.into_iter().enumerate() {
        let position = index as i32;
        if module.position != position {
            conn.execute(
                "UPDATE surface_modules SET position = ?1 WHERE id = ?2",
                params![position, module.id.to_string()],
            )?;
        }
        written.push(SurfaceModule { position, ..module });
    }
    conn.execute(
        "UPDATE surfaces SET updated_at = ?1 WHERE id = ?2",
        params![&now_str, surface_id.to_string()],
    )?;
    Ok(written)
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
impl SurfaceStore for SqliteStorage {
    async fn create_page(&self, surface: NewSurface) -> Result<Surface, StorageError> {
        self.with_conn(move |conn| {
            let id = Uuid::new_v4();
            let now = Utc::now();
            let now_str = now.to_rfc3339();
            conn.execute(
                "INSERT INTO surfaces (id, session_id, title, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?4)",
                params![
                    id.to_string(),
                    surface.session_id.to_string(),
                    &surface.title,
                    &now_str
                ],
            )?;
            Ok(Surface {
                id,
                session_id: surface.session_id,
                title: surface.title,
                created_at: now,
                updated_at: now,
                library_entry_id: None,
                seeded_from_name: None,
                seeded_at: None,
            })
        })
        .await
    }

    async fn get_page(&self, id: Uuid) -> Result<Option<Surface>, StorageError> {
        self.with_conn(move |conn| {
            conn.query_row(
                &format!("SELECT {SURFACE_COLUMNS} FROM surfaces WHERE id = ?1"),
                params![id.to_string()],
                row_to_surface,
            )
            .optional()
        })
        .await
    }

    async fn list_surfaces_by_session(
        &self,
        session_id: Uuid,
    ) -> Result<Vec<Surface>, StorageError> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {SURFACE_COLUMNS} FROM surfaces WHERE session_id = ?1
                 ORDER BY created_at ASC"
            ))?;
            let rows = stmt.query_map(params![session_id.to_string()], row_to_surface)?;
            rows.collect()
        })
        .await
    }

    async fn create_surface_module(
        &self,
        module: NewSurfaceModule,
    ) -> Result<SurfaceModule, StorageError> {
        self.with_conn(move |conn| {
            let id = Uuid::new_v4();
            let now = Utc::now();
            let now_str = now.to_rfc3339();
            conn.execute(
                "INSERT INTO surface_modules (id, surface_id, kind, position, source, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
                params![
                    id.to_string(),
                    module.surface_id.to_string(),
                    &module.kind,
                    module.position,
                    &module.source,
                    &now_str,
                ],
            )?;
            conn.execute(
                "UPDATE surfaces SET updated_at = ?1 WHERE id = ?2",
                params![&now_str, module.surface_id.to_string()],
            )?;
            Ok(SurfaceModule {
                id,
                surface_id: module.surface_id,
                kind: module.kind,
                position: module.position,
                source: module.source,
                revision: 0,
                created_at: now,
                updated_at: now,
            })
        })
        .await
    }

    async fn get_surface_module(&self, id: Uuid) -> Result<Option<SurfaceModule>, StorageError> {
        self.with_conn(move |conn| {
            conn.query_row(
                &format!("SELECT {MODULE_COLUMNS} FROM surface_modules WHERE id = ?1"),
                params![id.to_string()],
                row_to_module,
            )
            .optional()
        })
        .await
    }

    async fn list_surface_modules(
        &self,
        surface_id: Uuid,
    ) -> Result<Vec<SurfaceModule>, StorageError> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {MODULE_COLUMNS} FROM surface_modules WHERE surface_id = ?1
                 ORDER BY position ASC, created_at ASC"
            ))?;
            let rows = stmt.query_map(params![surface_id.to_string()], row_to_module)?;
            rows.collect()
        })
        .await
    }

    async fn replace_surface_module_source(
        &self,
        id: Uuid,
        expected_revision: i64,
        source: &str,
    ) -> Result<Option<ModuleSourceReplacement>, StorageError> {
        let source = source.to_owned();
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let now_str = Utc::now().to_rfc3339();
            // The revision is the guard: the update lands only on the row the
            // writer read. A zero-row update is either the stale case or a
            // missing module — the read-back tells the two apart.
            let changed = tx.execute(
                "UPDATE surface_modules
                 SET source = ?1, revision = revision + 1, updated_at = ?2
                 WHERE id = ?3 AND revision = ?4",
                params![&source, &now_str, id.to_string(), expected_revision],
            )?;
            if changed == 1 {
                tx.execute(
                    "UPDATE surfaces SET updated_at = ?1
                     WHERE id = (SELECT surface_id FROM surface_modules WHERE id = ?2)",
                    params![&now_str, id.to_string()],
                )?;
            }
            let current = module_by_id(&tx, id)?;
            tx.commit()?;
            Ok(current.map(|module| {
                if changed == 1 {
                    ModuleSourceReplacement::Replaced(module)
                } else {
                    ModuleSourceReplacement::Stale(module)
                }
            }))
        })
        .await
    }

    async fn remove_surface_module(
        &self,
        id: Uuid,
    ) -> Result<Option<RemovedSurfaceModule>, StorageError> {
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let Some(module) = module_by_id(&tx, id)? else {
                return Ok(None);
            };
            let annotations = {
                let mut stmt = tx.prepare(&format!(
                    "SELECT {ANNOTATION_COLUMNS} FROM surface_annotations
                     WHERE module_id = ?1 ORDER BY created_at ASC, id ASC"
                ))?;
                let rows = stmt.query_map(params![id.to_string()], row_to_annotation)?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };
            // The notes go with the module they hang on — removed here as
            // one visible statement, not left to the foreign-key cascade.
            tx.execute(
                "DELETE FROM surface_annotations WHERE module_id = ?1",
                params![id.to_string()],
            )?;
            tx.execute(
                "DELETE FROM surface_modules WHERE id = ?1",
                params![id.to_string()],
            )?;
            let remaining = modules_in_order(&tx, module.surface_id)?;
            write_module_order(&tx, module.surface_id, remaining)?;
            tx.commit()?;
            Ok(Some(RemovedSurfaceModule {
                module,
                annotations,
            }))
        })
        .await
    }

    async fn move_surface_module(
        &self,
        id: Uuid,
        position: i32,
    ) -> Result<Option<Vec<SurfaceModule>>, StorageError> {
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let Some(module) = module_by_id(&tx, id)? else {
                return Ok(None);
            };
            let mut ordered = modules_in_order(&tx, module.surface_id)?;
            let from = ordered
                .iter()
                .position(|item| item.id == module.id)
                .expect("a module lists among its own surface's modules");
            let moved = ordered.remove(from);
            let to = position.clamp(0, ordered.len() as i32) as usize;
            ordered.insert(to, moved);
            let written = write_module_order(&tx, module.surface_id, ordered)?;
            tx.commit()?;
            Ok(Some(written))
        })
        .await
    }

    async fn create_library_entry(
        &self,
        entry: NewLibraryEntry,
        modules: Vec<NewLibraryEntryModule>,
        mut audit: NewAuditEvent,
    ) -> Result<LibraryEntry, StorageError> {
        self.with_conn(move |conn| {
            let id = Uuid::new_v4();
            let now = Utc::now();
            let now_str = now.to_rfc3339();
            // The entry, its modules and its origin event commit together.
            let tx = conn.unchecked_transaction()?;
            tx.execute(
                "INSERT INTO surface_library_entries
                     (id, name, description, tags, origin_topic_id, saved_from_surface_id,
                      created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
                params![
                    id.to_string(),
                    &entry.name,
                    &entry.description,
                    encode_tags(&entry.tags),
                    entry.origin_topic_id.to_string(),
                    entry.saved_from_surface_id.map(|id| id.to_string()),
                    &now_str,
                ],
            )?;
            for module in &modules {
                tx.execute(
                    "INSERT INTO surface_library_entry_modules
                         (id, entry_id, kind, position, source, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
                    params![
                        Uuid::new_v4().to_string(),
                        id.to_string(),
                        &module.kind,
                        module.position,
                        &module.source,
                        &now_str,
                    ],
                )?;
            }
            audit.entity_id = id.to_string();
            super::audit::insert_audit_event(&tx, audit)?;
            tx.commit()?;
            Ok(LibraryEntry {
                id,
                name: entry.name,
                description: entry.description,
                tags: entry.tags,
                origin_topic_id: entry.origin_topic_id,
                saved_from_surface_id: entry.saved_from_surface_id,
                created_at: now,
                updated_at: now,
            })
        })
        .await
    }

    async fn get_library_entry(&self, id: Uuid) -> Result<Option<LibraryEntry>, StorageError> {
        self.with_conn(move |conn| {
            conn.query_row(
                &format!("SELECT {ENTRY_COLUMNS} FROM surface_library_entries WHERE id = ?1"),
                params![id.to_string()],
                row_to_entry,
            )
            .optional()
        })
        .await
    }

    async fn list_library_entries(&self) -> Result<Vec<LibraryEntry>, StorageError> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {ENTRY_COLUMNS} FROM surface_library_entries
                 ORDER BY created_at DESC, id DESC"
            ))?;
            let rows = stmt.query_map([], row_to_entry)?;
            rows.collect()
        })
        .await
    }

    async fn list_library_entry_modules(
        &self,
        entry_id: Uuid,
    ) -> Result<Vec<LibraryEntryModule>, StorageError> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {ENTRY_MODULE_COLUMNS} FROM surface_library_entry_modules
                 WHERE entry_id = ?1 ORDER BY position ASC, created_at ASC"
            ))?;
            let rows = stmt.query_map(params![entry_id.to_string()], row_to_entry_module)?;
            rows.collect()
        })
        .await
    }

    async fn delete_library_entry(&self, id: Uuid) -> Result<bool, StorageError> {
        self.with_conn(move |conn| {
            let affected = conn.execute(
                "DELETE FROM surface_library_entries WHERE id = ?1",
                params![id.to_string()],
            )?;
            Ok(affected > 0)
        })
        .await
    }

    async fn create_surface_from_library(
        &self,
        surface: NewSurface,
        entry_id: Uuid,
        seeding: LibrarySeeding,
    ) -> Result<Surface, StorageError> {
        self.with_conn(move |conn| {
            let id = Uuid::new_v4();
            let now = Utc::now();
            let now_str = now.to_rfc3339();
            // One transaction: a surface whose seeded modules are missing is
            // never observable, and a failed copy leaves no surface behind.
            let tx = conn.unchecked_transaction()?;
            tx.execute(
                "INSERT INTO surfaces
                     (id, session_id, title, created_at, updated_at, library_entry_id, seeded_at, seeded_from_name)
                 VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?4, (SELECT name FROM surface_library_entries WHERE id = ?5))",
                params![
                    id.to_string(),
                    surface.session_id.to_string(),
                    &surface.title,
                    &now_str,
                    entry_id.to_string(),
                ],
            )?;
            let seeds: Vec<LibraryEntryModule> = {
                let mut stmt = tx.prepare(&format!(
                    "SELECT {ENTRY_MODULE_COLUMNS} FROM surface_library_entry_modules
                     WHERE entry_id = ?1 ORDER BY position ASC, created_at ASC"
                ))?;
                let rows = stmt.query_map(params![entry_id.to_string()], row_to_entry_module)?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };
            // The seeds are copied into rows of the surface's own: new ids, new
            // timestamps. Nothing links back but the lineage column. Outside
            // the entry's origin scope the shape comes over and the content
            // starts empty — another scope's data never arrives by accident.
            for (position, seed) in seeds.into_iter().enumerate() {
                let source = match seeding {
                    LibrarySeeding::WithContent => seed.source,
                    LibrarySeeding::ShapeOnly => String::new(),
                };
                tx.execute(
                    "INSERT INTO surface_modules
                         (id, surface_id, kind, position, source, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
                    params![
                        Uuid::new_v4().to_string(),
                        id.to_string(),
                        &seed.kind,
                        position as i32,
                        source,
                        &now_str,
                    ],
                )?;
            }
            let seeded_from_name: String = tx.query_row(
                "SELECT seeded_from_name FROM surfaces WHERE id = ?1",
                params![id.to_string()],
                |row| row.get(0),
            )?;
            tx.commit()?;
            Ok(Surface {
                id,
                session_id: surface.session_id,
                title: surface.title,
                created_at: now,
                updated_at: now,
                library_entry_id: Some(entry_id),
                seeded_from_name: Some(seeded_from_name),
                seeded_at: Some(now),
            })
        })
        .await
    }

    async fn list_topic_default_entries(
        &self,
        topic_id: Uuid,
    ) -> Result<Vec<LibraryEntry>, StorageError> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {ENTRY_COLUMNS} FROM surface_library_entries
                 JOIN topic_default_surfaces
                   ON topic_default_surfaces.entry_id = surface_library_entries.id
                 WHERE topic_default_surfaces.topic_id = ?1
                 ORDER BY topic_default_surfaces.designated_at ASC, surface_library_entries.id ASC"
            ))?;
            let rows = stmt.query_map(params![topic_id.to_string()], row_to_entry)?;
            rows.collect()
        })
        .await
    }

    async fn add_topic_default_entry(
        &self,
        topic_id: Uuid,
        entry_id: Uuid,
    ) -> Result<(), StorageError> {
        self.with_conn(move |conn| {
            conn.execute(
                "INSERT OR IGNORE INTO topic_default_surfaces (topic_id, entry_id, designated_at)
                 VALUES (?1, ?2, ?3)",
                params![
                    topic_id.to_string(),
                    entry_id.to_string(),
                    Utc::now().to_rfc3339()
                ],
            )?;
            Ok(())
        })
        .await
    }

    async fn remove_topic_default_entry(
        &self,
        topic_id: Uuid,
        entry_id: Uuid,
    ) -> Result<bool, StorageError> {
        self.with_conn(move |conn| {
            let affected = conn.execute(
                "DELETE FROM topic_default_surfaces WHERE topic_id = ?1 AND entry_id = ?2",
                params![topic_id.to_string(), entry_id.to_string()],
            )?;
            Ok(affected > 0)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use crate::model::{NewSession, NewSurface, NewSurfaceModule, NewTopic};
    use crate::store::{SessionStore, TopicStore};

    use super::*;

    fn entry_audit(session_id: Uuid) -> NewAuditEvent {
        NewAuditEvent {
            id: Uuid::new_v4().to_string(),
            session_id: Some(session_id),
            entity_kind: "surface_library_entry".into(),
            entity_id: String::new(),
            event_kind: "surface_saved_to_library".into(),
            summary: "surface saved to the library".into(),
            metadata: serde_json::json!({}),
        }
    }

    /// A topic and a session in it: the owner every surface here belongs to.
    async fn make_session() -> (SqliteStorage, Uuid, Uuid) {
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
        let session = session_in(&storage, topic.id).await;
        (storage, topic.id, session)
    }

    async fn session_in(storage: &SqliteStorage, topic_id: Uuid) -> Uuid {
        storage
            .create_session(NewSession {
                ..crate::fixtures::new_session(topic_id)
            })
            .await
            .unwrap()
            .id
    }

    async fn surface_with_modules(
        storage: &SqliteStorage,
        session_id: Uuid,
        sources: &[&str],
    ) -> Surface {
        let surface = storage
            .create_page(NewSurface {
                session_id,
                title: "Planning".into(),
            })
            .await
            .unwrap();
        for (position, source) in sources.iter().enumerate() {
            storage
                .create_surface_module(NewSurfaceModule {
                    surface_id: surface.id,
                    kind: "document".into(),
                    position: position as i32,
                    source: (*source).into(),
                })
                .await
                .unwrap();
        }
        surface
    }

    /// Save a surface the way the gateway does: its modules copied in order.
    async fn save(
        storage: &SqliteStorage,
        surface: &Surface,
        origin_topic_id: Uuid,
        tags: &[&str],
    ) -> LibraryEntry {
        let modules = storage
            .list_surface_modules(surface.id)
            .await
            .unwrap()
            .into_iter()
            .map(|module| NewLibraryEntryModule {
                kind: module.kind,
                position: module.position,
                source: module.source,
            })
            .collect();
        storage
            .create_library_entry(
                NewLibraryEntry {
                    name: "Standup".into(),
                    description: "Daily standup notes — use at the start of a working day.".into(),
                    tags: tags.iter().map(|tag| (*tag).to_string()).collect(),
                    origin_topic_id,
                    saved_from_surface_id: Some(surface.id),
                },
                modules,
                entry_audit(surface.session_id),
            )
            .await
            .unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn surfaces_belong_to_their_session_and_list_in_creation_order() {
        let (storage, topic_id, session) = make_session().await;
        let other_session = session_in(&storage, topic_id).await;
        let mine = surface_with_modules(&storage, session, &["# Mine"]).await;
        surface_with_modules(&storage, other_session, &["# Theirs"]).await;
        let second = surface_with_modules(&storage, session, &[]).await;

        let listed = storage.list_surfaces_by_session(session).await.unwrap();
        assert_eq!(
            listed.iter().map(|surface| surface.id).collect::<Vec<_>>(),
            vec![mine.id, second.id],
            "a session sees its own surfaces and nothing of the other session's"
        );
        let modules = storage.list_surface_modules(mine.id).await.unwrap();
        assert_eq!(modules.len(), 1);
        assert_eq!(modules[0].source, "# Mine");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_replace_lands_against_the_revision_read_and_advances_it() {
        let (storage, _, session) = make_session().await;
        let surface = surface_with_modules(&storage, session, &["before"]).await;
        let module = storage.list_surface_modules(surface.id).await.unwrap()[0].clone();
        assert_eq!(module.revision, 0, "a new module starts at revision 0");

        let outcome = storage
            .replace_surface_module_source(module.id, module.revision, "after")
            .await
            .unwrap()
            .expect("the module exists");
        let ModuleSourceReplacement::Replaced(read) = outcome else {
            panic!("a replace against the revision read lands");
        };
        assert_eq!(read.source, "after");
        assert_eq!(read.revision, 1);
        let surface_read = storage.get_page(surface.id).await.unwrap().unwrap();
        assert!(
            surface_read.updated_at >= read.created_at,
            "surface carries the mutation's timestamp"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_replace_against_a_moved_on_revision_changes_nothing_and_returns_the_row() {
        let (storage, _, session) = make_session().await;
        let surface = surface_with_modules(&storage, session, &["draft"]).await;
        let module = storage.list_surface_modules(surface.id).await.unwrap()[0].clone();
        storage
            .replace_surface_module_source(module.id, 0, "the agent's edit")
            .await
            .unwrap();

        // An editor opened at revision 0 saves after the agent's edit landed.
        let outcome = storage
            .replace_surface_module_source(module.id, 0, "the person's edit")
            .await
            .unwrap()
            .expect("the module exists");
        let ModuleSourceReplacement::Stale(current) = outcome else {
            panic!("a replace against a moved-on revision is refused");
        };
        assert_eq!(
            current.source, "the agent's edit",
            "the saver sees what it did not"
        );
        assert_eq!(current.revision, 1);
        let stored = storage
            .get_surface_module(module.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.source, "the agent's edit", "nothing landed");

        assert!(
            storage
                .replace_surface_module_source(Uuid::new_v4(), 0, "x")
                .await
                .unwrap()
                .is_none(),
            "a missing module is not a stale one"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn removing_a_module_takes_its_notes_and_closes_the_gap() {
        use crate::model::NewSurfaceAnnotation;
        use crate::store::SurfaceAnnotationStore;

        let (storage, _, session) = make_session().await;
        let surface = surface_with_modules(&storage, session, &["a", "b", "c"]).await;
        let modules = storage.list_surface_modules(surface.id).await.unwrap();
        let middle = modules[1].clone();
        storage
            .create_surface_annotation(NewSurfaceAnnotation {
                surface_id: surface.id,
                module_id: middle.id,
                body: "about b".into(),
                tags: Vec::new(),
                author: "user".into(),
            })
            .await
            .unwrap();

        let removed = storage
            .remove_surface_module(middle.id)
            .await
            .unwrap()
            .expect("the module exists");
        assert_eq!(removed.module.source, "b");
        assert_eq!(removed.annotations.len(), 1);
        assert_eq!(removed.annotations[0].body, "about b");

        let remaining = storage.list_surface_modules(surface.id).await.unwrap();
        assert_eq!(
            remaining
                .iter()
                .map(|module| (module.source.as_str(), module.position))
                .collect::<Vec<_>>(),
            vec![("a", 0), ("c", 1)],
            "positions close up behind the removed module"
        );
        assert!(
            storage
                .list_surface_annotations(surface.id)
                .await
                .unwrap()
                .is_empty(),
            "the notes went with their module"
        );
        assert!(
            storage
                .remove_surface_module(middle.id)
                .await
                .unwrap()
                .is_none(),
            "a second removal finds nothing"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn moving_a_module_renumbers_its_surface_and_clamps_the_target() {
        let (storage, _, session) = make_session().await;
        let surface = surface_with_modules(&storage, session, &["a", "b", "c"]).await;
        let modules = storage.list_surface_modules(surface.id).await.unwrap();
        let (a, c) = (modules[0].clone(), modules[2].clone());

        let order = storage
            .move_surface_module(c.id, 0)
            .await
            .unwrap()
            .expect("the module exists");
        assert_eq!(
            order
                .iter()
                .map(|m| (m.source.as_str(), m.position))
                .collect::<Vec<_>>(),
            vec![("c", 0), ("a", 1), ("b", 2)]
        );

        let order = storage
            .move_surface_module(a.id, 99)
            .await
            .unwrap()
            .expect("the module exists");
        assert_eq!(
            order.iter().map(|m| m.source.as_str()).collect::<Vec<_>>(),
            vec!["c", "b", "a"],
            "a position past the end is the end"
        );
        let stored = storage.list_surface_modules(surface.id).await.unwrap();
        assert_eq!(
            stored.iter().map(|m| m.position).collect::<Vec<_>>(),
            vec![0, 1, 2],
            "the stored positions are the returned order"
        );
        assert!(
            storage
                .move_surface_module(Uuid::new_v4(), 0)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_saved_surface_keeps_its_modules_in_order_and_its_catalogue_fields() {
        let (storage, topic_id, session) = make_session().await;
        let surface =
            surface_with_modules(&storage, session, &["## Yesterday", "## Blockers"]).await;

        let entry = save(&storage, &surface, topic_id, &["standup", "ops"]).await;

        let fetched = storage.get_library_entry(entry.id).await.unwrap().unwrap();
        assert_eq!(fetched.name, "Standup");
        assert_eq!(fetched.tags, vec!["standup", "ops"]);
        assert_eq!(fetched.origin_topic_id, topic_id);
        assert_eq!(fetched.saved_from_surface_id, Some(surface.id));
        let modules = storage.list_library_entry_modules(entry.id).await.unwrap();
        assert_eq!(
            modules
                .iter()
                .map(|m| m.source.as_str())
                .collect::<Vec<_>>(),
            vec!["## Yesterday", "## Blockers"]
        );
        assert_eq!(storage.list_library_entries().await.unwrap().len(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn saving_rolls_back_when_a_later_module_insert_fails() {
        let (storage, topic_id, session) = make_session().await;
        storage
            .with_conn(|conn| {
                conn.execute_batch(
                    "CREATE TRIGGER reject_entry_module
                     BEFORE INSERT ON surface_library_entry_modules
                     WHEN NEW.source = 'reject this module'
                     BEGIN SELECT RAISE(ABORT, 'forced entry module failure'); END;",
                )
            })
            .await
            .unwrap();

        let result = storage
            .create_library_entry(
                NewLibraryEntry {
                    name: "Broken".into(),
                    description: "never lands".into(),
                    tags: Vec::new(),
                    origin_topic_id: topic_id,
                    saved_from_surface_id: None,
                },
                vec![
                    NewLibraryEntryModule {
                        kind: "document".into(),
                        position: 0,
                        source: "this module lands first".into(),
                    },
                    NewLibraryEntryModule {
                        kind: "document".into(),
                        position: 1,
                        source: "reject this module".into(),
                    },
                ],
                entry_audit(session),
            )
            .await;

        assert!(result.is_err(), "the trigger rejects the later module");
        assert!(storage.list_library_entries().await.unwrap().is_empty());
        let module_count: i64 = storage
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM surface_library_entry_modules",
                    [],
                    |row| row.get(0),
                )
            })
            .await
            .unwrap();
        assert_eq!(module_count, 0, "earlier entry module rows roll back");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn adding_an_entry_seeds_content_in_its_origin_scope_and_shape_elsewhere() {
        let (storage, topic_id, session) = make_session().await;
        let surface =
            surface_with_modules(&storage, session, &["## Yesterday", "## Blockers"]).await;
        let entry = save(&storage, &surface, topic_id, &[]).await;
        let same_scope = session_in(&storage, topic_id).await;
        let other_topic = TopicStore::create(
            &storage,
            NewTopic {
                name: "q".into(),
                slug: "q".into(),
            },
        )
        .await
        .unwrap();
        let other_scope = session_in(&storage, other_topic.id).await;

        let with_content = storage
            .create_surface_from_library(
                NewSurface {
                    session_id: same_scope,
                    title: "Monday".into(),
                },
                entry.id,
                LibrarySeeding::WithContent,
            )
            .await
            .unwrap();
        let shape_only = storage
            .create_surface_from_library(
                NewSurface {
                    session_id: other_scope,
                    title: "Monday elsewhere".into(),
                },
                entry.id,
                LibrarySeeding::ShapeOnly,
            )
            .await
            .unwrap();

        assert_eq!(with_content.library_entry_id, Some(entry.id));
        assert!(with_content.seeded_at.is_some());
        async fn sources(storage: &SqliteStorage, surface_id: Uuid) -> Vec<(String, String)> {
            storage
                .list_surface_modules(surface_id)
                .await
                .unwrap()
                .into_iter()
                .map(|m| (m.kind, m.source))
                .collect()
        }
        assert_eq!(
            sources(&storage, with_content.id).await,
            vec![
                ("document".to_string(), "## Yesterday".to_string()),
                ("document".to_string(), "## Blockers".to_string())
            ],
            "within the origin scope the saved content comes along"
        );
        assert_eq!(
            sources(&storage, shape_only.id).await,
            vec![
                ("document".to_string(), String::new()),
                ("document".to_string(), String::new())
            ],
            "outside it the shape arrives and the content is the destination's to fill"
        );
        // The copies are the sessions' own: editing one moves neither the
        // entry nor the other.
        let module = storage.list_surface_modules(with_content.id).await.unwrap()[0].clone();
        storage
            .replace_surface_module_source(module.id, module.revision, "## Today")
            .await
            .unwrap();
        assert_eq!(
            storage.list_library_entry_modules(entry.id).await.unwrap()[0].source,
            "## Yesterday"
        );
        assert_eq!(sources(&storage, shape_only.id).await[0].1, "");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn adding_rolls_back_when_a_later_seed_insert_fails() {
        let (storage, topic_id, session) = make_session().await;
        let surface = surface_with_modules(&storage, session, &["fine", "reject this seed"]).await;
        let entry = save(&storage, &surface, topic_id, &[]).await;
        storage
            .with_conn(|conn| {
                conn.execute_batch(
                    "CREATE TRIGGER reject_seed
                     BEFORE INSERT ON surface_modules
                     WHEN NEW.source = 'reject this seed'
                     BEGIN SELECT RAISE(ABORT, 'forced seed failure'); END;",
                )
            })
            .await
            .unwrap();
        let target = session_in(&storage, topic_id).await;

        let result = storage
            .create_surface_from_library(
                NewSurface {
                    session_id: target,
                    title: "Broken".into(),
                },
                entry.id,
                LibrarySeeding::WithContent,
            )
            .await;

        assert!(result.is_err());
        assert!(
            storage
                .list_surfaces_by_session(target)
                .await
                .unwrap()
                .is_empty(),
            "the surface head rolls back with its seeds"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_topic_designates_defaults_in_order_and_withdraws_them() {
        let (storage, topic_id, session) = make_session().await;
        let first = save(
            &storage,
            &surface_with_modules(&storage, session, &["a"]).await,
            topic_id,
            &[],
        )
        .await;
        let second = save(
            &storage,
            &surface_with_modules(&storage, session, &["b"]).await,
            topic_id,
            &[],
        )
        .await;

        storage
            .add_topic_default_entry(topic_id, second.id)
            .await
            .unwrap();
        storage
            .add_topic_default_entry(topic_id, first.id)
            .await
            .unwrap();
        // Designating twice is not two defaults.
        storage
            .add_topic_default_entry(topic_id, first.id)
            .await
            .unwrap();

        let defaults = storage.list_topic_default_entries(topic_id).await.unwrap();
        assert_eq!(
            defaults.iter().map(|entry| entry.id).collect::<Vec<_>>(),
            vec![second.id, first.id],
            "defaults keep the order they were designated in"
        );
        assert!(
            storage
                .remove_topic_default_entry(topic_id, second.id)
                .await
                .unwrap()
        );
        assert!(
            !storage
                .remove_topic_default_entry(topic_id, second.id)
                .await
                .unwrap()
        );
        let defaults = storage.list_topic_default_entries(topic_id).await.unwrap();
        assert_eq!(defaults.len(), 1);
        assert_eq!(defaults[0].id, first.id);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn deleting_an_entry_leaves_seeded_surfaces_with_their_rows() {
        let (storage, topic_id, session) = make_session().await;
        let surface = surface_with_modules(&storage, session, &["kept"]).await;
        let entry = save(&storage, &surface, topic_id, &[]).await;
        storage
            .add_topic_default_entry(topic_id, entry.id)
            .await
            .unwrap();
        let seeded = storage
            .create_surface_from_library(
                NewSurface {
                    session_id: session,
                    title: "Seeded".into(),
                },
                entry.id,
                LibrarySeeding::WithContent,
            )
            .await
            .unwrap();

        assert!(storage.delete_library_entry(entry.id).await.unwrap());
        assert!(!storage.delete_library_entry(entry.id).await.unwrap());

        let survivor = storage.get_page(seeded.id).await.unwrap().unwrap();
        assert_eq!(
            survivor.library_entry_id, None,
            "the origin link clears; the surface stays"
        );
        assert_eq!(
            storage.list_surface_modules(seeded.id).await.unwrap()[0].source,
            "kept"
        );
        assert!(
            storage
                .list_topic_default_entries(topic_id)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn deleting_an_origin_topic_keeps_entry_modules_and_other_topics_designation() {
        let (storage, origin_topic, origin_session) = make_session().await;
        let destination = TopicStore::create(
            &storage,
            NewTopic {
                name: "destination".into(),
                slug: "destination".into(),
            },
        )
        .await
        .unwrap();
        let surface = surface_with_modules(&storage, origin_session, &["saved content"]).await;
        let entry = save(&storage, &surface, origin_topic, &[]).await;
        storage
            .add_topic_default_entry(destination.id, entry.id)
            .await
            .unwrap();

        assert!(TopicStore::delete(&storage, origin_topic).await.unwrap());
        let survivor = storage
            .get_library_entry(entry.id)
            .await
            .unwrap()
            .expect("entry remains");
        assert_eq!(
            survivor.origin_topic_id, origin_topic,
            "the vanished origin remains distinct from general"
        );
        assert_eq!(
            storage.list_library_entry_modules(entry.id).await.unwrap()[0].source,
            "saved content"
        );
        assert_eq!(
            storage
                .list_topic_default_entries(destination.id)
                .await
                .unwrap()[0]
                .id,
            entry.id
        );
    }
}
