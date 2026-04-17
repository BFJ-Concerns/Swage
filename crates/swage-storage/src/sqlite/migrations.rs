use rusqlite::{Connection, params};
use swage_core::StorageError;

/// Track which migrations have been applied.
const CREATE_MIGRATIONS_TABLE: &str = "
    CREATE TABLE IF NOT EXISTS _swage_migrations (
        id         INTEGER PRIMARY KEY AUTOINCREMENT,
        name       TEXT    NOT NULL UNIQUE,
        applied_at TEXT    NOT NULL
    )
";

/// The ordered set of migrations to apply. Each entry is (name, sql).
///
/// Order is significant — migrations are applied in array order and never re-run.
fn migrations() -> Vec<(&'static str, &'static str)> {
    vec![
        ("001_complete_schema", COMPLETE_SCHEMA),
        ("002_context_snapshots", CREATE_CONTEXT_SNAPSHOTS),
        ("003_turn_cost", ADD_TURN_COST_COLUMN),
        ("004_active_skills", CREATE_ACTIVE_SKILLS),
        (
            "005_turn_structured_output",
            ADD_TURN_STRUCTURED_OUTPUT_COLUMN,
        ),
        ("006_approval_requests", CREATE_APPROVAL_REQUESTS),
        ("007_agent_messages", CREATE_AGENT_MESSAGES),
        ("008_turn_parent_turn_id", ADD_TURN_PARENT_TURN_ID),
        ("009_teams", CREATE_TEAMS),
        ("010_topology_trace", CREATE_TOPOLOGY_TRACE),
        ("011_approval_memory", CREATE_APPROVAL_MEMORY),
        ("012_thread_branches", CREATE_THREAD_BRANCHES),
        ("013_message_turn_id", ADD_MESSAGE_TURN_ID),
        ("014_turn_inbound_audit", ADD_TURN_INBOUND_AUDIT),
        ("015_project_default_flag", ADD_PROJECT_DEFAULT_FLAG),
        ("016_session_metadata", ADD_SESSION_METADATA_COLUMN),
        ("017_scheduled_jobs", CREATE_SCHEDULED_JOB_TABLES),
        ("018_heartbeats", CREATE_HEARTBEAT_TABLES),
        ("019_execution_hosts", CREATE_EXECUTION_HOST_TABLE),
    ]
}

/// Per-turn context assembly report, stored as raw JSON to avoid coupling the
/// storage layer to the AssemblyReport type definition.
const CREATE_CONTEXT_SNAPSHOTS: &str = "
    CREATE TABLE IF NOT EXISTS context_snapshots (
        turn_id     TEXT NOT NULL PRIMARY KEY REFERENCES turns(id) ON DELETE CASCADE,
        report_json TEXT NOT NULL,
        created_at  TEXT NOT NULL
    );
";

const CREATE_EXECUTION_HOST_TABLE: &str = "
    CREATE TABLE IF NOT EXISTS execution_hosts (
        host_id           TEXT NOT NULL PRIMARY KEY,
        label             TEXT,
        kind              TEXT NOT NULL,
        base_url          TEXT NOT NULL,
        shared_token      TEXT NOT NULL,
        workspace_root    TEXT NOT NULL,
        capabilities_json TEXT NOT NULL,
        registered_at     TEXT NOT NULL
    );
";

/// Additive migration: cost in microcents derived from provider pricing data.
/// NULL when pricing was not configured at the time of the turn.
const ADD_TURN_COST_COLUMN: &str = "
    ALTER TABLE turns ADD COLUMN cost_microcents INTEGER;
";

/// Skill activation state per session.
///
/// Composite primary key on (session_id, skill_name) ensures each skill can
/// only appear once per session. Cascades on session delete so that removing
/// a session cleans up its active skills automatically.
const CREATE_ACTIVE_SKILLS: &str = "
    CREATE TABLE IF NOT EXISTS active_skills (
        session_id    TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
        skill_name    TEXT NOT NULL,
        activated_at  TEXT NOT NULL,
        PRIMARY KEY (session_id, skill_name)
    );
";

