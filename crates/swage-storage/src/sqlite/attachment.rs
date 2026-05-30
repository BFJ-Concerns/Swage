use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::Connection;
use rusqlite::params;
use uuid::Uuid;

use swage_core::StorageError;

use crate::model::{
    Attachment, AttachmentKind, AttachmentScopeKind, AttachmentStatus, BackgroundJob,
    BackgroundJobKind, BackgroundJobState, NewAttachment, NewBackgroundJob,
};
use crate::sqlite::SqliteStorage;
use crate::store::{AttachmentStore, BackgroundJobStore};

fn parse_uuid(s: &str) -> rusqlite::Result<Uuid> {
    s.parse::<Uuid>().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
    })
}

fn parse_dt(s: &str) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })
}

fn row_to_attachment(row: &rusqlite::Row<'_>) -> rusqlite::Result<Attachment> {
    let id: String = row.get(0)?;
    let session_id: Option<String> = row.get(2)?;
    let bench_id: Option<String> = row.get(3)?;
    let created_at: String = row.get(10)?;
    let updated_at: String = row.get(11)?;
    Ok(Attachment {
        id: parse_uuid(&id)?,
        scope_kind: row
            .get::<_, String>(1)?
            .parse::<AttachmentScopeKind>()
            .map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    1,
                    rusqlite::types::Type::Text,
                    Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, error)),
                )
            })?,
        session_id: session_id.as_deref().map(parse_uuid).transpose()?,
        bench_id: bench_id.as_deref().map(parse_uuid).transpose()?,
        kind: row
            .get::<_, String>(4)?
            .parse::<AttachmentKind>()
            .map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    4,
                    rusqlite::types::Type::Text,
                    Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, error)),
                )
            })?,
        status: row
            .get::<_, String>(5)?
            .parse::<AttachmentStatus>()
            .map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    5,
                    rusqlite::types::Type::Text,
                    Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, error)),
                )
            })?,
        filename: row.get(6)?,
        mime_type: row.get(7)?,
        bytes: row.get(8)?,
        byte_count: row.get(9)?,
        created_at: parse_dt(&created_at)?,
        updated_at: parse_dt(&updated_at)?,
    })
}

fn row_to_background_job(row: &rusqlite::Row<'_>) -> rusqlite::Result<BackgroundJob> {
    let id: String = row.get(0)?;
    let attachment_id: Option<String> = row.get(3)?;
    let payload_json: String = row.get(4)?;
    let result_json: Option<String> = row.get(5)?;
    let created_at: String = row.get(8)?;
    let updated_at: String = row.get(9)?;
    let started_at: Option<String> = row.get(10)?;
    let completed_at: Option<String> = row.get(11)?;
    Ok(BackgroundJob {
        id: parse_uuid(&id)?,
        kind: row
            .get::<_, String>(1)?
            .parse::<BackgroundJobKind>()
            .map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    1,
                    rusqlite::types::Type::Text,
                    Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, error)),
                )
            })?,
        state: row
            .get::<_, String>(2)?
            .parse::<BackgroundJobState>()
            .map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    2,
                    rusqlite::types::Type::Text,
                    Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, error)),
                )
            })?,
        attachment_id: attachment_id.as_deref().map(parse_uuid).transpose()?,
        payload: serde_json::from_str(&payload_json).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                4,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        result: result_json
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    5,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?,
        error_message: row.get(6)?,
        attempt_count: row.get(7)?,
        created_at: parse_dt(&created_at)?,
        updated_at: parse_dt(&updated_at)?,
        started_at: started_at.as_deref().map(parse_dt).transpose()?,
        completed_at: completed_at.as_deref().map(parse_dt).transpose()?,
    })
}

