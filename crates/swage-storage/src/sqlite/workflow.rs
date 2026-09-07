use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

use swage_core::StorageError;

use crate::model::{
    NewWorkflowJournalIntent, NewWorkflowRun, NewWorkflowRunEvent, NewWorkflowScript,
    WorkflowJournalRecord, WorkflowRunEventRecord, WorkflowRunRecord, WorkflowScriptRecord,
    WorkflowScriptVersion,
};
use crate::sqlite::SqliteStorage;
use crate::store::WorkflowStore;

fn uuid(value: String) -> rusqlite::Result<Uuid> {
    value.parse().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
    })
}

fn time(value: String) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(&value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })
}

fn script_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkflowScriptRecord> {
    let grant: Option<String> = row.get(4)?;
    Ok(WorkflowScriptRecord {
        id: uuid(row.get(0)?)?,
        space_id: uuid(row.get(1)?)?,
        name: row.get(2)?,
        description: row.get(3)?,
        delivery_grant: grant
            .map(|value| serde_json::from_str(&value))
            .transpose()
            .map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    4,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?,
        captured_sources_grant: row.get(5)?,
        authoring_session_id: row.get::<_, Option<String>>(6)?.map(uuid).transpose()?,
        created_at: time(row.get(7)?)?,
        updated_at: time(row.get(8)?)?,
    })
}

fn version_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkflowScriptVersion> {
    Ok(WorkflowScriptVersion {
        id: uuid(row.get(0)?)?,
        script_id: uuid(row.get(1)?)?,
        version: row.get(2)?,
        source: row.get(3)?,
        created_at: time(row.get(4)?)?,
    })
}

fn run_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkflowRunRecord> {
    let parent_run_id: Option<String> = row.get(2)?;
    let script_version_id: Option<String> = row.get(4)?;
    let script_id: Option<String> = row.get(5)?;
    let final_output: Option<String> = row.get(15)?;
    let handover: Option<String> = row.get(16)?;
    let completed_at: Option<String> = row.get(20)?;
    Ok(WorkflowRunRecord {
        id: uuid(row.get(0)?)?,
        space_id: uuid(row.get(1)?)?,
        parent_run_id: parent_run_id.map(uuid).transpose()?,
        parent_seq: row.get::<_, Option<i64>>(3)?.map(|value| value as u64),
        script_version_id: script_version_id.map(uuid).transpose()?,
        script_id: script_id.map(uuid).transpose()?,
        script_name: row.get(6)?,
        script_version: row.get::<_, Option<i64>>(7)?.map(|value| value as u32),
        carrying_skill: row.get(8)?,
        task: row.get(9)?,
        source: row.get(10)?,
        source_hash: row.get(11)?,
        args: serde_json::from_str(&row.get::<_, String>(12)?).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                12,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        budget: serde_json::from_str(&row.get::<_, String>(13)?).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                13,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        state: row.get(14)?,
        final_output: final_output
            .map(|value| serde_json::from_str(&value))
            .transpose()
            .map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    15,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?,
        handover: handover
            .map(|value| serde_json::from_str(&value))
            .transpose()
            .map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    16,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?,
        error_message: row.get(17)?,
        engine_version: row.get(18)?,
        dispatch_bound_agent_ids: serde_json::from_str(&row.get::<_, String>(21)?).map_err(
            |error| {
                rusqlite::Error::FromSqlConversionFailure(
                    21,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            },
        )?,
        dispatch_bound_task_definition_ids: serde_json::from_str(&row.get::<_, String>(22)?)
            .map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    22,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?,
        wake_at: row.get::<_, Option<String>>(23)?.map(time).transpose()?,
        inherited_worker_placement: row
            .get::<_, Option<String>>(24)?
            .map(|value| serde_json::from_str(&value))
            .transpose()
            .map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    24,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?,
        workflow_call_tools: row
            .get::<_, Option<String>>(25)?
            .map(|value| serde_json::from_str(&value))
            .transpose()
            .map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    25,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?,
        created_at: time(row.get(19)?)?,
        completed_at: completed_at.map(time).transpose()?,
    })
}

const SCRIPT_COLUMNS: &str = "id, space_id, name, description, delivery_grant_json, captured_sources_grant, authoring_session_id, created_at, updated_at";
const VERSION_COLUMNS: &str = "id, script_id, version, source, created_at";
const RUN_COLUMNS: &str = "id, space_id, parent_run_id, parent_seq, script_version_id, \
    COALESCE((SELECT script_id FROM workflow_script_versions WHERE id = workflow_runs.script_version_id), inline_workflow_id), \
    COALESCE((SELECT name FROM workflow_scripts WHERE id = (SELECT script_id FROM workflow_script_versions WHERE id = workflow_runs.script_version_id)), inline_workflow_name), \
    COALESCE((SELECT version FROM workflow_script_versions WHERE id = workflow_runs.script_version_id), inline_workflow_version), \
    carrying_skill, task, source, source_hash, args_json, budget_json, state, final_output_json, handover_json, error_message, engine_version, created_at, completed_at, dispatch_bound_agent_ids_json, dispatch_bound_task_definition_ids_json, wake_at, inherited_worker_placement_json, workflow_call_tools_json";
const JOURNAL_COLUMNS: &str = "id, run_id, seq, completed_order, kind, status, args_hash, request_json, result_json, terminal_metadata_json, error_message, effect_kind, idempotency_key, target_kind, started_at, completed_at, dispatch_json, dispatch_attempts_json, termination_json";
const RUN_EVENT_COLUMNS: &str = "id, run_id, journal_entry_id, seq, kind, payload_json, source_surface, source_agent_id, source_session_id, source_turn_id, posted_at, delivered_wake_seq, delivered_at";

fn run_event_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkflowRunEventRecord> {
    let delivered_at: Option<String> = row.get(12)?;
    Ok(WorkflowRunEventRecord {
        id: uuid(row.get(0)?)?,
        run_id: uuid(row.get(1)?)?,
        journal_entry_id: uuid(row.get(2)?)?,
        seq: row.get::<_, i64>(3)? as u64,
        kind: row.get(4)?,
        payload: serde_json::from_str(&row.get::<_, String>(5)?).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                5,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        source_surface: row.get(6)?,
        source_agent_id: row.get(7)?,
        source_session_id: row.get::<_, Option<String>>(8)?.map(uuid).transpose()?,
        source_turn_id: row.get::<_, Option<String>>(9)?.map(uuid).transpose()?,
        posted_at: time(row.get(10)?)?,
        delivered_wake_seq: row.get::<_, Option<i64>>(11)?.map(|value| value as u64),
        delivered_at: delivered_at.map(time).transpose()?,
    })
}

fn journal_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkflowJournalRecord> {
    let result: Option<String> = row.get(8)?;
    let terminal_metadata: Option<String> = row.get(9)?;
    let key: Option<String> = row.get(12)?;
    let completed: Option<String> = row.get(15)?;
    Ok(WorkflowJournalRecord {
        id: uuid(row.get(0)?)?,
        run_id: uuid(row.get(1)?)?,
        seq: row.get::<_, i64>(2)? as u64,
        completed_order: row.get::<_, Option<i64>>(3)?.map(|value| value as u64),
        kind: row.get(4)?,
        status: row.get(5)?,
        args_hash: row.get(6)?,
        request: serde_json::from_str(&row.get::<_, String>(7)?).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                7,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        result: result
            .map(|value| serde_json::from_str(&value))
            .transpose()
            .map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    8,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?,
        terminal_metadata: terminal_metadata
            .map(|value| serde_json::from_str(&value))
            .transpose()
            .map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    9,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?,
        error_message: row.get(10)?,
        effect_kind: row.get(11)?,
        idempotency_key: key.map(uuid).transpose()?,
        target_kind: row.get(13)?,
        started_at: time(row.get(14)?)?,
        completed_at: completed.map(time).transpose()?,
        dispatch: json_column(row, 16)?,
        dispatch_attempts: json_column(row, 17)?,
        termination: json_column(row, 18)?,
    })
}

/// Read a nullable generic-JSON column, failing the row rather than dropping
/// a record the projection is contracted to read.
fn json_column(
    row: &rusqlite::Row<'_>,
    index: usize,
) -> rusqlite::Result<Option<serde_json::Value>> {
    row.get::<_, Option<String>>(index)?
        .map(|value| serde_json::from_str(&value))
        .transpose()
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                index,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })
}