/// Full target schema produced in a single migration.
///
/// Replaces the previous incremental migrations (001–005). All tables are
/// created with their final columns and constraints; there is no incremental
/// ALTER TABLE patching required on a fresh database.
const COMPLETE_SCHEMA: &str = "
    -- Projects: name + slug uniqueness, optional workspace path.
    CREATE TABLE IF NOT EXISTS projects (
        id             TEXT NOT NULL PRIMARY KEY,
        name           TEXT NOT NULL UNIQUE,
        slug           TEXT NOT NULL DEFAULT '',
        workspace_path TEXT,
        created_at     TEXT NOT NULL,
        updated_at     TEXT NOT NULL
    );

    CREATE UNIQUE INDEX IF NOT EXISTS idx_projects_slug ON projects(slug);

    -- Sessions: belong to a project, track which agent owns the session.
    CREATE TABLE IF NOT EXISTS sessions (
        id         TEXT NOT NULL PRIMARY KEY,
        project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
        agent_id   TEXT NOT NULL,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL
    );

    CREATE INDEX IF NOT EXISTS idx_sessions_project_id ON sessions(project_id);
    CREATE INDEX IF NOT EXISTS idx_sessions_agent_id   ON sessions(agent_id);

    CREATE TABLE IF NOT EXISTS scheduled_jobs (
        id                  TEXT NOT NULL PRIMARY KEY,
        session_id          TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
        agent_id            TEXT NOT NULL,
        message             TEXT NOT NULL,
        status              TEXT NOT NULL,
        kind                TEXT NOT NULL,
        schedule_expression TEXT NOT NULL,
        delivery_json       TEXT NOT NULL,
        next_run_at         TEXT,
        last_run_at         TEXT,
        created_at          TEXT NOT NULL,
        updated_at          TEXT NOT NULL
    );

    CREATE INDEX IF NOT EXISTS idx_scheduled_jobs_next_run
        ON scheduled_jobs(status, next_run_at);
    CREATE INDEX IF NOT EXISTS idx_scheduled_jobs_session_id
        ON scheduled_jobs(session_id);

    CREATE TABLE IF NOT EXISTS scheduled_runs (
        id            TEXT NOT NULL PRIMARY KEY,
        job_id        TEXT NOT NULL REFERENCES scheduled_jobs(id) ON DELETE CASCADE,
        trigger_kind  TEXT NOT NULL,
        state         TEXT NOT NULL,
        scheduled_for TEXT,
        turn_id       TEXT,
        output_text   TEXT,
        error_message TEXT,
        created_at    TEXT NOT NULL,
        started_at    TEXT,
        completed_at  TEXT
    );

    CREATE INDEX IF NOT EXISTS idx_scheduled_runs_job_id
        ON scheduled_runs(job_id, created_at);

    CREATE TABLE IF NOT EXISTS heartbeats (
        id               TEXT NOT NULL PRIMARY KEY,
        session_id       TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
        agent_id         TEXT NOT NULL,
        message          TEXT NOT NULL,
        interval_seconds INTEGER NOT NULL,
        status           TEXT NOT NULL,
        last_beat_at     TEXT,
        next_beat_at     TEXT,
        created_at       TEXT NOT NULL,
        updated_at       TEXT NOT NULL
    );

    CREATE INDEX IF NOT EXISTS idx_heartbeats_next_beat
        ON heartbeats(status, next_beat_at);
    CREATE INDEX IF NOT EXISTS idx_heartbeats_session_id
        ON heartbeats(session_id);

    CREATE TABLE IF NOT EXISTS heartbeat_runs (
        id            TEXT NOT NULL PRIMARY KEY,
        heartbeat_id  TEXT NOT NULL REFERENCES heartbeats(id) ON DELETE CASCADE,
        state         TEXT NOT NULL,
        scheduled_for TEXT,
        turn_id       TEXT,
        output_text   TEXT,
        error_message TEXT,
        created_at    TEXT NOT NULL,
        started_at    TEXT,
        completed_at  TEXT
    );

    CREATE INDEX IF NOT EXISTS idx_heartbeat_runs_heartbeat_id
        ON heartbeat_runs(heartbeat_id, created_at);

    -- Threads: a conversation thread within a session, owned by an agent.
    -- State defaults to 'idle'; transitions are managed by the agent runtime.
    CREATE TABLE IF NOT EXISTS threads (
        id         TEXT NOT NULL PRIMARY KEY,
        session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
        agent_id   TEXT NOT NULL,
        state      TEXT NOT NULL DEFAULT 'idle',
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL
    );

    CREATE INDEX IF NOT EXISTS idx_threads_session_id ON threads(session_id);
    CREATE INDEX IF NOT EXISTS idx_threads_agent_id   ON threads(agent_id);

    -- Turns: a single exchange within a thread.
    -- State defaults to 'pending' (was 'in_progress' in the old schema).
    -- user_input / assistant_response / token counts replace the old
    -- messages-only approach for fast summary access.
    CREATE TABLE IF NOT EXISTS turns (
        id                 TEXT    NOT NULL PRIMARY KEY,
        thread_id          TEXT    NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
        agent_id           TEXT    NOT NULL,
        user_input         TEXT,
        assistant_response TEXT,
        input_tokens       INTEGER,
        output_tokens      INTEGER,
        state              TEXT    NOT NULL DEFAULT 'pending',
        created_at         TEXT    NOT NULL,
        completed_at       TEXT
    );

    CREATE INDEX IF NOT EXISTS idx_turns_thread_id ON turns(thread_id);
    CREATE INDEX IF NOT EXISTS idx_turns_agent_id  ON turns(agent_id);
    CREATE INDEX IF NOT EXISTS idx_turns_state     ON turns(state);

    -- Messages: individual messages within a thread.
    CREATE TABLE IF NOT EXISTS messages (
        id           TEXT    NOT NULL PRIMARY KEY,
        thread_id    TEXT    NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
        role         TEXT    NOT NULL,
        content      TEXT    NOT NULL,
        position     INTEGER NOT NULL,
        agent_id     TEXT    NOT NULL,
        tool_call_id TEXT,
        tool_name    TEXT,
        created_at   TEXT    NOT NULL
    );

    CREATE INDEX IF NOT EXISTS idx_messages_thread_id ON messages(thread_id);
    CREATE INDEX IF NOT EXISTS idx_messages_agent_id  ON messages(agent_id);
    CREATE UNIQUE INDEX IF NOT EXISTS idx_messages_thread_position
        ON messages(thread_id, position);

    -- Tool calls: execution records for a specific turn.
    -- parent_call_id supports nested / sub-tool invocations (NULL = top-level).
    CREATE TABLE IF NOT EXISTS tool_calls (
        id             TEXT    NOT NULL PRIMARY KEY,
        turn_id        TEXT    NOT NULL REFERENCES turns(id) ON DELETE CASCADE,
        call_id        TEXT    NOT NULL,
        tool           TEXT    NOT NULL,
        arguments      TEXT    NOT NULL,
        agent_id       TEXT    NOT NULL,
        result         TEXT,
        status         TEXT,
        duration_ms    INTEGER,
        truncated      INTEGER NOT NULL DEFAULT 0,
        total_bytes    INTEGER,
        position       INTEGER NOT NULL,
        parent_call_id TEXT,
        created_at     TEXT    NOT NULL
    );

    CREATE INDEX IF NOT EXISTS idx_tool_calls_turn_id  ON tool_calls(turn_id);
    CREATE INDEX IF NOT EXISTS idx_tool_calls_call_id  ON tool_calls(call_id);
    CREATE INDEX IF NOT EXISTS idx_tool_calls_agent_id ON tool_calls(agent_id);
";

/// Additive migration: validated structured output from schema validation.
/// JSON text column, nullable — only present when schema validation passed.
const ADD_TURN_STRUCTURED_OUTPUT_COLUMN: &str = "
    ALTER TABLE turns ADD COLUMN structured_output TEXT;
";

/// Additive migration: opaque host-managed session metadata stored as JSON.
const ADD_SESSION_METADATA_COLUMN: &str = "
    ALTER TABLE sessions ADD COLUMN metadata_json TEXT;
";

/// Scheduled jobs and their recorded executions.
const CREATE_SCHEDULED_JOB_TABLES: &str = "
    CREATE TABLE IF NOT EXISTS scheduled_jobs (
        id                  TEXT NOT NULL PRIMARY KEY,
        session_id          TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
        agent_id            TEXT NOT NULL,
        message             TEXT NOT NULL,
        status              TEXT NOT NULL,
        kind                TEXT NOT NULL,
        schedule_expression TEXT NOT NULL,
        delivery_json       TEXT NOT NULL,
        next_run_at         TEXT,
        last_run_at         TEXT,
        created_at          TEXT NOT NULL,
        updated_at          TEXT NOT NULL
    );

    CREATE INDEX IF NOT EXISTS idx_scheduled_jobs_next_run
        ON scheduled_jobs(status, next_run_at);
    CREATE INDEX IF NOT EXISTS idx_scheduled_jobs_session_id
        ON scheduled_jobs(session_id);

    CREATE TABLE IF NOT EXISTS scheduled_runs (
        id            TEXT NOT NULL PRIMARY KEY,
        job_id        TEXT NOT NULL REFERENCES scheduled_jobs(id) ON DELETE CASCADE,
        trigger_kind  TEXT NOT NULL,
        state         TEXT NOT NULL,
        scheduled_for TEXT,
        turn_id       TEXT,
        output_text   TEXT,
        error_message TEXT,
        created_at    TEXT NOT NULL,
        started_at    TEXT,
        completed_at  TEXT
    );

    CREATE INDEX IF NOT EXISTS idx_scheduled_runs_job_id
        ON scheduled_runs(job_id, created_at);