pub fn migrate_attachment_ingested_markdown_dedup(conn: &Connection) -> Result<(), StorageError> {
    conn.execute_batch("BEGIN IMMEDIATE")
        .map_err(|error| StorageError::MigrationFailed(error.to_string()))?;
    let result = (|| -> Result<(), StorageError> {
        let mut stmt = conn
            .prepare(
                "SELECT
                    a.id,
                    a.scope_kind,
                    a.session_id,
                    a.bench_id,
                    a.filename,
                    a.ingested_markdown,
                    a.created_at,
                    a.updated_at
                 FROM attachments a
                 LEFT JOIN materials m ON m.source_attachment_id = a.id
                 WHERE a.ingested_markdown IS NOT NULL
                   AND m.id IS NULL",
            )
            .map_err(|error| StorageError::MigrationFailed(error.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                ))
            })
            .map_err(|error| StorageError::MigrationFailed(error.to_string()))?;

        for row in rows {
            let (
                attachment_id,
                scope_kind,
                session_id,
                bench_id,
                filename,
                content_text,
                created_at,
                updated_at,
            ) = row.map_err(|error| StorageError::MigrationFailed(error.to_string()))?;
            conn.execute(
                "INSERT INTO materials (
                    id, scope_kind, session_id, bench_id, source_kind, source_attachment_id,
                    status, title, relative_path, mime_type, content_text, created_at, updated_at, archived_at
                ) VALUES (?1, ?2, ?3, ?4, 'attachment_ingestion', ?5, 'active', ?6, ?7, 'text/markdown', ?8, ?9, ?10, NULL)",
                params![
                    Uuid::new_v4().to_string(),
                    scope_kind,
                    session_id,
                    bench_id,
                    attachment_id,
                    filename,
                    format!("attachments/{attachment_id}-attachment.md"),
                    content_text,
                    created_at,
                    updated_at,
                ],
            )
            .map_err(|error| StorageError::MigrationFailed(error.to_string()))?;
        }

        conn.execute(
            "UPDATE attachments SET ingested_markdown = NULL WHERE ingested_markdown IS NOT NULL",
            [],
        )
        .map_err(|error| StorageError::MigrationFailed(error.to_string()))?;
        Ok(())
    })();

    match result {
        Ok(()) => conn
            .execute_batch("COMMIT")
            .map_err(|error| StorageError::MigrationFailed(error.to_string())),
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

#[async_trait]
impl AttachmentStore for SqliteStorage {
    async fn queue_attachment_ingestion(
        &self,
        attachment: NewAttachment,
        job: NewBackgroundJob,
    ) -> Result<(Attachment, BackgroundJob), StorageError> {
        self.with_conn(move |conn| {
            let now = Utc::now();
            let now_str = now.to_rfc3339();
            let payload_json = serde_json::to_string(&job.payload)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
            conn.execute_batch("BEGIN IMMEDIATE")?;
            let result = (|| -> Result<(Attachment, BackgroundJob), rusqlite::Error> {
                conn.execute(
                    "INSERT INTO attachments (
                        id, scope_kind, session_id, bench_id, kind, status, filename,
                        mime_type, content_bytes, byte_count, created_at, updated_at
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)",
                    params![
                        attachment.id.to_string(),
                        attachment.scope_kind.to_string(),
                        attachment.session_id.map(|id| id.to_string()),
                        attachment.bench_id.map(|id| id.to_string()),
                        attachment.kind.to_string(),
                        AttachmentStatus::Pending.to_string(),
                        attachment.filename,
                        attachment.mime_type,
                        attachment.bytes,
                        i64::try_from(attachment.bytes.len()).unwrap_or(i64::MAX),
                        now_str,
                    ],
                )?;
                conn.execute(
                    "INSERT INTO background_jobs (
                        id, kind, state, attachment_id, payload_json, result_json, error_message,
                        attempt_count, created_at, updated_at, started_at, completed_at
                    ) VALUES (?1, ?2, ?3, ?4, ?5, NULL, NULL, 0, ?6, ?6, NULL, NULL)",
                    params![
                        job.id.to_string(),
                        job.kind.to_string(),
                        BackgroundJobState::Queued.to_string(),
                        job.attachment_id.map(|id| id.to_string()),
                        payload_json,
                        now_str,
                    ],
                )?;
                let attachment_record = conn.query_row(
                    "SELECT id, scope_kind, session_id, bench_id, kind, status, filename,
                            mime_type, content_bytes, byte_count, created_at, updated_at
                     FROM attachments WHERE id = ?1",
                    params![attachment.id.to_string()],
                    row_to_attachment,
                )?;
                let job_record = conn.query_row(
                    "SELECT id, kind, state, attachment_id, payload_json, result_json, error_message,
                            attempt_count, created_at, updated_at, started_at, completed_at
                     FROM background_jobs WHERE id = ?1",
                    params![job.id.to_string()],
                    row_to_background_job,
                )?;
                Ok((attachment_record, job_record))
            })();
            match result {
                Ok(records) => {
                    conn.execute_batch("COMMIT")?;
                    Ok(records)
                }
                Err(error) => {
                    let _ = conn.execute_batch("ROLLBACK");
                    Err(error)
                }
            }
        })
        .await
    }

    async fn get_attachment(&self, id: Uuid) -> Result<Option<Attachment>, StorageError> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, scope_kind, session_id, bench_id, kind, status, filename,
                        mime_type, content_bytes, byte_count, created_at, updated_at
                 FROM attachments WHERE id = ?1",
            )?;
            let mut rows = stmt.query(params![id.to_string()])?;
            match rows.next()? {
                Some(row) => row_to_attachment(row).map(Some),
                None => Ok(None),
            }
        })
        .await
    }

    async fn list_attachments_by_session(
        &self,
        session_id: Uuid,
    ) -> Result<Vec<Attachment>, StorageError> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, scope_kind, session_id, bench_id, kind, status, filename,
                        mime_type, content_bytes, byte_count, created_at, updated_at
                 FROM attachments WHERE session_id = ?1 ORDER BY created_at ASC, id ASC",
            )?;
            stmt.query_map(params![session_id.to_string()], row_to_attachment)?
                .collect()
        })
        .await
    }

    async fn list_attachments_by_bench(
        &self,
        bench_id: Uuid,
    ) -> Result<Vec<Attachment>, StorageError> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, scope_kind, session_id, bench_id, kind, status, filename,
                        mime_type, content_bytes, byte_count, created_at, updated_at
                 FROM attachments WHERE bench_id = ?1 ORDER BY created_at ASC, id ASC",
            )?;
            stmt.query_map(params![bench_id.to_string()], row_to_attachment)?
                .collect()
        })
        .await
    }

    async fn update_attachment_ingestion(
        &self,
        id: Uuid,
        status: AttachmentStatus,
    ) -> Result<Attachment, StorageError> {
        self.with_conn(move |conn| {
            let now = Utc::now().to_rfc3339();
            conn.execute(
                "UPDATE attachments
                 SET status = ?2, updated_at = ?3
                 WHERE id = ?1",
                params![id.to_string(), status.to_string(), now],
            )?;
            conn.query_row(
                "SELECT id, scope_kind, session_id, bench_id, kind, status, filename,
                        mime_type, content_bytes, byte_count, created_at, updated_at
                 FROM attachments WHERE id = ?1",
                params![id.to_string()],
                row_to_attachment,
            )
        })
        .await
    }
}