#[async_trait]
impl WorkflowStore for SqliteStorage {
    async fn create_workflow_script(
        &self,
        script: NewWorkflowScript,
    ) -> Result<(WorkflowScriptRecord, WorkflowScriptVersion), StorageError> {
        let id = Uuid::new_v4();
        let version_id = Uuid::new_v4();
        let now = Utc::now();
        let grant = script
            .delivery_grant
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|error| StorageError::QueryFailed(error.to_string()))?;
        let created = self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            tx.execute("INSERT INTO workflow_scripts (id, space_id, name, description, delivery_grant_json, captured_sources_grant, authoring_session_id, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)", params![id.to_string(), script.space_id.to_string(), script.name, script.description, grant, script.captured_sources_grant, script.authoring_session_id.map(|id| id.to_string()), now.to_rfc3339()])?;
            tx.execute("INSERT INTO workflow_script_versions (id, script_id, version, source, created_at) VALUES (?1, ?2, 1, ?3, ?4)", params![version_id.to_string(), id.to_string(), script.source, now.to_rfc3339()])?;
            tx.commit()?;
            Ok((id, version_id))
        }).await?;
        let record = self
            .get_workflow_script(created.0)
            .await?
            .expect("created script exists");
        let version = self
            .get_workflow_script_head(created.0)
            .await?
            .expect("created version exists");
        Ok((record, version))
    }

    async fn append_workflow_script_version(
        &self,
        script_id: Uuid,
        source: &str,
    ) -> Result<WorkflowScriptVersion, StorageError> {
        let id = Uuid::new_v4();
        let source = source.to_string();
        let now = Utc::now();
        self.with_conn(move |conn| {
            conn.execute("INSERT INTO workflow_script_versions (id, script_id, version, source, created_at) SELECT ?1, ?2, COALESCE(MAX(version), 0) + 1, ?3, ?4 FROM workflow_script_versions WHERE script_id = ?2", params![id.to_string(), script_id.to_string(), source, now.to_rfc3339()])?;
            conn.query_row(&format!("SELECT {VERSION_COLUMNS} FROM workflow_script_versions WHERE id = ?1"), [id.to_string()], version_row)
        }).await
    }

    async fn get_workflow_script(
        &self,
        id: Uuid,
    ) -> Result<Option<WorkflowScriptRecord>, StorageError> {
        self.with_conn(move |conn| {
            conn.query_row(
                &format!("SELECT {SCRIPT_COLUMNS} FROM workflow_scripts WHERE id = ?1"),
                [id.to_string()],
                script_row,
            )
            .optional()
        })
        .await
    }

    async fn get_workflow_script_head(
        &self,
        id: Uuid,
    ) -> Result<Option<WorkflowScriptVersion>, StorageError> {
        self.with_conn(move |conn| conn.query_row(&format!("SELECT {VERSION_COLUMNS} FROM workflow_script_versions WHERE script_id = ?1 ORDER BY version DESC LIMIT 1"), [id.to_string()], version_row).optional()).await
    }

    async fn get_workflow_script_version(
        &self,
        id: Uuid,
    ) -> Result<Option<WorkflowScriptVersion>, StorageError> {
        self.with_conn(move |conn| {
            conn.query_row(
                &format!("SELECT {VERSION_COLUMNS} FROM workflow_script_versions WHERE id = ?1"),
                [id.to_string()],
                version_row,
            )
            .optional()
        })
        .await
    }

    async fn update_workflow_script(
        &self,
        id: Uuid,
        description: Option<&str>,
        delivery_grant: Option<serde_json::Value>,
        captured_sources_grant: Option<bool>,
        authoring_session_id: Option<Uuid>,
    ) -> Result<WorkflowScriptRecord, StorageError> {
        let description = description.map(str::to_string);
        let grant = delivery_grant
            .map(|value| serde_json::to_string(&value))
            .transpose()
            .map_err(|error| StorageError::QueryFailed(error.to_string()))?;
        let authoring_session_id = authoring_session_id.map(|id| id.to_string());
        let now = Utc::now().to_rfc3339();
        self.with_conn(move |conn| { conn.execute("UPDATE workflow_scripts SET description=COALESCE(?2,description), delivery_grant_json=COALESCE(?3,delivery_grant_json), captured_sources_grant=COALESCE(?4,captured_sources_grant), authoring_session_id=COALESCE(?5,authoring_session_id), updated_at=?6 WHERE id=?1",params![id.to_string(),description,grant,captured_sources_grant,authoring_session_id,now])?; conn.query_row(&format!("SELECT {SCRIPT_COLUMNS} FROM workflow_scripts WHERE id=?1"),[id.to_string()],script_row) }).await
    }

    async fn list_workflow_scripts(
        &self,
        space_id: Uuid,
    ) -> Result<Vec<WorkflowScriptRecord>, StorageError> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {SCRIPT_COLUMNS} FROM workflow_scripts WHERE space_id = ?1 ORDER BY name"
            ))?;
            stmt.query_map([space_id.to_string()], script_row)?
                .collect()
        })
        .await
    }

    async fn create_workflow_run(
        &self,
        run: NewWorkflowRun,
    ) -> Result<WorkflowRunRecord, StorageError> {
        let id = Uuid::new_v4();
        let now = Utc::now();
        let args = serde_json::to_string(&run.args)
            .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
        let budget = serde_json::to_string(&run.budget)
            .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
        let dispatch_bound_agent_ids = serde_json::to_string(&run.dispatch_bound_agent_ids)
            .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
        let dispatch_bound_task_definition_ids =
            serde_json::to_string(&run.dispatch_bound_task_definition_ids)
                .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
        let inherited_worker_placement = run
            .inherited_worker_placement
            .map(|placement| serde_json::to_string(&placement))
            .transpose()
            .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
        let workflow_call_tools = run
            .workflow_call_tools
            .map(|tools| serde_json::to_string(&tools))
            .transpose()
            .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
        self.with_conn(move |conn| { conn.execute("INSERT INTO workflow_runs (id, space_id, parent_run_id, parent_seq, script_version_id, inline_workflow_id, inline_workflow_name, inline_workflow_version, carrying_skill, task, source, source_hash, args_json, budget_json, state, engine_version, dispatch_bound_agent_ids_json, dispatch_bound_task_definition_ids_json, inherited_worker_placement_json, workflow_call_tools_json, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, 'running', ?15, ?16, ?17, ?18, ?19, ?20)", params![id.to_string(), run.space_id.to_string(), run.parent_run_id.map(|v| v.to_string()), run.parent_seq.map(|v| v as i64), run.script_version_id.map(|v| v.to_string()), run.inline_workflow_id.map(|v| v.to_string()), run.inline_workflow_name, run.inline_workflow_version, run.carrying_skill, run.task, run.source, run.source_hash, args, budget, run.engine_version, dispatch_bound_agent_ids, dispatch_bound_task_definition_ids, inherited_worker_placement, workflow_call_tools, now.to_rfc3339()])?; conn.query_row(&format!("SELECT {RUN_COLUMNS} FROM workflow_runs WHERE id = ?1"), [id.to_string()], run_row) }).await
    }

    async fn get_workflow_run(&self, id: Uuid) -> Result<Option<WorkflowRunRecord>, StorageError> {
        self.with_conn(move |conn| {
            conn.query_row(
                &format!("SELECT {RUN_COLUMNS} FROM workflow_runs WHERE id = ?1"),
                [id.to_string()],
                run_row,
            )
            .optional()
        })
        .await
    }
    async fn find_child_workflow_run(
        &self,
        parent_run_id: Uuid,
        parent_seq: u64,
    ) -> Result<Option<WorkflowRunRecord>, StorageError> {
        self.with_conn(move |conn| {
            conn.query_row(
                &format!(
                    "SELECT {RUN_COLUMNS} FROM workflow_runs WHERE parent_run_id = ?1 AND parent_seq = ?2"
                ),
                params![parent_run_id.to_string(), parent_seq as i64],
                run_row,
            )
            .optional()
        })
        .await
    }

    async fn list_workflow_runs(
        &self,
        space_id: Uuid,
    ) -> Result<Vec<WorkflowRunRecord>, StorageError> {
        self.with_conn(move |conn| { let mut stmt = conn.prepare(&format!("SELECT {RUN_COLUMNS} FROM workflow_runs WHERE space_id = ?1 ORDER BY created_at DESC"))?; stmt.query_map([space_id.to_string()], run_row)?.collect() }).await
    }
    async fn try_transition_workflow_run_state(
        &self,
        id: Uuid,
        from: &str,
        to: &str,
    ) -> Result<bool, StorageError> {
        let from = from.to_string();
        let to = to.to_string();
        self.with_conn(move |conn| {
            let count = conn.execute(
                "UPDATE workflow_runs SET state=?3, wake_at=NULL, completed_at=NULL \
                 WHERE id=?1 AND state=?2",
                params![id.to_string(), from, to],
            )?;
            Ok(count > 0)
        })
        .await
    }

    async fn set_workflow_run_parked(
        &self,
        id: Uuid,
        wake_at: DateTime<Utc>,
    ) -> Result<(), StorageError> {
        self.with_conn(move |conn| {
            conn.execute(
                "UPDATE workflow_runs SET state='parked', wake_at=?2, completed_at=NULL WHERE id=?1",
                params![id.to_string(), wake_at.to_rfc3339()],
            )?;
            Ok(())
        })
        .await
    }

    async fn park_workflow_run_if_no_matching_events(
        &self,
        id: Uuid,
        wake_at: DateTime<Utc>,
        kinds: &[String],
    ) -> Result<bool, StorageError> {
        let kinds = serde_json::to_string(kinds)
            .map_err(|error| StorageError::QueryFailed(error.to_string()))?;
        self.with_conn(move |conn| {
            let changed = conn.execute(
                "UPDATE workflow_runs SET state='parked', wake_at=?2, completed_at=NULL \
                 WHERE id=?1 AND state='running' AND NOT EXISTS (\
                     SELECT 1 FROM workflow_run_events \
                     WHERE run_id=?1 AND delivered_at IS NULL \
                       AND kind IN (SELECT value FROM json_each(?3))\
                 )",
                params![id.to_string(), wake_at.to_rfc3339(), kinds],
            )?;
            Ok(changed == 1)
        })
        .await
    }

    async fn make_parked_workflow_run_due(&self, id: Uuid) -> Result<bool, StorageError> {
        let now = Utc::now().to_rfc3339();
        self.with_conn(move |conn| {
            let changed = conn.execute(
                "UPDATE workflow_runs SET wake_at=?2 WHERE id=?1 AND state='parked'",
                params![id.to_string(), now],
            )?;
            Ok(changed == 1)
        })
        .await
    }

    async fn list_due_parked_workflow_runs(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Vec<WorkflowRunRecord>, StorageError> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {RUN_COLUMNS} FROM workflow_runs \
                 WHERE state = 'parked' AND wake_at <= ?1 ORDER BY wake_at ASC"
            ))?;
            stmt.query_map([now.to_rfc3339()], run_row)?.collect()
        })
        .await
    }

    async fn list_parked_workflow_runs(&self) -> Result<Vec<WorkflowRunRecord>, StorageError> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {RUN_COLUMNS} FROM workflow_runs WHERE state = 'parked' ORDER BY wake_at ASC"
            ))?;
            stmt.query_map([], run_row)?.collect()
        })
        .await
    }

    async fn list_interrupted_workflow_runs(&self) -> Result<Vec<WorkflowRunRecord>, StorageError> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {RUN_COLUMNS} FROM workflow_runs \
                 WHERE state = 'interrupted' ORDER BY created_at ASC"
            ))?;
            stmt.query_map([], run_row)?.collect()
        })
        .await
    }

    async fn update_workflow_run_state(
        &self,
        id: Uuid,
        state: &str,
        final_output: Option<serde_json::Value>,
        handover: Option<serde_json::Value>,
        error_message: Option<&str>,
    ) -> Result<(), StorageError> {
        let state = state.to_string();
        let output = final_output
            .map(|v| serde_json::to_string(&v))
            .transpose()
            .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
        let handover = handover
            .map(|value| serde_json::to_string(&value))
            .transpose()
            .map_err(|error| StorageError::QueryFailed(error.to_string()))?;
        let error = error_message.map(str::to_string);
        let now = Utc::now().to_rfc3339();
        self.with_conn(move |conn| { conn.execute("UPDATE workflow_runs SET state=?2, final_output_json=?3, handover_json=?4, error_message=?5, wake_at=NULL, completed_at=CASE WHEN ?2='running' THEN NULL ELSE ?6 END WHERE id=?1", params![id.to_string(),state,output,handover,error,now])?; Ok(()) }).await
    }
    async fn interrupt_running_workflow_runs(&self) -> Result<u64, StorageError> {
        self.with_conn(|conn| { let count=conn.execute("UPDATE workflow_runs SET state='interrupted', completed_at=NULL WHERE state='running'", [])?; Ok(count as u64) }).await
    }

    async fn create_workflow_run_event(
        &self,
        event: NewWorkflowRunEvent,
    ) -> Result<WorkflowRunEventRecord, StorageError> {
        let id = Uuid::new_v4();
        let now = Utc::now();
        let payload = serde_json::to_string(&event.payload)
            .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
        self.with_conn(move |conn| {
            // Seq assignment and the idempotency key live in one statement:
            // a replayed post (same journal entry) inserts nothing and reads
            // back the row the original run created.
            conn.execute(
                "INSERT INTO workflow_run_events (id, run_id, journal_entry_id, seq, kind, payload_json, source_surface, source_agent_id, source_session_id, source_turn_id, posted_at) \
                 VALUES (?1, ?2, ?3, (SELECT COALESCE(MAX(seq), 0) + 1 FROM workflow_run_events WHERE run_id = ?2), ?4, ?5, ?6, ?7, ?8, ?9, ?10) \
                 ON CONFLICT(journal_entry_id) DO NOTHING",
                params![
                    id.to_string(),
                    event.run_id.to_string(),
                    event.journal_entry_id.to_string(),
                    event.kind,
                    payload,
                    event.source_surface,
                    event.source_agent_id,
                    event.source_session_id.map(|id| id.to_string()),
                    event.source_turn_id.map(|id| id.to_string()),
                    now.to_rfc3339()
                ],
            )?;
            conn.query_row(
                &format!(
                    "SELECT {RUN_EVENT_COLUMNS} FROM workflow_run_events WHERE journal_entry_id = ?1"
                ),
                [event.journal_entry_id.to_string()],
                run_event_row,
            )
        })
        .await
    }

    async fn list_pending_workflow_run_events(
        &self,
        run_id: Uuid,
    ) -> Result<Vec<WorkflowRunEventRecord>, StorageError> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {RUN_EVENT_COLUMNS} FROM workflow_run_events \
                 WHERE run_id = ?1 AND delivered_at IS NULL ORDER BY seq ASC"
            ))?;
            stmt.query_map([run_id.to_string()], run_event_row)?
                .collect()
        })
        .await
    }

    async fn list_workflow_run_events(
        &self,
        run_id: Uuid,
    ) -> Result<Vec<WorkflowRunEventRecord>, StorageError> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {RUN_EVENT_COLUMNS} FROM workflow_run_events \
                 WHERE run_id = ?1 ORDER BY seq ASC"
            ))?;
            stmt.query_map([run_id.to_string()], run_event_row)?
                .collect()
        })
        .await
    }

    async fn mark_workflow_run_events_delivered(
        &self,
        run_id: Uuid,
        up_to_seq: u64,
        delivered_wake_seq: u64,
    ) -> Result<u64, StorageError> {
        let now = Utc::now();
        self.with_conn(move |conn| {
            let count = conn.execute(
                "UPDATE workflow_run_events SET delivered_wake_seq = ?3, delivered_at = ?4 \
                 WHERE run_id = ?1 AND seq <= ?2 AND delivered_at IS NULL",
                params![
                    run_id.to_string(),
                    up_to_seq as i64,
                    delivered_wake_seq as i64,
                    now.to_rfc3339()
                ],
            )?;
            Ok(count as u64)
        })
        .await
    }

    async fn mark_workflow_run_event_delivered(
        &self,
        run_id: Uuid,
        event_seq: u64,
        delivered_wake_seq: u64,
    ) -> Result<bool, StorageError> {
        let now = Utc::now();
        self.with_conn(move |conn| {
            let changed = conn.execute(
                "UPDATE workflow_run_events SET delivered_wake_seq = ?3, delivered_at = ?4 \
                 WHERE run_id = ?1 AND seq = ?2 AND delivered_at IS NULL",
                params![
                    run_id.to_string(),
                    event_seq as i64,
                    delivered_wake_seq as i64,
                    now.to_rfc3339()
                ],
            )?;
            Ok(changed == 1)
        })
        .await
    }

    async fn create_workflow_journal_intent(
        &self,
        intent: NewWorkflowJournalIntent,
    ) -> Result<WorkflowJournalRecord, StorageError> {
        let id = Uuid::new_v4();
        let now = Utc::now();
        let request = serde_json::to_string(&intent.request)
            .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
        self.with_conn(move |conn| { conn.execute("INSERT INTO workflow_journal (id, run_id, seq, kind, status, args_hash, request_json, effect_kind, idempotency_key, target_kind, started_at) VALUES (?1, ?2, ?3, ?4, 'started', ?5, ?6, ?7, ?8, ?9, ?10)", params![id.to_string(),intent.run_id.to_string(),intent.seq as i64,intent.kind,intent.args_hash,request,intent.effect_kind,intent.idempotency_key.map(|v|v.to_string()),intent.target_kind,now.to_rfc3339()])?; conn.query_row(&format!("SELECT {JOURNAL_COLUMNS} FROM workflow_journal WHERE id=?1"),[id.to_string()],journal_row) }).await
    }

    async fn create_failed_workflow_journal_entry(
        &self,
        intent: NewWorkflowJournalIntent,
        error_message: &str,
    ) -> Result<WorkflowJournalRecord, StorageError> {
        let id = Uuid::new_v4();
        let now = Utc::now();
        let request = serde_json::to_string(&intent.request)
            .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
        let error_message = error_message.to_string();
        self.with_conn(move |conn| { let tx=conn.unchecked_transaction()?; let order:i64=tx.query_row("SELECT COALESCE(MAX(completed_order),0)+1 FROM workflow_journal WHERE run_id=?1",[intent.run_id.to_string()],|row|row.get(0))?; tx.execute("INSERT INTO workflow_journal (id, run_id, seq, completed_order, kind, status, args_hash, request_json, error_message, effect_kind, idempotency_key, target_kind, started_at, completed_at) VALUES (?1, ?2, ?3, ?4, ?5, 'failed', ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12)", params![id.to_string(),intent.run_id.to_string(),intent.seq as i64,order,intent.kind,intent.args_hash,request,error_message,intent.effect_kind,intent.idempotency_key.map(|v|v.to_string()),intent.target_kind,now.to_rfc3339()])?; tx.commit()?; conn.query_row(&format!("SELECT {JOURNAL_COLUMNS} FROM workflow_journal WHERE id=?1"),[id.to_string()],journal_row) }).await
    }

    async fn complete_workflow_journal_entry(
        &self,
        id: Uuid,
        result: serde_json::Value,
    ) -> Result<WorkflowJournalRecord, StorageError> {
        let result =
            serde_json::to_string(&result).map_err(|e| StorageError::QueryFailed(e.to_string()))?;
        let now = Utc::now();
        self.with_conn(move |conn| { let tx=conn.unchecked_transaction()?; let run_id:String=tx.query_row("SELECT run_id FROM workflow_journal WHERE id=?1",[id.to_string()],|row|row.get(0))?; let order:i64=tx.query_row("SELECT COALESCE(MAX(completed_order),0)+1 FROM workflow_journal WHERE run_id=?1",[run_id],|row|row.get(0))?; tx.execute("UPDATE workflow_journal SET status='completed', result_json=?2, completed_order=?3, completed_at=?4 WHERE id=?1",params![id.to_string(),result,order,now.to_rfc3339()])?; tx.commit()?; conn.query_row(&format!("SELECT {JOURNAL_COLUMNS} FROM workflow_journal WHERE id=?1"),[id.to_string()],journal_row) }).await
    }

    async fn set_workflow_journal_terminal_metadata(
        &self,
        id: Uuid,
        metadata: serde_json::Value,
    ) -> Result<WorkflowJournalRecord, StorageError> {
        let metadata = serde_json::to_string(&metadata)
            .map_err(|error| StorageError::QueryFailed(error.to_string()))?;
        self.with_conn(move |connection| {
            connection.execute(
                "UPDATE workflow_journal SET terminal_metadata_json = ?2 WHERE id = ?1",
                params![id.to_string(), metadata],
            )?;
            connection.query_row(
                &format!("SELECT {JOURNAL_COLUMNS} FROM workflow_journal WHERE id = ?1"),
                [id.to_string()],
                journal_row,
            )
        })
        .await
    }

    async fn fail_workflow_journal_entry(
        &self,
        id: Uuid,
        error_message: &str,
    ) -> Result<WorkflowJournalRecord, StorageError> {
        let error_message = error_message.to_string();
        let now = Utc::now();
        self.with_conn(move |conn| { let tx=conn.unchecked_transaction()?; let run_id:String=tx.query_row("SELECT run_id FROM workflow_journal WHERE id=?1",[id.to_string()],|row|row.get(0))?; let order:i64=tx.query_row("SELECT COALESCE(MAX(completed_order),0)+1 FROM workflow_journal WHERE run_id=?1",[run_id],|row|row.get(0))?; tx.execute("UPDATE workflow_journal SET status='failed', error_message=?2, completed_order=?3, completed_at=?4 WHERE id=?1",params![id.to_string(),error_message,order,now.to_rfc3339()])?; tx.commit()?; conn.query_row(&format!("SELECT {JOURNAL_COLUMNS} FROM workflow_journal WHERE id=?1"),[id.to_string()],journal_row) }).await
    }

    async fn record_workflow_call_dispatch(
        &self,
        id: Uuid,
        dispatch: serde_json::Value,
    ) -> Result<(), StorageError> {
        let dispatch = serde_json::to_string(&dispatch)
            .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
        let updated = self
            .with_conn(move |conn| {
                conn.execute(
                    "UPDATE workflow_journal SET dispatch_json=?2 WHERE id=?1",
                    params![id.to_string(), dispatch],
                )
            })
            .await?;
        // An update that matches nothing means the entry this record belongs
        // to is not there. Reporting success would leave the caller believing
        // the account was written.
        if updated == 0 {
            return Err(StorageError::NotFound {
                entity: "workflow journal entry".to_string(),
                id: id.to_string(),
            });
        }
        Ok(())
    }

    async fn record_workflow_call_attempt(
        &self,
        id: Uuid,
        attempt: serde_json::Value,
    ) -> Result<Vec<serde_json::Value>, StorageError> {
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            // Read, append, write inside one transaction: an attempt that
            // replaced its predecessor would erase the turn the interrupted
            // attempt ran, which is the record this list exists to keep.
            let stored: Option<Option<String>> = tx
                .query_row(
                    "SELECT dispatch_attempts_json FROM workflow_journal WHERE id=?1",
                    [id.to_string()],
                    |row| row.get(0),
                )
                .optional()?;
            // No row means the entry this record belongs to is not there.
            // Reporting success would leave the caller believing the account
            // was written.
            let Some(stored) = stored else {
                return Ok(None);
            };
            let earlier: Vec<serde_json::Value> = match stored {
                Some(json) => serde_json::from_str(&json).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?,
                None => Vec::new(),
            };
            let mut attempts = earlier.clone();
            attempts.push(attempt);
            let encoded = serde_json::to_string(&attempts)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
            tx.execute(
                "UPDATE workflow_journal SET dispatch_attempts_json=?2 WHERE id=?1",
                params![id.to_string(), encoded],
            )?;
            tx.commit()?;
            Ok(Some(earlier))
        })
        .await?
        .ok_or_else(|| StorageError::NotFound {
            entity: "workflow journal entry".to_string(),
            id: id.to_string(),
        })
    }

    async fn cut_short_workflow_journal_entry(
        &self,
        id: Uuid,
        termination: serde_json::Value,
        error_message: &str,
    ) -> Result<WorkflowJournalRecord, StorageError> {
        let termination = serde_json::to_string(&termination)
            .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
        let error_message = error_message.to_string();
        let now = Utc::now();
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let run_id: String = tx.query_row(
                "SELECT run_id FROM workflow_journal WHERE id=?1",
                [id.to_string()],
                |row| row.get(0),
            )?;
            let order: i64 = tx.query_row(
                "SELECT COALESCE(MAX(completed_order),0)+1 FROM workflow_journal WHERE run_id=?1",
                [run_id],
                |row| row.get(0),
            )?;
            tx.execute(
                "UPDATE workflow_journal SET status='failed', error_message=?2, \
                 termination_json=?3, completed_order=?4, completed_at=?5 WHERE id=?1",
                params![
                    id.to_string(),
                    error_message,
                    termination,
                    order,
                    now.to_rfc3339()
                ],
            )?;
            tx.commit()?;
            conn.query_row(
                &format!("SELECT {JOURNAL_COLUMNS} FROM workflow_journal WHERE id=?1"),
                [id.to_string()],
                journal_row,
            )
        })
        .await
    }

    async fn list_workflow_journal(
        &self,
        run_id: Uuid,
    ) -> Result<Vec<WorkflowJournalRecord>, StorageError> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {JOURNAL_COLUMNS} FROM workflow_journal WHERE run_id=?1 ORDER BY seq"
            ))?;
            stmt.query_map([run_id.to_string()], journal_row)?.collect()
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NewSpace, SpaceStore};

    #[tokio::test(flavor = "multi_thread")]
    async fn list_parked_workflow_runs_returns_only_parked_runs_across_spaces() {
        let storage = SqliteStorage::in_memory().expect("storage");
        let first_space = storage
            .create(NewSpace {
                name: "first workflow space".into(),
                slug: "first-workflow-space".parse().expect("slug"),
            })
            .await
            .expect("first space");
        let second_space = storage
            .create(NewSpace {
                name: "second workflow space".into(),
                slug: "second-workflow-space".parse().expect("slug"),
            })
            .await
            .expect("second space");
        let new_run = |space_id| NewWorkflowRun {
            space_id,
            parent_run_id: None,
            parent_seq: None,
            script_version_id: None,
            inline_workflow_id: None,
            inline_workflow_name: None,
            inline_workflow_version: None,
            carrying_skill: None,
            task: None,
            source: Some("return null".into()),
            source_hash: Uuid::new_v4().to_string(),
            args: serde_json::Value::Null,
            budget: serde_json::json!({}),
            engine_version: "test".into(),
            dispatch_bound_agent_ids: vec![],
            dispatch_bound_task_definition_ids: vec![],
            workflow_call_tools: None,
            inherited_worker_placement: None,
        };

        let parked_first = storage
            .create_workflow_run(new_run(first_space.id))
            .await
            .expect("first parked run");
        storage
            .set_workflow_run_parked(parked_first.id, Utc::now() + chrono::Duration::hours(2))
            .await
            .expect("park first run");
        let running = storage
            .create_workflow_run(new_run(first_space.id))
            .await
            .expect("running run");
        let parked_second = storage
            .create_workflow_run(new_run(second_space.id))
            .await
            .expect("second parked run");
        storage
            .set_workflow_run_parked(parked_second.id, Utc::now() + chrono::Duration::hours(1))
            .await
            .expect("park second run");

        let parked = storage
            .list_parked_workflow_runs()
            .await
            .expect("list parked runs");
        assert_eq!(
            parked.iter().map(|run| run.id).collect::<Vec<_>>(),
            vec![parked_second.id, parked_first.id],
            "the lister includes parked runs from every space in wake order"
        );
        assert!(
            parked.iter().all(|run| run.state == "parked"),
            "the lister excludes non-parked runs"
        );
        assert!(
            !parked.iter().any(|run| run.id == running.id),
            "running workflows are not recovery owners"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn script_versions_append_and_runs_survive_reopen_shape() {
        let storage = SqliteStorage::in_memory().expect("storage");
        let space = storage
            .create(NewSpace {
                name: "workflow-test".into(),
                slug: "workflow-test".parse().expect("slug"),
            })
            .await
            .expect("space");
        let (script, first) = storage
            .create_workflow_script(NewWorkflowScript {
                space_id: space.id,
                name: "publish".into(),
                description: "reviewed delivery".into(),
                source: "return 1".into(),
                delivery_grant: None,
                captured_sources_grant: Default::default(),
                authoring_session_id: None,
            })
            .await
            .expect("script");
        let second = storage
            .append_workflow_script_version(script.id, "return 2")
            .await
            .expect("version");
        assert_eq!(first.version, 1);
        assert_eq!(second.version, 2);
        let run = storage
            .create_workflow_run(NewWorkflowRun {
                space_id: space.id,
                parent_run_id: None,
                parent_seq: None,
                script_version_id: Some(second.id),
                inline_workflow_id: None,
                inline_workflow_name: None,
                inline_workflow_version: None,
                carrying_skill: None,
                task: None,
                source: None,
                source_hash: "hash".into(),
                args: serde_json::json!({}),
                budget: serde_json::json!({}),
                engine_version: "rquickjs-0.12.0".into(),
                dispatch_bound_agent_ids: vec![],
                dispatch_bound_task_definition_ids: vec![],
                workflow_call_tools: None,
                inherited_worker_placement: None,
            })
            .await
            .expect("run");
        assert_eq!(run.script_id, Some(script.id));
        assert_eq!(run.script_name.as_deref(), Some("publish"));
        assert_eq!(run.script_version, Some(2));

        let inline_id = Uuid::new_v4();
        let inline = storage
            .create_workflow_run(NewWorkflowRun {
                space_id: space.id,
                parent_run_id: None,
                parent_seq: None,
                script_version_id: None,
                inline_workflow_id: Some(inline_id),
                inline_workflow_name: Some("compiled-workflow".into()),
                inline_workflow_version: Some(3),
                carrying_skill: None,
                task: None,
                source: Some("return 3".into()),
                source_hash: "inline-hash".into(),
                args: serde_json::json!({}),
                budget: serde_json::json!({}),
                engine_version: "rquickjs-0.12.0".into(),
                dispatch_bound_agent_ids: vec![],
                dispatch_bound_task_definition_ids: vec![],
                workflow_call_tools: None,
                inherited_worker_placement: None,
            })
            .await
            .expect("inline workflow run");
        let inline = storage
            .get_workflow_run(inline.id)
            .await
            .expect("inline run query")
            .expect("inline run");
        assert_eq!(inline.script_version_id, None);
        assert_eq!(inline.script_id, Some(inline_id));
        assert_eq!(inline.script_name.as_deref(), Some("compiled-workflow"));
        assert_eq!(inline.script_version, Some(3));
        assert_eq!(inline.source.as_deref(), Some("return 3"));
        assert_eq!(inline.source_hash, "inline-hash");
        storage
            .interrupt_running_workflow_runs()
            .await
            .expect("recover");
        assert_eq!(
            storage
                .get_workflow_run(run.id)
                .await
                .expect("get")
                .expect("present")
                .state,
            "interrupted"
        );

        let first = storage
            .create_workflow_journal_intent(NewWorkflowJournalIntent {
                run_id: run.id,
                seq: 0,
                kind: "agent_call".into(),
                args_hash: "a".into(),
                request: serde_json::json!({}),
                effect_kind: None,
                idempotency_key: None,
                target_kind: None,
            })
            .await
            .expect("intent");
        let second = storage
            .create_workflow_journal_intent(NewWorkflowJournalIntent {
                run_id: run.id,
                seq: 1,
                kind: "effect".into(),
                args_hash: "b".into(),
                request: serde_json::json!({}),
                effect_kind: Some("idempotency_key".into()),
                idempotency_key: Some(Uuid::new_v4()),
                target_kind: Some("webhook".into()),
            })
            .await
            .expect("effect intent");
        let second = storage
            .complete_workflow_journal_entry(second.id, serde_json::json!({"delivered":true}))
            .await
            .expect("second completion");
        let first = storage
            .complete_workflow_journal_entry(first.id, serde_json::json!({"output":"draft"}))
            .await
            .expect("first completion");
        assert_eq!(second.completed_order, Some(1));
        assert_eq!(first.completed_order, Some(2));

        let refusal = storage
            .create_failed_workflow_journal_entry(
                NewWorkflowJournalIntent {
                    run_id: run.id,
                    seq: 2,
                    kind: "agent_call".into(),
                    args_hash: "c".into(),
                    request: serde_json::json!({}),
                    effect_kind: None,
                    idempotency_key: None,
                    target_kind: None,
                },
                "BudgetExceededError: agent call limit reached",
            )
            .await
            .expect("terminal refusal");
        assert_eq!(refusal.status, "failed");
        assert_eq!(refusal.completed_order, Some(3));
        assert_eq!(
            refusal.error_message.as_deref(),
            Some("BudgetExceededError: agent call limit reached")
        );
        assert!(refusal.completed_at.is_some());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn run_events_assign_dense_seqs_and_deliver_idempotently() {
        let storage = SqliteStorage::in_memory().expect("storage");
        let space = storage
            .create(NewSpace {
                name: "events-test".into(),
                slug: "events-test".parse().expect("slug"),
            })
            .await
            .expect("space");
        let run = storage
            .create_workflow_run(NewWorkflowRun {
                space_id: space.id,
                parent_run_id: None,
                parent_seq: None,
                script_version_id: None,
                inline_workflow_id: None,
                inline_workflow_name: None,
                inline_workflow_version: None,
                carrying_skill: None,
                task: None,
                source: Some("return null".into()),
                source_hash: "hash".into(),
                args: serde_json::json!({}),
                budget: serde_json::json!({}),
                engine_version: "rquickjs-0.12.0".into(),
                dispatch_bound_agent_ids: vec![],
                dispatch_bound_task_definition_ids: vec![],
                workflow_call_tools: None,
                inherited_worker_placement: None,
            })
            .await
            .expect("run");

        let journal_ids = [Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()];
        for (index, journal_entry_id) in journal_ids.iter().enumerate() {
            let event = storage
                .create_workflow_run_event(NewWorkflowRunEvent {
                    run_id: run.id,
                    journal_entry_id: *journal_entry_id,
                    kind: format!("worker.{index}.completion"),
                    payload: serde_json::json!({"worker": index}),
                    source_surface: "script".into(),
                    source_agent_id: None,
                    source_session_id: None,
                    source_turn_id: None,
                })
                .await
                .expect("event");
            assert_eq!(event.seq, index as u64 + 1, "seqs are dense per run");
        }

        // A replayed post (same journal entry) returns the existing row
        // rather than a duplicate with a fresh seq.
        let replayed = storage
            .create_workflow_run_event(NewWorkflowRunEvent {
                run_id: run.id,
                journal_entry_id: journal_ids[1],
                kind: "worker.1.completion".into(),
                payload: serde_json::json!({"worker": 1}),
                source_surface: "script".into(),
                source_agent_id: None,
                source_session_id: None,
                source_turn_id: None,
            })
            .await
            .expect("replayed post");
        assert_eq!(replayed.seq, 2);
        assert_eq!(
            storage
                .list_pending_workflow_run_events(run.id)
                .await
                .expect("pending")
                .len(),
            3
        );

        // An await stamps only the event it returned, preserving earlier
        // pending events of other kinds for their own later awaits.
        assert!(
            storage
                .mark_workflow_run_event_delivered(run.id, 2, 6)
                .await
                .expect("deliver one")
        );
        let pending = storage
            .list_pending_workflow_run_events(run.id)
            .await
            .expect("pending after one");
        assert_eq!(
            pending.iter().map(|event| event.seq).collect::<Vec<_>>(),
            [1, 3]
        );

        // Explicit batch delivery still stamps through the read seq;
        // already-delivered rows are untouched, so a re-run stamps nothing.
        let stamped = storage
            .mark_workflow_run_events_delivered(run.id, 2, 7)
            .await
            .expect("deliver");
        assert_eq!(stamped, 1);
        let pending = storage
            .list_pending_workflow_run_events(run.id)
            .await
            .expect("pending after");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].seq, 3);
        assert_eq!(
            storage
                .mark_workflow_run_events_delivered(run.id, 2, 9)
                .await
                .expect("re-deliver"),
            0
        );
        let all = storage
            .list_workflow_run_events(run.id)
            .await
            .expect("all events");
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].delivered_wake_seq, Some(7));
        assert_eq!(all[1].delivered_wake_seq, Some(6));
        assert!(all[2].delivered_at.is_none());

        // The event-wait park is a compare-and-swap over run state and the
        // matching queue predicate. A post that wins this ordering prevents
        // the distant park; an unrelated pending kind does not.
        assert!(
            !storage
                .park_workflow_run_if_no_matching_events(
                    run.id,
                    Utc::now() + chrono::Duration::days(36500),
                    &["worker.2.completion".into()],
                )
                .await
                .expect("matching event blocks park")
        );
        assert_eq!(
            storage
                .get_workflow_run(run.id)
                .await
                .unwrap()
                .unwrap()
                .state,
            "running"
        );
        assert!(
            storage
                .park_workflow_run_if_no_matching_events(
                    run.id,
                    Utc::now() + chrono::Duration::days(36500),
                    &["operator.decision".into()],
                )
                .await
                .expect("unrelated event permits park")
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_dispatch_record_for_an_absent_entry_is_an_error_not_a_silent_success() {
        let storage = SqliteStorage::in_memory().expect("storage");

        let error = storage
            .record_workflow_call_dispatch(Uuid::new_v4(), serde_json::json!({"prompt": "go"}))
            .await
            .expect_err("a dispatch record with no entry to attach to must fail");
        assert!(
            matches!(error, StorageError::NotFound { .. }),
            "expected the missing entry to be reported, got {error:?}"
        );

        let error = storage
            .record_workflow_call_attempt(Uuid::new_v4(), serde_json::json!({"model": "m"}))
            .await
            .expect_err("an attempt record with no entry to attach to must fail");
        assert!(
            matches!(error, StorageError::NotFound { .. }),
            "expected the missing entry to be reported, got {error:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_second_attempt_is_appended_beside_the_first_not_over_it() {
        let storage = SqliteStorage::in_memory().expect("storage");
        let space = storage.get_or_create_default().await.expect("space");
        let run = storage
            .create_workflow_run(NewWorkflowRun {
                space_id: space.id,
                parent_run_id: None,
                parent_seq: None,
                script_version_id: None,
                inline_workflow_id: None,
                inline_workflow_name: None,
                inline_workflow_version: None,
                carrying_skill: None,
                task: None,
                source: Some("return null".into()),
                source_hash: "hash".into(),
                args: serde_json::json!({}),
                budget: serde_json::json!({}),
                engine_version: "test".into(),
                dispatch_bound_agent_ids: vec![],
                dispatch_bound_task_definition_ids: vec![],
                workflow_call_tools: None,
                inherited_worker_placement: None,
            })
            .await
            .expect("run");
        let entry = storage
            .create_workflow_journal_intent(NewWorkflowJournalIntent {
                run_id: run.id,
                seq: 0,
                kind: "agent_call".into(),
                args_hash: "a".into(),
                request: serde_json::json!({}),
                effect_kind: None,
                idempotency_key: None,
                target_kind: None,
            })
            .await
            .expect("intent");

        let before_first = storage
            .record_workflow_call_attempt(entry.id, serde_json::json!({"turn_id": "one"}))
            .await
            .expect("first attempt");
        assert!(
            before_first.is_empty(),
            "the first attempt had none before it"
        );
        let before_second = storage
            .record_workflow_call_attempt(entry.id, serde_json::json!({"turn_id": "two"}))
            .await
            .expect("second attempt");
        assert_eq!(
            before_second,
            vec![serde_json::json!({"turn_id": "one"})],
            "the re-dispatch is told which attempts it superseded"
        );

        let stored = storage
            .list_workflow_journal(run.id)
            .await
            .expect("journal")
            .into_iter()
            .find(|stored| stored.id == entry.id)
            .expect("entry")
            .dispatch_attempts
            .expect("attempts");
        assert_eq!(
            stored,
            serde_json::json!([{"turn_id": "one"}, {"turn_id": "two"}]),
            "both attempts stay in the record, oldest first"
        );
    }
}