";

/// Additive migration: durable heartbeat configuration and run history.
const CREATE_HEARTBEAT_TABLES: &str = "
    CREATE TABLE IF NOT EXISTS heartbeats (
        id               TEXT NOT NULL PRIMARY KEY,
        session_id       TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
        agent_id         TEXT NOT NULL,
        message          TEXT NOT NULL,
        interval_seconds INTEGER NOT NULL,
        status           TEXT NOT NULL,
        last_beat_at     TEXT,
        next_beat_at     TEXT,
        created_at       TEXT NOT NULL,
        updated_at       TEXT NOT NULL
    );

    CREATE INDEX IF NOT EXISTS idx_heartbeats_next_beat
        ON heartbeats(status, next_beat_at);
    CREATE INDEX IF NOT EXISTS idx_heartbeats_session_id
        ON heartbeats(session_id);

    CREATE TABLE IF NOT EXISTS heartbeat_runs (
        id            TEXT NOT NULL PRIMARY KEY,
        heartbeat_id  TEXT NOT NULL REFERENCES heartbeats(id) ON DELETE CASCADE,
        state         TEXT NOT NULL,
        scheduled_for TEXT,
        turn_id       TEXT,
        output_text   TEXT,
        error_message TEXT,
        created_at    TEXT NOT NULL,
        started_at    TEXT,
        completed_at  TEXT
    );

    CREATE INDEX IF NOT EXISTS idx_heartbeat_runs_heartbeat_id
        ON heartbeat_runs(heartbeat_id, created_at);
";

/// Approval requests — one row per HITL approval gate triggered during a turn.
///
/// `kind` is "tool" or "hook", selecting which of the optional `tool_*` /
/// `hook_name` fields are populated. `state` tracks the lifecycle: pending →
/// approved | rejected | expired. `timeout_policy` records the configured
/// behaviour for timeout: "interrupt" or "allow".
///
/// Cascades on turn delete so that incomplete approvals are cleaned up
/// automatically when the owning turn is purged.
const CREATE_APPROVAL_REQUESTS: &str = "
    CREATE TABLE IF NOT EXISTS approval_requests (
        id              TEXT NOT NULL PRIMARY KEY,
        turn_id         TEXT NOT NULL REFERENCES turns(id) ON DELETE CASCADE,
        agent_id        TEXT NOT NULL,
        kind            TEXT NOT NULL,
        tool_name       TEXT,
        tool_arguments  TEXT,
        hook_name       TEXT,
        prompt          TEXT NOT NULL,
        context         TEXT,
        state           TEXT NOT NULL DEFAULT 'pending',
        timeout_at      TEXT NOT NULL,
        timeout_policy  TEXT NOT NULL,
        decision_reason TEXT,
        created_at      TEXT NOT NULL,
        decided_at      TEXT
    );

    CREATE INDEX IF NOT EXISTS idx_approval_requests_turn_id  ON approval_requests(turn_id);
    CREATE INDEX IF NOT EXISTS idx_approval_requests_agent_id ON approval_requests(agent_id);
    CREATE INDEX IF NOT EXISTS idx_approval_requests_state    ON approval_requests(state);
";

/// Inter-agent message mailbox — store-and-forward for durable message kinds.
const CREATE_AGENT_MESSAGES: &str = "
    CREATE TABLE IF NOT EXISTS agent_messages (
        id                  TEXT PRIMARY KEY,
        sender_agent_id     TEXT NOT NULL,
        recipient_agent_id  TEXT NOT NULL,
        kind                TEXT NOT NULL,
        correlation_id      TEXT,
        payload             TEXT NOT NULL,
        status              TEXT NOT NULL DEFAULT 'pending',
        failure_reason      TEXT,
        created_at          TEXT NOT NULL,
        delivered_at        TEXT
    );

    CREATE INDEX IF NOT EXISTS idx_agent_messages_recipient_status
        ON agent_messages(recipient_agent_id, status);
    CREATE INDEX IF NOT EXISTS idx_agent_messages_correlation
        ON agent_messages(correlation_id);
    CREATE INDEX IF NOT EXISTS idx_agent_messages_sender
        ON agent_messages(sender_agent_id);
";

/// Additive migration: preserve message-to-turn attribution for honest reads.
const ADD_MESSAGE_TURN_ID: &str = "
    ALTER TABLE messages ADD COLUMN turn_id TEXT REFERENCES turns(id) ON DELETE SET NULL;

    CREATE INDEX IF NOT EXISTS idx_messages_turn_id ON messages(turn_id);
";

/// Canonical inbound audit metadata: final canonical input plus modifying hooks.
const ADD_TURN_INBOUND_AUDIT: &str = "
    ALTER TABLE turns ADD COLUMN inbound_audit TEXT;
";

/// Structural default-project identity.
///
/// The default project is no longer identified by the mutable display name.
const ADD_PROJECT_DEFAULT_FLAG: &str = "
    ALTER TABLE projects ADD COLUMN is_default INTEGER NOT NULL DEFAULT 0;
    UPDATE projects
    SET is_default = 1
    WHERE id = (
        SELECT id
        FROM projects
        WHERE name = 'default'
        ORDER BY created_at ASC
        LIMIT 1
    );
    CREATE UNIQUE INDEX IF NOT EXISTS idx_projects_single_default
        ON projects(is_default) WHERE is_default = 1;
";

/// Delegation chain tracking — nullable parent reference on turns.
const ADD_TURN_PARENT_TURN_ID: &str = "
    ALTER TABLE turns ADD COLUMN parent_turn_id TEXT;

    CREATE INDEX IF NOT EXISTS idx_turns_parent_turn_id
        ON turns(parent_turn_id);
";