#[async_trait]
impl BackgroundJobStore for SqliteStorage {
    async fn get_background_job(&self, id: Uuid) -> Result<Option<BackgroundJob>, StorageError> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, kind, state, attachment_id, payload_json, result_json, error_message,
                        attempt_count, created_at, updated_at, started_at, completed_at
                 FROM background_jobs WHERE id = ?1",
            )?;
            let mut rows = stmt.query(params![id.to_string()])?;
            match rows.next()? {
                Some(row) => row_to_background_job(row).map(Some),
                None => Ok(None),
            }
        })
        .await
    }

    async fn list_runnable_background_jobs(&self) -> Result<Vec<BackgroundJob>, StorageError> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, kind, state, attachment_id, payload_json, result_json, error_message,
                        attempt_count, created_at, updated_at, started_at, completed_at
                 FROM background_jobs
                 WHERE state IN ('queued', 'retried')
                 ORDER BY created_at ASC, id ASC",
            )?;
            stmt.query_map([], row_to_background_job)?.collect()
        })
        .await
    }

    async fn mark_background_job_running(&self, id: Uuid) -> Result<BackgroundJob, StorageError> {
        self.with_conn(move |conn| {
            let now = Utc::now().to_rfc3339();
            conn.execute(
                "UPDATE background_jobs
                 SET state = 'running',
                     attempt_count = attempt_count + 1,
                     started_at = ?2,
                     updated_at = ?2,
                     completed_at = NULL
                 WHERE id = ?1",
                params![id.to_string(), now],
            )?;
            conn.query_row(
                "SELECT id, kind, state, attachment_id, payload_json, result_json, error_message,
                        attempt_count, created_at, updated_at, started_at, completed_at
                 FROM background_jobs WHERE id = ?1",
                params![id.to_string()],
                row_to_background_job,
            )
        })
        .await
    }

    async fn complete_background_job(
        &self,
        id: Uuid,
        result: Option<&serde_json::Value>,
    ) -> Result<BackgroundJob, StorageError> {
        self.with_conn(move |conn| {
            let now = Utc::now().to_rfc3339();
            let result_json = result
                .map(serde_json::to_string)
                .transpose()
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
            conn.execute(
                "UPDATE background_jobs
                 SET state = 'completed', result_json = ?2, error_message = NULL,
                     updated_at = ?3, completed_at = ?3
                 WHERE id = ?1",
                params![id.to_string(), result_json, now],
            )?;
            conn.query_row(
                "SELECT id, kind, state, attachment_id, payload_json, result_json, error_message,
                        attempt_count, created_at, updated_at, started_at, completed_at
                 FROM background_jobs WHERE id = ?1",
                params![id.to_string()],
                row_to_background_job,
            )
        })
        .await
    }

    async fn fail_background_job(
        &self,
        id: Uuid,
        error_message: &str,
    ) -> Result<BackgroundJob, StorageError> {
        self.with_conn(move |conn| {
            let now = Utc::now().to_rfc3339();
            conn.execute(
                "UPDATE background_jobs
                 SET state = 'failed', error_message = ?2, updated_at = ?3, completed_at = ?3
                 WHERE id = ?1",
                params![id.to_string(), error_message, now],
            )?;
            conn.query_row(
                "SELECT id, kind, state, attachment_id, payload_json, result_json, error_message,
                        attempt_count, created_at, updated_at, started_at, completed_at
                 FROM background_jobs WHERE id = ?1",
                params![id.to_string()],
                row_to_background_job,
            )
        })
        .await
    }

    async fn retry_background_job(&self, id: Uuid) -> Result<BackgroundJob, StorageError> {
        self.with_conn(move |conn| {
            let now = Utc::now().to_rfc3339();
            conn.execute(
                "UPDATE background_jobs
                 SET state = 'retried', error_message = NULL, result_json = NULL,
                     updated_at = ?2, started_at = NULL, completed_at = NULL
                 WHERE id = ?1",
                params![id.to_string(), now],
            )?;
            conn.query_row(
                "SELECT id, kind, state, attachment_id, payload_json, result_json, error_message,
                        attempt_count, created_at, updated_at, started_at, completed_at
                 FROM background_jobs WHERE id = ?1",
                params![id.to_string()],
                row_to_background_job,
            )
        })
        .await
    }

    async fn list_background_jobs_by_attachment(
        &self,
        attachment_id: Uuid,
    ) -> Result<Vec<BackgroundJob>, StorageError> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, kind, state, attachment_id, payload_json, result_json, error_message,
                        attempt_count, created_at, updated_at, started_at, completed_at
                 FROM background_jobs
                 WHERE attachment_id = ?1
                 ORDER BY created_at ASC, id ASC",
            )?;
            stmt.query_map(params![attachment_id.to_string()], row_to_background_job)?
                .collect()
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;
    use serde_json::json;

    use super::*;
    use crate::model::{MaterialSourceKind, NewMaterial};
    use crate::store::{BenchStore, MaterialStore};

    #[tokio::test(flavor = "multi_thread")]
    async fn attachment_round_trips_and_background_job_states_transition() {
        let storage = SqliteStorage::in_memory().unwrap();
        let attachment_id = Uuid::new_v4();
        let job_id = Uuid::new_v4();
        let (attachment, job) = storage
            .queue_attachment_ingestion(
                NewAttachment {
                    id: attachment_id,
                    scope_kind: AttachmentScopeKind::Session,
                    session_id: None,
                    bench_id: None,
                    kind: AttachmentKind::Document,
                    filename: "note.txt".into(),
                    mime_type: "text/plain".into(),
                    bytes: b"hello".to_vec(),
                },
                NewBackgroundJob {
                    id: job_id,
                    kind: BackgroundJobKind::AttachmentIngestion,
                    attachment_id: Some(attachment_id),
                    payload: json!({"attachment_id": attachment_id}),
                },
            )
            .await
            .unwrap();
        assert_eq!(attachment.status, AttachmentStatus::Pending);
        assert_eq!(job.state, BackgroundJobState::Queued);

        let running = storage.mark_background_job_running(job_id).await.unwrap();
        assert_eq!(running.state, BackgroundJobState::Running);

        let completed = storage
            .complete_background_job(job_id, Some(&json!({"markdown": "hello"})))
            .await
            .unwrap();
        assert_eq!(completed.state, BackgroundJobState::Completed);

        let updated = storage
            .update_attachment_ingestion(attachment_id, AttachmentStatus::Ready)
            .await
            .unwrap();
        assert_eq!(updated.status, AttachmentStatus::Ready);

        let second_attachment_id = Uuid::new_v4();
        let second_job_id = Uuid::new_v4();
        storage
            .queue_attachment_ingestion(
                NewAttachment {
                    id: second_attachment_id,
                    scope_kind: AttachmentScopeKind::Bench,
                    session_id: None,
                    bench_id: None,
                    kind: AttachmentKind::Document,
                    filename: "bad.bin".into(),
                    mime_type: "text/plain".into(),
                    bytes: vec![0xff],
                },
                NewBackgroundJob {
                    id: second_job_id,
                    kind: BackgroundJobKind::AttachmentIngestion,
                    attachment_id: Some(second_attachment_id),
                    payload: json!({"attachment_id": second_attachment_id}),
                },
            )
            .await
            .unwrap();
        storage
            .mark_background_job_running(second_job_id)
            .await
            .unwrap();
        let failed = storage
            .fail_background_job(second_job_id, "not utf-8")
            .await
            .unwrap();
        assert_eq!(failed.state, BackgroundJobState::Failed);
        let retried = storage.retry_background_job(second_job_id).await.unwrap();
        assert_eq!(retried.state, BackgroundJobState::Retried);
    }

    #[test]
    fn attachment_markdown_migration_moves_legacy_content_into_materials() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "
            CREATE TABLE attachments (
                id TEXT NOT NULL PRIMARY KEY,
                scope_kind TEXT NOT NULL,
                session_id TEXT,
                bench_id TEXT,
                kind TEXT NOT NULL,
                status TEXT NOT NULL,
                filename TEXT NOT NULL,
                mime_type TEXT NOT NULL,
                content_bytes BLOB NOT NULL,
                byte_count INTEGER NOT NULL,
                ingested_markdown TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE materials (
                id TEXT NOT NULL PRIMARY KEY,
                scope_kind TEXT NOT NULL,
                session_id TEXT,
                bench_id TEXT,
                source_kind TEXT NOT NULL,
                source_attachment_id TEXT,
                status TEXT NOT NULL,
                title TEXT NOT NULL,
                relative_path TEXT NOT NULL,
                mime_type TEXT NOT NULL,
                content_text TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                archived_at TEXT
            );
            CREATE UNIQUE INDEX idx_materials_source_attachment_id
                ON materials(source_attachment_id)
                WHERE source_attachment_id IS NOT NULL;
            ",
        )
        .unwrap();

        let attachment_id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO attachments (
                id, scope_kind, session_id, bench_id, kind, status, filename, mime_type,
                content_bytes, byte_count, ingested_markdown, created_at, updated_at
            ) VALUES (?1, 'bench', NULL, ?2, 'document', 'ready', 'legacy.txt', 'text/plain', ?3, 6, ?4, ?5, ?5)",
            params![
                attachment_id,
                Uuid::new_v4().to_string(),
                b"legacy".to_vec(),
                "legacy markdown",
                now,
            ],
        )
        .unwrap();

        migrate_attachment_ingested_markdown_dedup(&conn).unwrap();

        let material_content: String = conn
            .query_row(
                "SELECT content_text FROM materials WHERE source_attachment_id = ?1",
                params![attachment_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(material_content, "legacy markdown");

        let attachment_markdown: Option<String> = conn
            .query_row(
                "SELECT ingested_markdown FROM attachments WHERE id = ?1",
                params![attachment_id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(attachment_markdown.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn attachment_rows_do_not_persist_duplicate_ingested_markdown() {
        let storage = SqliteStorage::in_memory().unwrap();
        let bench = storage.get_or_create_default().await.unwrap();
        let attachment_id = Uuid::new_v4();
        let (attachment, _) = storage
            .queue_attachment_ingestion(
                NewAttachment {
                    id: attachment_id,
                    scope_kind: AttachmentScopeKind::Bench,
                    session_id: None,
                    bench_id: Some(bench.id),
                    kind: AttachmentKind::Document,
                    filename: "note.md".into(),
                    mime_type: "text/markdown".into(),
                    bytes: b"# Note".to_vec(),
                },
                NewBackgroundJob {
                    id: Uuid::new_v4(),
                    kind: BackgroundJobKind::AttachmentIngestion,
                    attachment_id: Some(attachment_id),
                    payload: json!({"attachment_id": attachment_id}),
                },
            )
            .await
            .unwrap();
        storage
            .update_attachment_ingestion(attachment.id, AttachmentStatus::Ready)
            .await
            .unwrap();
        storage
            .create_material(NewMaterial {
                id: Uuid::new_v4(),
                scope_kind: attachment.scope_kind.clone(),
                session_id: attachment.session_id,
                bench_id: attachment.bench_id,
                source_kind: MaterialSourceKind::AttachmentIngestion,
                source_attachment_id: Some(attachment.id),
                title: attachment.filename.clone(),
                relative_path: "attachments/note.md".into(),
                mime_type: "text/markdown".into(),
                content_text: "# Note".into(),
            })
            .await
            .unwrap();

        let persisted_markdown: Option<String> = storage
            .with_conn(move |conn| {
                conn.query_row(
                    "SELECT ingested_markdown FROM attachments WHERE id = ?1",
                    params![attachment_id.to_string()],
                    |row| row.get(0),
                )
            })
            .await
            .unwrap();
        assert!(persisted_markdown.is_none());

        let material = storage
            .get_material_by_source_attachment(attachment.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(material.content_text, "# Note");
    }
}
