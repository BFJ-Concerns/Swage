use async_trait::async_trait;
use chrono::{DateTime, Utc};
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
    let project_id: Option<String> = row.get(3)?;
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
        project_id: project_id.as_deref().map(parse_uuid).transpose()?,
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
        ingested_markdown: row.get(12)?,
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
                        id, scope_kind, session_id, project_id, kind, status, filename,
                        mime_type, content_bytes, byte_count, created_at, updated_at
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)",
                    params![
                        attachment.id.to_string(),
                        attachment.scope_kind.to_string(),
                        attachment.session_id.map(|id| id.to_string()),
                        attachment.project_id.map(|id| id.to_string()),
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
                    "SELECT id, scope_kind, session_id, project_id, kind, status, filename,
                            mime_type, content_bytes, byte_count, created_at, updated_at, ingested_markdown
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
                "SELECT id, scope_kind, session_id, project_id, kind, status, filename,
                        mime_type, content_bytes, byte_count, created_at, updated_at, ingested_markdown
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
                "SELECT id, scope_kind, session_id, project_id, kind, status, filename,
                        mime_type, content_bytes, byte_count, created_at, updated_at, ingested_markdown
                 FROM attachments WHERE session_id = ?1 ORDER BY created_at ASC, id ASC",
            )?;
            stmt.query_map(params![session_id.to_string()], row_to_attachment)?
                .collect()
        })
        .await
    }

    async fn list_attachments_by_project(
        &self,
        project_id: Uuid,
    ) -> Result<Vec<Attachment>, StorageError> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, scope_kind, session_id, project_id, kind, status, filename,
                        mime_type, content_bytes, byte_count, created_at, updated_at, ingested_markdown
                 FROM attachments WHERE project_id = ?1 ORDER BY created_at ASC, id ASC",
            )?;
            stmt.query_map(params![project_id.to_string()], row_to_attachment)?
                .collect()
        })
        .await
    }

    async fn update_attachment_ingestion(
        &self,
        id: Uuid,
        status: AttachmentStatus,
        ingested_markdown: Option<&str>,
    ) -> Result<Attachment, StorageError> {
        self.with_conn(move |conn| {
            let now = Utc::now().to_rfc3339();
            conn.execute(
                "UPDATE attachments
                 SET status = ?2, ingested_markdown = ?3, updated_at = ?4
                 WHERE id = ?1",
                params![id.to_string(), status.to_string(), ingested_markdown, now],
            )?;
            conn.query_row(
                "SELECT id, scope_kind, session_id, project_id, kind, status, filename,
                        mime_type, content_bytes, byte_count, created_at, updated_at, ingested_markdown
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
    use serde_json::json;

    use super::*;

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
                    project_id: None,
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
            .update_attachment_ingestion(attachment_id, AttachmentStatus::Ready, Some("hello"))
            .await
            .unwrap();
        assert_eq!(updated.ingested_markdown.as_deref(), Some("hello"));

        let second_attachment_id = Uuid::new_v4();
        let second_job_id = Uuid::new_v4();
        storage
            .queue_attachment_ingestion(
                NewAttachment {
                    id: second_attachment_id,
                    scope_kind: AttachmentScopeKind::Project,
                    session_id: None,
                    project_id: None,
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
}