/// Teams: the organisational unit for multi-agent orchestration.
///
/// A team belongs to a project and defines an entry agent (receives user
/// messages) plus a member list. Teams link to topology definitions and
/// are the scope boundary for budget controls.
const CREATE_TEAMS: &str = "
    CREATE TABLE IF NOT EXISTS teams (
        id             TEXT NOT NULL PRIMARY KEY,
        project_id     TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
        name           TEXT NOT NULL,
        description    TEXT NOT NULL DEFAULT '',
        entry_agent_id TEXT NOT NULL,
        members        TEXT NOT NULL DEFAULT '[]',
        created_at     TEXT NOT NULL,
        updated_at     TEXT NOT NULL,
        UNIQUE(project_id, name)
    );

    CREATE INDEX IF NOT EXISTS idx_teams_project_id ON teams(project_id);
";

/// Topology runs and execution traces for multi-agent team orchestration.
const CREATE_TOPOLOGY_TRACE: &str = "
    CREATE TABLE IF NOT EXISTS topology_runs (
        id               TEXT NOT NULL PRIMARY KEY,
        team_id          TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
        session_id       TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
        source_turn_id   TEXT REFERENCES turns(id) ON DELETE SET NULL,
        state            TEXT NOT NULL DEFAULT 'pending',
        final_output_json TEXT,
        error_message    TEXT,
        created_at       TEXT NOT NULL,
        completed_at     TEXT
    );

    CREATE INDEX IF NOT EXISTS idx_topology_runs_team_id ON topology_runs(team_id);
    CREATE INDEX IF NOT EXISTS idx_topology_runs_session_id ON topology_runs(session_id);
    CREATE INDEX IF NOT EXISTS idx_topology_runs_source_turn_id ON topology_runs(source_turn_id);

    CREATE TABLE IF NOT EXISTS node_executions (
        id               TEXT NOT NULL PRIMARY KEY,
        topology_run_id  TEXT NOT NULL REFERENCES topology_runs(id) ON DELETE CASCADE,
        node_id          TEXT NOT NULL,
        node_kind        TEXT NOT NULL,
        agent_id         TEXT,
        task_name        TEXT,
        state            TEXT NOT NULL DEFAULT 'pending',
        input_json       TEXT,
        output_json      TEXT,
        error_message    TEXT,
        created_at       TEXT NOT NULL,
        completed_at     TEXT
    );

    CREATE INDEX IF NOT EXISTS idx_node_executions_run_id ON node_executions(topology_run_id);
    CREATE INDEX IF NOT EXISTS idx_node_executions_run_node_id ON node_executions(topology_run_id, node_id);

    CREATE TABLE IF NOT EXISTS edge_evaluations (
        id                       TEXT NOT NULL PRIMARY KEY,
        topology_run_id          TEXT NOT NULL REFERENCES topology_runs(id) ON DELETE CASCADE,
        source_node_execution_id TEXT NOT NULL REFERENCES node_executions(id) ON DELETE CASCADE,
        edge_id                  TEXT NOT NULL,
        target_node_id           TEXT NOT NULL,
        condition_expression     TEXT,
        matched                  INTEGER NOT NULL,
        payload_json             TEXT,
        created_at               TEXT NOT NULL
    );

    CREATE INDEX IF NOT EXISTS idx_edge_evaluations_run_id ON edge_evaluations(topology_run_id);
    CREATE INDEX IF NOT EXISTS idx_edge_evaluations_source_execution_id ON edge_evaluations(source_node_execution_id);

    CREATE TABLE IF NOT EXISTS node_retries (
        id                 TEXT NOT NULL PRIMARY KEY,
        node_execution_id  TEXT NOT NULL REFERENCES node_executions(id) ON DELETE CASCADE,
        attempt_number     INTEGER NOT NULL,
        failure_kind       TEXT NOT NULL,
        error_message      TEXT NOT NULL,
        recovery_guidance  TEXT,
        created_at         TEXT NOT NULL
    );

    CREATE INDEX IF NOT EXISTS idx_node_retries_execution_id ON node_retries(node_execution_id);
";

/// Session-scoped memory for approved tool requests.
const CREATE_APPROVAL_MEMORY: &str = "
    CREATE TABLE IF NOT EXISTS approval_memory (
        session_id           TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
        tool_name            TEXT NOT NULL,
        request_fingerprint  TEXT NOT NULL,
        created_at           TEXT NOT NULL,
        PRIMARY KEY (session_id, tool_name, request_fingerprint)
    );

    CREATE INDEX IF NOT EXISTS idx_approval_memory_session_id ON approval_memory(session_id);
";

/// Branch lineage for conversation threads.
const CREATE_THREAD_BRANCHES: &str = "
    CREATE TABLE IF NOT EXISTS thread_branches (
        child_thread_id          TEXT NOT NULL PRIMARY KEY REFERENCES threads(id) ON DELETE CASCADE,
        parent_thread_id         TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
        branched_from_message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
        created_at               TEXT NOT NULL
    );

    CREATE INDEX IF NOT EXISTS idx_thread_branches_parent_thread_id ON thread_branches(parent_thread_id);
";

/// Apply all pending migrations to `conn`.
///
/// Migrations are tracked by name in `_swage_migrations`. Already-applied
/// migrations are skipped, so this is safe to call on every startup.
pub fn run_migrations(conn: &Connection) -> Result<(), StorageError> {
    // Ensure the tracking table exists first.
    conn.execute_batch(CREATE_MIGRATIONS_TABLE)
        .map_err(|e| StorageError::MigrationFailed(e.to_string()))?;

    for (name, sql) in migrations() {
        // Check whether this migration has already been applied.
        let already_applied: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM _swage_migrations WHERE name = ?1",
                params![name],
                |row| row.get::<_, i64>(0),
            )
            .map(|count| count > 0)
            .map_err(|e| StorageError::MigrationFailed(e.to_string()))?;

        if already_applied {
            continue;
        }

        // Apply migration inside a transaction for atomicity.
        conn.execute_batch(&format!("BEGIN; {sql} COMMIT;"))
            .map_err(|e| {
                StorageError::MigrationFailed(format!("migration '{name}' failed: {e}"))
            })?;

        conn.execute(
            "INSERT INTO _swage_migrations (name, applied_at) VALUES (?1, datetime('now'))",
            params![name],
        )
        .map_err(|e| StorageError::MigrationFailed(e.to_string()))?;
    }

    // Seed the default project if it does not already exist.
    seed_default_project(conn)
}

/// Insert the default project on first run. Idempotent via INSERT OR IGNORE.
fn seed_default_project(conn: &Connection) -> Result<(), StorageError> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();

    conn.execute(
        "INSERT OR IGNORE INTO projects (id, name, is_default, slug, workspace_path, created_at, updated_at)
         VALUES (?1, 'default', 1, 'default', NULL, ?2, ?2)",
        params![id, now],
    )
    .map_err(|e| StorageError::MigrationFailed(e.to_string()))?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    fn open_migrated() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory DB");
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        super::run_migrations(&conn).expect("migrations should succeed");
        conn
    }

    // Convenience: insert a session using the new schema (requires agent_id).
    fn insert_session(conn: &Connection, project_id: &str, now: &str) -> String {
        let session_id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO sessions (id, project_id, agent_id, created_at, updated_at)
             VALUES (?1,?2,'agent-test',?3,?3)",
            rusqlite::params![session_id, project_id, now],
        )
        .expect("insert session");
        session_id
    }

    // Convenience: insert a thread under a session.
    fn insert_thread(conn: &Connection, session_id: &str, now: &str) -> String {
        let thread_id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO threads (id, session_id, agent_id, created_at, updated_at)
             VALUES (?1,?2,'agent-test',?3,?3)",
            rusqlite::params![thread_id, session_id, now],
        )
        .expect("insert thread");
        thread_id
    }

    // Convenience: insert a turn under a thread.
    fn insert_turn(conn: &Connection, thread_id: &str, now: &str) -> String {
        let turn_id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO turns (id, thread_id, agent_id, state, created_at)
             VALUES (?1,?2,'agent-test','pending',?3)",
            rusqlite::params![turn_id, thread_id, now],
        )
        .expect("insert turn");
        turn_id
    }

    fn default_project_id(conn: &Connection) -> String {
        conn.query_row("SELECT id FROM projects WHERE name='default'", [], |r| {
            r.get(0)
        })
        .expect("default project")
    }

    // ---------------------------------------------------------------------------
    // Idempotency and basic table existence
    // ---------------------------------------------------------------------------

    #[test]
    fn tool_calls_table_created() {
        let conn = open_migrated();
        // Querying the table is the cleanest proof it exists.
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM tool_calls", [], |r| r.get(0))
            .expect("tool_calls table should exist after migration");
        assert_eq!(count, 0, "fresh DB should have no rows");
    }

    #[test]
    fn tool_calls_migration_idempotent() {
        let conn = open_migrated();
        // Running migrations a second time must not error (already-applied guard).
        super::run_migrations(&conn).expect("second migration run should be idempotent");
        // Table should still be queryable.
        let _: i64 = conn
            .query_row("SELECT COUNT(*) FROM tool_calls", [], |r| r.get(0))
            .expect("table should still exist");
    }

    #[test]
    fn existing_data_unaffected_after_migration() {
        // Insert data into the tables, then re-run migrations; data must survive.
        let conn = open_migrated();
        let project_id = default_project_id(&conn);
        let now = chrono::Utc::now().to_rfc3339();

        let session_id = insert_session(&conn, &project_id, &now);
        let thread_id = insert_thread(&conn, &session_id, &now);
        let _turn_id = insert_turn(&conn, &thread_id, &now);

        // Re-run migrations (idempotent).
        super::run_migrations(&conn).expect("re-run should succeed");

        let session_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
            .expect("sessions query");
        assert_eq!(session_count, 1, "session should survive migration re-run");

        let turn_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM turns", [], |r| r.get(0))
            .expect("turns query");
        assert_eq!(turn_count, 1, "turn should survive migration re-run");
    }

    // ---------------------------------------------------------------------------
    // Index checks
    // ---------------------------------------------------------------------------

    #[test]
    fn tool_calls_turn_id_index_exists() {
        let conn = open_migrated();
        let idx_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_tool_calls_turn_id'",
                [],
                |r| r.get(0),
            )
            .expect("schema query");
        assert_eq!(idx_count, 1, "idx_tool_calls_turn_id index should exist");
    }

    #[test]
    fn tool_calls_call_id_index_exists() {
        let conn = open_migrated();
        let idx_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_tool_calls_call_id'",
                [],
                |r| r.get(0),
            )
            .expect("schema query");
        assert_eq!(idx_count, 1, "idx_tool_calls_call_id index should exist");
    }

    // ---------------------------------------------------------------------------
    // Column existence: tool_calls
    // ---------------------------------------------------------------------------

    #[test]
    fn tool_calls_parent_call_id_column_exists() {
        let conn = open_migrated();
        let col_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('tool_calls') WHERE name='parent_call_id'",
                [],
                |r| r.get(0),
            )
            .expect("pragma_table_info query");
        assert_eq!(col_count, 1, "parent_call_id column should exist");
    }

    #[test]
    fn tool_calls_parent_call_id_nullable() {
        // Existing rows should have NULL parent_call_id when not set.
        let conn = open_migrated();
        let project_id = default_project_id(&conn);
        let now = chrono::Utc::now().to_rfc3339();

        let session_id = insert_session(&conn, &project_id, &now);
        let thread_id = insert_thread(&conn, &session_id, &now);
        let turn_id = insert_turn(&conn, &thread_id, &now);

        // Insert without parent_call_id — should default to NULL.
        let call_id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO tool_calls
             (id, turn_id, call_id, tool, arguments, agent_id, truncated, position, created_at)
             VALUES (?1,?2,?3,'shell','{}','agent-test',0,0,?4)",
            rusqlite::params![uuid::Uuid::new_v4().to_string(), turn_id, call_id, now],
        )
        .expect("insert without parent_call_id");

        let parent: Option<String> = conn
            .query_row(
                "SELECT parent_call_id FROM tool_calls WHERE call_id = ?1",
                rusqlite::params![call_id],
                |r| r.get(0),
            )
            .expect("select parent_call_id");
        assert!(
            parent.is_none(),
            "parent_call_id should be NULL for native calls"
        );
    }

    // ---------------------------------------------------------------------------
    // Projects: slug column
    // ---------------------------------------------------------------------------

    #[test]
    fn slug_column_exists_after_migration() {
        let conn = open_migrated();
        let col_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('projects') WHERE name='slug'",
                [],
                |r| r.get(0),
            )
            .expect("pragma_table_info query");
        assert_eq!(col_count, 1, "slug column should exist after migration");
    }

    #[test]
    fn default_project_has_slug_default() {
        let conn = open_migrated();
        let slug: String = conn
            .query_row(
                "SELECT slug FROM projects WHERE name = 'default'",
                [],
                |r| r.get(0),
            )
            .expect("default project should exist with slug");
        assert_eq!(slug, "default", "default project slug should be 'default'");
    }

    #[test]
    fn slug_uniqueness_constraint_enforced() {
        let conn = open_migrated();
        let now = chrono::Utc::now().to_rfc3339();

        conn.execute(
            "INSERT INTO projects (id, name, slug, workspace_path, created_at, updated_at)
             VALUES (?1, 'alpha', 'alpha', NULL, ?2, ?2)",
            rusqlite::params![uuid::Uuid::new_v4().to_string(), now],
        )
        .expect("first insert should succeed");

        let result = conn.execute(
            "INSERT INTO projects (id, name, slug, workspace_path, created_at, updated_at)
             VALUES (?1, 'beta', 'alpha', NULL, ?2, ?2)",
            rusqlite::params![uuid::Uuid::new_v4().to_string(), now],
        );
        assert!(
            result.is_err(),
            "duplicate slug should violate the unique index"
        );
    }

    #[test]
    fn slug_migration_idempotent() {
        let conn = open_migrated();
        super::run_migrations(&conn).expect("second migration run should be idempotent");
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('projects') WHERE name='slug'",
                [],
                |r| r.get(0),
            )
            .expect("pragma_table_info query after idempotent re-run");
        assert_eq!(
            count, 1,
            "slug column should still exist after idempotent re-run"
        );
    }

    // ---------------------------------------------------------------------------
    // New-schema tests: threads table, agent_id columns, updated FK chains.
    // ---------------------------------------------------------------------------

    #[test]
    fn threads_table_created() {
        let conn = open_migrated();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM threads", [], |r| r.get(0))
            .expect("threads table should exist after migration");
        assert_eq!(count, 0, "fresh DB should have no thread rows");
    }

    #[test]
    fn sessions_agent_id_column_exists() {
        let conn = open_migrated();
        let col_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('sessions') WHERE name='agent_id'",
                [],
                |r| r.get(0),
            )
            .expect("pragma_table_info query");
        assert_eq!(col_count, 1, "sessions.agent_id column should exist");
    }

    #[test]
    fn sessions_agent_id_index_exists() {
        let conn = open_migrated();
        let idx_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_sessions_agent_id'",
                [],
                |r| r.get(0),
            )
            .expect("schema query");
        assert_eq!(idx_count, 1, "idx_sessions_agent_id index should exist");
    }

    #[test]
    fn threads_agent_id_column_exists() {
        let conn = open_migrated();
        let col_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('threads') WHERE name='agent_id'",
                [],
                |r| r.get(0),
            )
            .expect("pragma_table_info query");
        assert_eq!(col_count, 1, "threads.agent_id column should exist");
    }

    #[test]
    fn threads_state_column_exists_with_idle_default() {
        let conn = open_migrated();
        // Verify column is present.
        let col_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('threads') WHERE name='state'",
                [],
                |r| r.get(0),
            )
            .expect("pragma_table_info query");
        assert_eq!(col_count, 1, "threads.state column should exist");

        let project_id = default_project_id(&conn);
        let now = chrono::Utc::now().to_rfc3339();
        let session_id = insert_session(&conn, &project_id, &now);

        // Insert a thread without specifying state — should default to 'idle'.
        let thread_id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO threads (id, session_id, agent_id, created_at, updated_at)
             VALUES (?1,?2,'agent-test',?3,?3)",
            rusqlite::params![thread_id, session_id, now],
        )
        .expect("insert thread without explicit state");

        let state: String = conn
            .query_row(
                "SELECT state FROM threads WHERE id=?1",
                rusqlite::params![thread_id],
                |r| r.get(0),
            )
            .expect("query thread state");
        assert_eq!(state, "idle", "threads.state should default to 'idle'");
    }

    #[test]
    fn turns_thread_id_column_exists() {
        let conn = open_migrated();
        let col_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('turns') WHERE name='thread_id'",
                [],
                |r| r.get(0),
            )
            .expect("pragma_table_info query");
        assert_eq!(col_count, 1, "turns.thread_id column should exist");
    }

    #[test]
    fn turns_agent_id_column_exists() {
        let conn = open_migrated();
        let col_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('turns') WHERE name='agent_id'",
                [],
                |r| r.get(0),
            )
            .expect("pragma_table_info query");
        assert_eq!(col_count, 1, "turns.agent_id column should exist");
    }

    #[test]
    fn turns_state_defaults_to_pending() {
        let conn = open_migrated();
        let project_id = default_project_id(&conn);
        let now = chrono::Utc::now().to_rfc3339();
        let session_id = insert_session(&conn, &project_id, &now);
        let thread_id = insert_thread(&conn, &session_id, &now);

        // Insert a turn without specifying state — should default to 'pending'.
        let turn_id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO turns (id, thread_id, agent_id, created_at)
             VALUES (?1,?2,'agent-test',?3)",
            rusqlite::params![turn_id, thread_id, now],
        )
        .expect("insert turn without explicit state");

        let state: String = conn
            .query_row(
                "SELECT state FROM turns WHERE id=?1",
                rusqlite::params![turn_id],
                |r| r.get(0),
            )
            .expect("query turn state");
        assert_eq!(state, "pending", "turns.state should default to 'pending'");
    }

    #[test]
    fn messages_thread_id_column_exists() {
        let conn = open_migrated();
        let col_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('messages') WHERE name='thread_id'",
                [],
                |r| r.get(0),
            )
            .expect("pragma_table_info query");
        assert_eq!(col_count, 1, "messages.thread_id column should exist");
    }

    #[test]
    fn messages_agent_id_column_exists() {
        let conn = open_migrated();
        let col_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('messages') WHERE name='agent_id'",
                [],
                |r| r.get(0),
            )
            .expect("pragma_table_info query");
        assert_eq!(col_count, 1, "messages.agent_id column should exist");
    }

    #[test]
    fn messages_tool_call_id_column_exists() {
        let conn = open_migrated();
        let col_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('messages') WHERE name='tool_call_id'",
                [],
                |r| r.get(0),
            )
            .expect("pragma_table_info query");
        assert_eq!(col_count, 1, "messages.tool_call_id column should exist");
    }

    #[test]
    fn messages_tool_name_column_exists() {
        let conn = open_migrated();
        let col_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('messages') WHERE name='tool_name'",
                [],
                |r| r.get(0),
            )
            .expect("pragma_table_info query");
        assert_eq!(col_count, 1, "messages.tool_name column should exist");
    }

    #[test]
    fn tool_calls_agent_id_column_exists() {
        let conn = open_migrated();
        let col_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('tool_calls') WHERE name='agent_id'",
                [],
                |r| r.get(0),
            )
            .expect("pragma_table_info query");
        assert_eq!(col_count, 1, "tool_calls.agent_id column should exist");
    }

    #[test]
    fn threads_indices_exist() {
        let conn = open_migrated();
        for idx in &["idx_threads_session_id", "idx_threads_agent_id"] {
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name=?1",
                    rusqlite::params![idx],
                    |r| r.get(0),
                )
                .expect("schema query");
            assert_eq!(count, 1, "{idx} should exist");
        }
    }

    // ---------------------------------------------------------------------------
    // Cascade delete tests for the full FK chain
    // ---------------------------------------------------------------------------

    #[test]
    fn cascade_session_delete_removes_threads() {
        let conn = open_migrated();
        let project_id = default_project_id(&conn);
        let now = chrono::Utc::now().to_rfc3339();
        let session_id = insert_session(&conn, &project_id, &now);
        let _thread_id = insert_thread(&conn, &session_id, &now);

        conn.execute(
            "DELETE FROM sessions WHERE id=?1",
            rusqlite::params![session_id],
        )
        .expect("delete session");

        let remaining: i64 = conn
            .query_row("SELECT COUNT(*) FROM threads", [], |r| r.get(0))
            .expect("count after cascade");
        assert_eq!(
            remaining, 0,
            "threads should cascade-delete when session is deleted"
        );
    }

    #[test]
    fn cascade_thread_delete_removes_turns() {
        let conn = open_migrated();
        let project_id = default_project_id(&conn);
        let now = chrono::Utc::now().to_rfc3339();
        let session_id = insert_session(&conn, &project_id, &now);
        let thread_id = insert_thread(&conn, &session_id, &now);
        let _turn_id = insert_turn(&conn, &thread_id, &now);

        conn.execute(
            "DELETE FROM threads WHERE id=?1",
            rusqlite::params![thread_id],
        )
        .expect("delete thread");

        let remaining: i64 = conn
            .query_row("SELECT COUNT(*) FROM turns", [], |r| r.get(0))
            .expect("count after cascade");
        assert_eq!(
            remaining, 0,
            "turns should cascade-delete when thread is deleted"
        );
    }

    #[test]
    fn cascade_thread_delete_removes_messages() {
        let conn = open_migrated();
        let project_id = default_project_id(&conn);
        let now = chrono::Utc::now().to_rfc3339();
        let session_id = insert_session(&conn, &project_id, &now);
        let thread_id = insert_thread(&conn, &session_id, &now);

        conn.execute(
            "INSERT INTO messages (id, thread_id, role, content, position, agent_id, created_at)
             VALUES (?1,?2,'user','hello',0,'agent-test',?3)",
            rusqlite::params![uuid::Uuid::new_v4().to_string(), thread_id, now],
        )
        .unwrap();

        conn.execute(
            "DELETE FROM threads WHERE id=?1",
            rusqlite::params![thread_id],
        )
        .expect("delete thread");

        let remaining: i64 = conn
            .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
            .expect("count after cascade");
        assert_eq!(
            remaining, 0,
            "messages should cascade-delete when thread is deleted"
        );
    }

    // ---------------------------------------------------------------------------
    // context_snapshots table (002_context_snapshots migration)
    // ---------------------------------------------------------------------------

    #[test]
    fn context_snapshots_table_created() {
        let conn = open_migrated();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM context_snapshots", [], |r| r.get(0))
            .expect("context_snapshots table should exist after migration");
        assert_eq!(count, 0, "fresh DB should have no snapshot rows");
    }

    #[test]
    fn context_snapshots_cascade_delete_with_turn() {
        let conn = open_migrated();
        let project_id = default_project_id(&conn);
        let now = chrono::Utc::now().to_rfc3339();
        let session_id = insert_session(&conn, &project_id, &now);
        let thread_id = insert_thread(&conn, &session_id, &now);
        let turn_id = insert_turn(&conn, &thread_id, &now);

        // Insert a snapshot for the turn.
        conn.execute(
            "INSERT INTO context_snapshots (turn_id, report_json, created_at)
             VALUES (?1, ?2, ?3)",
            rusqlite::params![turn_id, r#"{"total_tokens":100}"#, now],
        )
        .expect("insert snapshot");

        // Deleting the turn should cascade to context_snapshots.
        conn.execute("DELETE FROM turns WHERE id=?1", rusqlite::params![turn_id])
            .expect("delete turn");

        let remaining: i64 = conn
            .query_row("SELECT COUNT(*) FROM context_snapshots", [], |r| r.get(0))
            .expect("count after cascade");
        assert_eq!(
            remaining, 0,
            "context_snapshots should be deleted via turn cascade"
        );
    }

    // ---------------------------------------------------------------------------
    // cost_microcents column (003_turn_cost migration)
    // ---------------------------------------------------------------------------

    #[test]
    fn turns_cost_microcents_column_exists() {
        let conn = open_migrated();
        let col_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('turns') WHERE name='cost_microcents'",
                [],
                |r| r.get(0),
            )
            .expect("pragma_table_info query");
        assert_eq!(
            col_count, 1,
            "turns.cost_microcents column should exist after migration"
        );
    }

    #[test]
    fn turns_cost_microcents_is_nullable() {
        let conn = open_migrated();
        let project_id = default_project_id(&conn);
        let now = chrono::Utc::now().to_rfc3339();
        let session_id = insert_session(&conn, &project_id, &now);
        let thread_id = insert_thread(&conn, &session_id, &now);

        // Insert a turn without cost — should default to NULL.
        let turn_id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO turns (id, thread_id, agent_id, state, created_at)
             VALUES (?1,?2,'agent-test','pending',?3)",
            rusqlite::params![turn_id, thread_id, now],
        )
        .expect("insert turn");

        let cost: Option<i64> = conn
            .query_row(
                "SELECT cost_microcents FROM turns WHERE id=?1",
                rusqlite::params![turn_id],
                |r| r.get(0),
            )
            .expect("select cost_microcents");
        assert!(cost.is_none(), "cost_microcents should be NULL by default");
    }

    // ---------------------------------------------------------------------------
    // approval_requests table (006_approval_requests migration)
    // ---------------------------------------------------------------------------

    #[test]
    fn approval_requests_table_created() {
        let conn = open_migrated();
        // SELECT COUNT(*) is the mote's stated success criterion.
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM approval_requests", [], |r| r.get(0))
            .expect("approval_requests table should exist after migration");
        assert_eq!(count, 0, "fresh DB should have no approval request rows");
    }

    #[test]
    fn approval_requests_cascade_delete_on_turn_delete() {
        let conn = open_migrated();
        let project_id = default_project_id(&conn);
        let now = chrono::Utc::now().to_rfc3339();
        let session_id = insert_session(&conn, &project_id, &now);
        let thread_id = insert_thread(&conn, &session_id, &now);
        let turn_id = insert_turn(&conn, &thread_id, &now);

        // Insert a minimal approval request referencing the turn.
        conn.execute(
            "INSERT INTO approval_requests
             (id, turn_id, agent_id, kind, prompt, state, timeout_at, timeout_policy, created_at)
             VALUES (?1,?2,'agent-test','tool','Approve shell?','pending',?3,'interrupt',?3)",
            rusqlite::params![uuid::Uuid::new_v4().to_string(), turn_id, now],
        )
        .expect("insert approval request");

        // Deleting the turn should cascade to approval_requests.
        conn.execute("DELETE FROM turns WHERE id=?1", rusqlite::params![turn_id])
            .expect("delete turn");

        let remaining: i64 = conn
            .query_row("SELECT COUNT(*) FROM approval_requests", [], |r| r.get(0))
            .expect("count after cascade");
        assert_eq!(
            remaining, 0,
            "approval_requests should cascade-delete with the turn"
        );
    }

    #[test]
    fn approval_requests_indices_exist() {
        let conn = open_migrated();
        for idx in &[
            "idx_approval_requests_turn_id",
            "idx_approval_requests_agent_id",
            "idx_approval_requests_state",
        ] {
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name=?1",
                    rusqlite::params![idx],
                    |r| r.get(0),
                )
                .expect("schema query");
            assert_eq!(count, 1, "{idx} should exist after migration");
        }
    }

    #[test]
    fn tool_calls_foreign_key_cascade_on_turn_delete() {
        let conn = open_migrated();
        let project_id = default_project_id(&conn);
        let now = chrono::Utc::now().to_rfc3339();

        let session_id = insert_session(&conn, &project_id, &now);
        let thread_id = insert_thread(&conn, &session_id, &now);
        let turn_id = insert_turn(&conn, &thread_id, &now);

        // Insert a tool call referencing the turn.
        let call_id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO tool_calls
             (id, turn_id, call_id, tool, arguments, agent_id, truncated, position, created_at)
             VALUES (?1,?2,?3,'shell','{\"cmd\":\"ls\"}','agent-test',0,0,?4)",
            rusqlite::params![uuid::Uuid::new_v4().to_string(), turn_id, call_id, now],
        )
        .expect("insert tool call");

        // Deleting the turn should cascade to tool_calls.
        conn.execute("DELETE FROM turns WHERE id=?1", rusqlite::params![turn_id])
            .expect("delete turn");

        let remaining: i64 = conn
            .query_row("SELECT COUNT(*) FROM tool_calls", [], |r| r.get(0))
            .expect("count after cascade");
        assert_eq!(remaining, 0, "tool_calls should be deleted via cascade");
    }

    #[test]
    fn topology_runs_table_created() {
        let conn = open_migrated();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM topology_runs", [], |r| r.get(0))
            .expect("topology_runs table should exist after migration");
        assert_eq!(count, 0);
    }

    #[test]
    fn node_executions_table_created() {
        let conn = open_migrated();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM node_executions", [], |r| r.get(0))
            .expect("node_executions table should exist after migration");
        assert_eq!(count, 0);
    }

    #[test]
    fn edge_evaluations_table_created() {
        let conn = open_migrated();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM edge_evaluations", [], |r| r.get(0))
            .expect("edge_evaluations table should exist after migration");
        assert_eq!(count, 0);
    }

    #[test]
    fn node_retries_table_created() {
        let conn = open_migrated();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM node_retries", [], |r| r.get(0))
            .expect("node_retries table should exist after migration");
        assert_eq!(count, 0);
    }

    #[test]
    fn approval_memory_table_created() {
        let conn = open_migrated();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM approval_memory", [], |r| r.get(0))
            .expect("approval_memory table should exist after migration");
        assert_eq!(count, 0);
    }

    #[test]
    fn topology_run_cascade_delete_removes_trace_rows() {
        let conn = open_migrated();
        let project_id = default_project_id(&conn);
        let now = chrono::Utc::now().to_rfc3339();
        let session_id = insert_session(&conn, &project_id, &now);
        let team_id = uuid::Uuid::new_v4().to_string();

        conn.execute(
            "INSERT INTO teams (id, project_id, name, description, entry_agent_id, members, created_at, updated_at)
             VALUES (?1, ?2, 'trace-team', '', 'planner', '[\"planner\"]', ?3, ?3)",
            rusqlite::params![team_id, project_id, now],
        )
        .expect("insert team");

        let run_id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO topology_runs (id, team_id, session_id, state, created_at)
             VALUES (?1, ?2, ?3, 'running', ?4)",
            rusqlite::params![run_id, team_id, session_id, now],
        )
        .expect("insert topology run");

        let execution_id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO node_executions (id, topology_run_id, node_id, node_kind, state, created_at)
             VALUES (?1, ?2, 'plan', 'llm', 'running', ?3)",
            rusqlite::params![execution_id, run_id, now],
        )
        .expect("insert node execution");

        conn.execute(
            "INSERT INTO edge_evaluations (id, topology_run_id, source_node_execution_id, edge_id, target_node_id, matched, created_at)
             VALUES (?1, ?2, ?3, 'edge-1', 'review', 1, ?4)",
            rusqlite::params![uuid::Uuid::new_v4().to_string(), run_id, execution_id, now],
        )
        .expect("insert edge evaluation");

        conn.execute(
            "INSERT INTO node_retries (id, node_execution_id, attempt_number, failure_kind, error_message, created_at)
             VALUES (?1, ?2, 1, 'timeout', 'timed out', ?3)",
            rusqlite::params![uuid::Uuid::new_v4().to_string(), execution_id, now],
        )
        .expect("insert node retry");

        conn.execute(
            "DELETE FROM topology_runs WHERE id = ?1",
            rusqlite::params![run_id],
        )
        .expect("delete topology run");

        let remaining_nodes: i64 = conn
            .query_row("SELECT COUNT(*) FROM node_executions", [], |r| r.get(0))
            .expect("count node executions");
        let remaining_edges: i64 = conn
            .query_row("SELECT COUNT(*) FROM edge_evaluations", [], |r| r.get(0))
            .expect("count edge evaluations");
        let remaining_retries: i64 = conn
            .query_row("SELECT COUNT(*) FROM node_retries", [], |r| r.get(0))
            .expect("count node retries");

        assert_eq!(remaining_nodes, 0);
        assert_eq!(remaining_edges, 0);
        assert_eq!(remaining_retries, 0);
    }
}
