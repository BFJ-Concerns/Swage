use rusqlite::{Connection, params};
use swage_core::{
    StorageError, compact_tool_arguments_for_model, compact_tool_result_for_model_with_reference,
    tool_result_reference,
};

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
        ("011_approval_memory", CREATE_APPROVAL_MEMORY),
        ("012_thread_branches", CREATE_THREAD_BRANCHES),
        ("013_message_turn_id", ADD_MESSAGE_TURN_ID),
        ("014_turn_inbound_audit", ADD_TURN_INBOUND_AUDIT),
        ("016_session_metadata", ADD_SESSION_METADATA_COLUMN),
        // 017/018 were the scheduled-job and heartbeat tables, absorbed into
        // the unified schedules schema in 001 during the private alpha.
        ("021_delivery_and_audit", CREATE_DELIVERY_AND_AUDIT_TABLES),
        ("022_secrets", CREATE_SECRET_TABLES),
        ("023_encrypt_managed_secrets_at_rest", ""),
        (
            "024_attachments_and_background_jobs",
            CREATE_ATTACHMENT_AND_BACKGROUND_JOB_TABLES,
        ),
        ("025_materials", CREATE_MATERIAL_TABLES),
        ("028_deduplicate_attachment_ingestion_content", ""),
        ("032_agent_memory", CREATE_AGENT_MEMORY_TABLE),
        (
            "033_workspace_attachments",
            CREATE_WORKSPACE_ATTACHMENT_TABLE,
        ),
        (
            "034_one_in_flight_turn_per_thread",
            CREATE_ONE_IN_FLIGHT_TURN_INDEX,
        ),
        (
            "035_workspace_attachment_canonical_state",
            ADD_WORKSPACE_ATTACHMENT_CANONICAL_STATE,
        ),
        (
            "036_workspace_attachment_activation",
            ADD_WORKSPACE_ATTACHMENT_ACTIVATION,
        ),
        ("038_constrain_message_enums", CONSTRAIN_MESSAGE_ENUMS),
        ("040_session_selected_model", ADD_SESSION_SELECTED_MODEL),
        ("041_tool_call_model_replay", ADD_TOOL_CALL_MODEL_REPLAY),
        ("042_tool_call_provider_round", ADD_TOOL_CALL_PROVIDER_ROUND),
        ("043_knowledge_base", CREATE_KNOWLEDGE_BASE_TABLES),
        ("044_agent_definitions", CREATE_AGENT_DEFINITION_TABLE),
        (
            "045_knowledge_curation_assertions",
            CREATE_KNOWLEDGE_CURATION_ASSERTIONS,
        ),
        ("046_workflow_run_parentage", ADD_WORKFLOW_RUN_PARENTAGE),
        ("047_knowledge_citations", CREATE_KNOWLEDGE_CITATIONS),
        (
            "048_knowledge_orientation_cards",
            CREATE_KNOWLEDGE_ORIENTATION_CARDS,
        ),
        ("050_workflow_run_events", CREATE_WORKFLOW_RUN_EVENTS),
        ("051_workflow_run_parking", ADD_WORKFLOW_RUN_PARKING),
        ("052_task_definitions", CREATE_TASK_DEFINITION_TABLE),
        ("053_session_variables", CREATE_SESSION_VARIABLES),
        (
            "054_observation_capture_scope",
            ADD_OBSERVATION_CAPTURE_SCOPE,
        ),
        (
            "055_tool_call_display_summary",
            ADD_TOOL_CALL_DISPLAY_SUMMARY,
        ),
        ("056_turn_delegated_tokens", ADD_TURN_DELEGATED_TOKENS),
        ("057_pages", CREATE_PAGE_TABLES),
    ]
}

/// Pages — agent-run tabs on a space — and their module instances. The
/// database is authoritative for page content; the alpha wipe-and-rebuild
/// posture applies (pages are work product, regenerable by their authors,
/// unlike knowledge-base evidence).
const CREATE_PAGE_TABLES: &str = "
CREATE TABLE IF NOT EXISTS pages (
    id          TEXT NOT NULL PRIMARY KEY,
    space_id    TEXT NOT NULL REFERENCES spaces(id),
    title       TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_pages_space ON pages(space_id);
CREATE TABLE IF NOT EXISTS page_modules (
    id          TEXT NOT NULL PRIMARY KEY,
    page_id     TEXT NOT NULL REFERENCES pages(id) ON DELETE CASCADE,
    kind        TEXT NOT NULL,
    position    INTEGER NOT NULL,
    source      TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_page_modules_page ON page_modules(page_id);
";

/// Delegated-child usage aggregated onto the delegating turn: a billing
/// figure separate from the turn's own token columns, so which-child-spent-
/// what stays reconstructable from child turn rows (parent_turn_id + their
/// own tokens) while the parent's context measure keeps reading only its
/// own fields.
const ADD_TURN_DELEGATED_TOKENS: &str = "
ALTER TABLE turns ADD COLUMN delegated_input_tokens INTEGER;
ALTER TABLE turns ADD COLUMN delegated_output_tokens INTEGER;
";

/// One-line display headline per tool call, derived from the tool's declared
/// display hints at execution time. Nullable: a tool with no hint stores
/// NULL and renderers use their generic fallback.
const ADD_TOOL_CALL_DISPLAY_SUMMARY: &str = "
ALTER TABLE tool_calls ADD COLUMN display_summary TEXT;
";

/// Retry-safe capture identity for observations. A caller that may run the
/// same capture twice — a workflow's journalled agent call re-executed after
/// an interruption — records under a stable scope; a second capture of the
/// same origin in the same scope returns the existing observation instead of
/// forking the evidence. Additive on the knowledge-base family: existing
/// rows keep NULL (no scope, no dedup change) and bodies and origin
/// metadata are untouched.
const ADD_OBSERVATION_CAPTURE_SCOPE: &str = "
    ALTER TABLE source_observations ADD COLUMN capture_scope TEXT;

    CREATE INDEX IF NOT EXISTS idx_source_observations_space_scope
        ON source_observations(space_id, capture_scope)
        WHERE capture_scope IS NOT NULL;
";

const CREATE_SESSION_VARIABLES: &str = "
    CREATE TABLE session_variables (
        session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
        name TEXT NOT NULL,
        value TEXT NOT NULL,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL,
        PRIMARY KEY (session_id, name)
    );
";

/// Durable waits: a run sleeping on a long timer parks — no engine, no
/// per-run OS thread — recording when it is due (`wake_at`); a background
/// waker resumes it by journal replay. The state CHECK gains 'parked', which
/// SQLite cannot widen in place, so the table is rebuilt with explicit
/// column lists (the ALTER-added columns sit at the end of the old table).
const ADD_WORKFLOW_RUN_PARKING: &str = "
    CREATE TABLE workflow_runs_parking_new (
        id                   TEXT NOT NULL PRIMARY KEY,
        space_id             TEXT NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
        script_version_id    TEXT REFERENCES workflow_script_versions(id),
        inline_workflow_id   TEXT,
        inline_workflow_name TEXT,
        inline_workflow_version INTEGER,
        carrying_skill       TEXT,
        source               TEXT,
        source_hash          TEXT NOT NULL,
        args_json            TEXT NOT NULL,
        budget_json          TEXT NOT NULL,
        state                TEXT NOT NULL CHECK (state IN ('running', 'completed', 'failed', 'cancelled', 'interrupted', 'parked')),
        final_output_json    TEXT,
        handover_json        TEXT,
        error_message        TEXT,
        engine_version       TEXT NOT NULL,
        created_at           TEXT NOT NULL,
        completed_at         TEXT,
        parent_run_id        TEXT REFERENCES workflow_runs(id),
        parent_seq           INTEGER,
        dispatch_bound_agent_ids_json TEXT NOT NULL,
        dispatch_bound_task_definition_ids_json TEXT NOT NULL,
        workflow_call_tools_json TEXT,
        wake_at              TEXT,
        inherited_worker_placement_json TEXT
    );
    INSERT INTO workflow_runs_parking_new (
        id, space_id, script_version_id, inline_workflow_id,
        inline_workflow_name, inline_workflow_version, carrying_skill,
        source, source_hash, args_json,
        budget_json, state, final_output_json, handover_json, error_message, engine_version,
        created_at, completed_at, parent_run_id, parent_seq,
        dispatch_bound_agent_ids_json, dispatch_bound_task_definition_ids_json,
        workflow_call_tools_json,
        inherited_worker_placement_json
    )
    SELECT
        id, space_id, script_version_id, inline_workflow_id,
        inline_workflow_name, inline_workflow_version, NULL,
        source, source_hash, args_json,
        budget_json, state, final_output_json, NULL, error_message, engine_version,
        created_at, completed_at, parent_run_id, parent_seq,
        dispatch_bound_agent_ids_json, dispatch_bound_task_definition_ids_json,
        workflow_call_tools_json,
        inherited_worker_placement_json
    FROM workflow_runs;
    DROP TABLE workflow_runs;
    ALTER TABLE workflow_runs_parking_new RENAME TO workflow_runs;
    CREATE INDEX IF NOT EXISTS idx_workflow_runs_space_created
        ON workflow_runs(space_id, created_at DESC);
    CREATE INDEX IF NOT EXISTS idx_workflow_runs_state
        ON workflow_runs(state);
    CREATE UNIQUE INDEX IF NOT EXISTS idx_workflow_runs_parent_call
        ON workflow_runs(parent_run_id, parent_seq)
        WHERE parent_run_id IS NOT NULL;
";

/// A run's durable event queue: namespaced events from registered posters
/// accumulate as data between lead wakes. Events stay pending
/// until a wake delivers them — a gate that suppresses a wake consumes
/// nothing, so suppressed events ride the next wake for retrospective
/// review. Each row is keyed to the journal entry that posted it, making the
/// post exactly-once under replay. The journal's kind CHECK also widens for
/// the two new host-call kinds (event, gate); SQLite cannot widen a CHECK in
/// place, so the table is rebuilt, existing rows carrying over unchanged.
const CREATE_WORKFLOW_RUN_EVENTS: &str = "
    CREATE TABLE IF NOT EXISTS workflow_run_events (
        id                 TEXT NOT NULL PRIMARY KEY,
        run_id             TEXT NOT NULL REFERENCES workflow_runs(id) ON DELETE CASCADE,
        journal_entry_id   TEXT NOT NULL UNIQUE,
        seq                INTEGER NOT NULL,
        kind               TEXT NOT NULL,
        payload_json       TEXT NOT NULL,
        source_surface     TEXT NOT NULL,
        source_agent_id    TEXT,
        source_session_id  TEXT,
        source_turn_id     TEXT,
        posted_at          TEXT NOT NULL,
        delivered_wake_seq INTEGER,
        delivered_at       TEXT,
        UNIQUE (run_id, seq)
    );
    CREATE INDEX IF NOT EXISTS idx_workflow_run_events_pending
        ON workflow_run_events(run_id, seq)
        WHERE delivered_at IS NULL;

    CREATE TABLE workflow_journal_events_new (
        id                TEXT NOT NULL PRIMARY KEY,
        run_id            TEXT NOT NULL REFERENCES workflow_runs(id) ON DELETE CASCADE,
        seq               INTEGER NOT NULL,
        completed_order   INTEGER,
        kind              TEXT NOT NULL CHECK (kind IN ('agent_call', 'worker_placement', 'effect', 'log', 'phase', 'workflow_call', 'timer', 'event', 'await_event', 'gate', 'budget', 'terminal_budget', 'wind_down')),
        status            TEXT NOT NULL CHECK (status IN ('started', 'completed', 'failed')),
        args_hash         TEXT NOT NULL,
        request_json      TEXT NOT NULL,
        result_json       TEXT,
        error_message     TEXT,
        effect_kind       TEXT,
        idempotency_key   TEXT,
        target_kind       TEXT,
        started_at        TEXT NOT NULL,
        completed_at      TEXT,
        UNIQUE (run_id, seq),
        UNIQUE (run_id, completed_order)
    );
    INSERT INTO workflow_journal_events_new SELECT * FROM workflow_journal;
    DROP TABLE workflow_journal;
    ALTER TABLE workflow_journal_events_new RENAME TO workflow_journal;
    CREATE INDEX IF NOT EXISTS idx_workflow_journal_run_seq
        ON workflow_journal(run_id, seq);
";

/// Composed workflow runs: a child run started by a parent's `workflow()`
/// call records which parent and which journal seq started it, so a resumed
/// parent finds its existing child instead of starting a duplicate.
const ADD_WORKFLOW_RUN_PARENTAGE: &str = "
    ALTER TABLE workflow_runs
        ADD COLUMN parent_run_id TEXT REFERENCES workflow_runs(id);
    ALTER TABLE workflow_runs
        ADD COLUMN parent_seq INTEGER;
    CREATE UNIQUE INDEX IF NOT EXISTS idx_workflow_runs_parent_call
        ON workflow_runs(parent_run_id, parent_seq)
        WHERE parent_run_id IS NOT NULL;

    -- The journal gains two host-call kinds (workflow_call, timer); SQLite
    -- cannot widen a CHECK in place, so the table is rebuilt. Existing rows
    -- carry over unchanged.
    CREATE TABLE workflow_journal_new (
        id                TEXT NOT NULL PRIMARY KEY,
        run_id            TEXT NOT NULL REFERENCES workflow_runs(id) ON DELETE CASCADE,
        seq               INTEGER NOT NULL,
        completed_order   INTEGER,
        kind              TEXT NOT NULL CHECK (kind IN ('agent_call', 'effect', 'log', 'phase', 'workflow_call', 'timer')),
        status            TEXT NOT NULL CHECK (status IN ('started', 'completed', 'failed')),
        args_hash         TEXT NOT NULL,
        request_json      TEXT NOT NULL,
        result_json       TEXT,
        error_message     TEXT,
        effect_kind       TEXT,
        idempotency_key   TEXT,
        target_kind       TEXT,
        started_at        TEXT NOT NULL,
        completed_at      TEXT,
        UNIQUE (run_id, seq),
        UNIQUE (run_id, completed_order)
    );
    INSERT INTO workflow_journal_new SELECT * FROM workflow_journal;
    DROP TABLE workflow_journal;
    ALTER TABLE workflow_journal_new RENAME TO workflow_journal;
    CREATE INDEX IF NOT EXISTS idx_workflow_journal_run_seq
        ON workflow_journal(run_id, seq);
";

/// Per-turn context assembly report, stored as raw JSON to avoid coupling the
/// storage layer to the AssemblyReport type definition.
const CREATE_CONTEXT_SNAPSHOTS: &str = "
    CREATE TABLE IF NOT EXISTS context_snapshots (
        turn_id     TEXT NOT NULL PRIMARY KEY REFERENCES turns(id) ON DELETE CASCADE,
        report_json TEXT NOT NULL,
        created_at  TEXT NOT NULL
    );
";

const CREATE_WORKSPACE_ATTACHMENT_TABLE: &str = "
    CREATE TABLE IF NOT EXISTS workspace_attachments (
        id               TEXT NOT NULL PRIMARY KEY,
        space_id         TEXT NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
        label            TEXT NOT NULL,
        path             TEXT NOT NULL,
        permissions_json TEXT NOT NULL,
        is_trusted       INTEGER NOT NULL DEFAULT 0,
        created_at       TEXT NOT NULL,
        updated_at       TEXT NOT NULL
    );

    CREATE INDEX IF NOT EXISTS idx_workspace_attachments_space_id
        ON workspace_attachments(space_id);
    CREATE UNIQUE INDEX IF NOT EXISTS idx_workspace_attachments_space_path
        ON workspace_attachments(space_id, path);
";

const CREATE_DELIVERY_AND_AUDIT_TABLES: &str = "
    CREATE TABLE IF NOT EXISTS delivery_records (
        id           TEXT NOT NULL PRIMARY KEY,
        session_id   TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
        run_id       TEXT REFERENCES schedule_runs(id) ON DELETE SET NULL,
        turn_id      TEXT REFERENCES turns(id) ON DELETE SET NULL,
        target_json  TEXT NOT NULL,
        content      TEXT NOT NULL,
        status       TEXT NOT NULL CHECK (status IN ('sent', 'failed', 'not_sent')),
        error        TEXT,
        delivered_at TEXT NOT NULL
    );

    CREATE INDEX IF NOT EXISTS idx_delivery_records_session_id
        ON delivery_records(session_id, delivered_at);

    CREATE TABLE IF NOT EXISTS audit_events (
        id           TEXT NOT NULL PRIMARY KEY,
        session_id   TEXT REFERENCES sessions(id) ON DELETE CASCADE,
        entity_kind  TEXT NOT NULL,
        entity_id    TEXT NOT NULL,
        event_kind   TEXT NOT NULL,
        summary      TEXT NOT NULL,
        metadata_json TEXT NOT NULL,
        created_at   TEXT NOT NULL
    );

    CREATE INDEX IF NOT EXISTS idx_audit_events_session_id
        ON audit_events(session_id, created_at);
    CREATE INDEX IF NOT EXISTS idx_audit_events_entity
        ON audit_events(entity_kind, entity_id, created_at);
    CREATE INDEX IF NOT EXISTS idx_audit_events_created_at
        ON audit_events(created_at);

    CREATE TRIGGER IF NOT EXISTS trg_turns_state_audit
    AFTER UPDATE OF state ON turns
    WHEN OLD.state <> NEW.state
    BEGIN
        INSERT INTO audit_events (
            id, session_id, entity_kind, entity_id, event_kind, summary, metadata_json, created_at
        ) VALUES (
            lower(hex(randomblob(16))),
            (SELECT session_id FROM threads WHERE id = NEW.thread_id),
            'turn',
            NEW.id,
            'state_transition',
            'turn state ' || OLD.state || ' -> ' || NEW.state,
            json_object(
                'from_state', OLD.state,
                'to_state', NEW.state,
                'thread_id', NEW.thread_id,
                'agent_id', NEW.agent_id
            ),
            strftime('%Y-%m-%dT%H:%M:%fZ','now')
        );
    END;

    CREATE TRIGGER IF NOT EXISTS trg_approval_state_audit
    AFTER UPDATE OF state ON approval_requests
    WHEN OLD.state <> NEW.state
    BEGIN
        INSERT INTO audit_events (
            id, session_id, entity_kind, entity_id, event_kind, summary, metadata_json, created_at
        ) VALUES (
            lower(hex(randomblob(16))),
            (
                SELECT th.session_id
                FROM turns t
                INNER JOIN threads th ON th.id = t.thread_id
                WHERE t.id = NEW.turn_id
            ),
            'approval',
            NEW.id,
            'state_transition',
            'approval state ' || OLD.state || ' -> ' || NEW.state,
            json_object(
                'from_state', OLD.state,
                'to_state', NEW.state,
                'turn_id', NEW.turn_id,
                'kind', NEW.kind,
                'decision_reason', COALESCE(NEW.decision_reason, '')
            ),
            strftime('%Y-%m-%dT%H:%M:%fZ','now')
        );
    END;

    CREATE TRIGGER IF NOT EXISTS trg_schedule_runs_state_audit
    AFTER UPDATE OF state ON schedule_runs
    WHEN OLD.state <> NEW.state
    BEGIN
        INSERT INTO audit_events (
            id, session_id, entity_kind, entity_id, event_kind, summary, metadata_json, created_at
        ) VALUES (
            lower(hex(randomblob(16))),
            NEW.session_id,
            'schedule_run',
            NEW.id,
            'state_transition',
            'schedule run state ' || OLD.state || ' -> ' || NEW.state,
            json_object(
                'from_state', OLD.state,
                'to_state', NEW.state,
                'schedule_id', NEW.schedule_id,
                'trigger', NEW.trigger_kind,
                'skip_reason', COALESCE(NEW.skip_reason, ''),
                'gate_verdict', COALESCE(NEW.gate_verdict, ''),
                'turn_id', COALESCE(NEW.turn_id, ''),
                'error_message', COALESCE(NEW.error_message, '')
            ),
            strftime('%Y-%m-%dT%H:%M:%fZ','now')
        );
    END;

    CREATE TRIGGER IF NOT EXISTS trg_delivery_record_audit
    AFTER INSERT ON delivery_records
    BEGIN
        INSERT INTO audit_events (
            id, session_id, entity_kind, entity_id, event_kind, summary, metadata_json, created_at
        ) VALUES (
            lower(hex(randomblob(16))),
            NEW.session_id,
            'delivery_record',
            NEW.id,
            'delivery_recorded',
            'delivery recorded',
            json_object(
                'target', NEW.target_json,
                'content', NEW.content,
                'status', NEW.status,
                'error', COALESCE(NEW.error, '')
            ),
            strftime('%Y-%m-%dT%H:%M:%fZ','now')
        );
    END;

    CREATE TRIGGER IF NOT EXISTS trg_channel_inbound_state_audit
    AFTER UPDATE OF state ON channel_inbound_messages
    WHEN OLD.state <> NEW.state
    BEGIN
        INSERT INTO audit_events (
            id, session_id, entity_kind, entity_id, event_kind, summary, metadata_json, created_at
        ) VALUES (
            lower(hex(randomblob(16))),
            (
                SELECT th.session_id
                FROM turns t
                INNER JOIN threads th ON th.id = t.thread_id
                WHERE t.id = NEW.turn_id
            ),
            'channel_inbound_message',
            NEW.id,
            'state_transition',
            'channel inbound message state ' || OLD.state || ' -> ' || NEW.state,
            json_object(
                'from_state', OLD.state,
                'to_state', NEW.state,
                'integration', NEW.integration,
                'account', NEW.account,
                'conversation', NEW.conversation,
                'external_id', NEW.external_id,
                'turn_id', COALESCE(NEW.turn_id, ''),
                'discard_reason', COALESCE(NEW.discard_reason, '')
            ),
            strftime('%Y-%m-%dT%H:%M:%fZ','now')
        );
    END;
";

const CREATE_AGENT_MEMORY_TABLE: &str = "
    CREATE TABLE IF NOT EXISTS agent_memories (
        id                TEXT NOT NULL PRIMARY KEY,
        space_id        TEXT NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
        session_id        TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
        turn_id           TEXT NOT NULL REFERENCES turns(id) ON DELETE CASCADE,
        agent_id          TEXT NOT NULL,
        memory_kind       TEXT NOT NULL,
        title             TEXT NOT NULL,
        content           TEXT NOT NULL,
        tags_json         TEXT NOT NULL,
        outcome_status    TEXT,
        source_summary    TEXT NOT NULL,
        metadata_json     TEXT NOT NULL,
        created_at        TEXT NOT NULL,
        updated_at        TEXT NOT NULL
    );

    CREATE INDEX IF NOT EXISTS idx_agent_memories_space_agent
        ON agent_memories(space_id, agent_id, created_at DESC);
    CREATE INDEX IF NOT EXISTS idx_agent_memories_session
        ON agent_memories(session_id, created_at DESC);
    CREATE INDEX IF NOT EXISTS idx_agent_memories_turn
        ON agent_memories(turn_id, created_at DESC);

    CREATE TRIGGER IF NOT EXISTS trg_agent_memories_created_audit
    AFTER INSERT ON agent_memories
    BEGIN
        INSERT INTO audit_events (
            id, session_id, entity_kind, entity_id, event_kind, summary, metadata_json, created_at
        ) VALUES (
            lower(hex(randomblob(16))),
            NEW.session_id,
            'agent_memory',
            NEW.id,
            'memory_recorded',
            'agent memory recorded',
            json_object(
                'space_id', NEW.space_id,
                'turn_id', NEW.turn_id,
                'agent_id', NEW.agent_id,
                'memory_kind', NEW.memory_kind,
                'title', NEW.title,
                'outcome_status', COALESCE(NEW.outcome_status, ''),
                'source_summary', NEW.source_summary,
                'tags', json(NEW.tags_json)
            ),
            strftime('%Y-%m-%dT%H:%M:%fZ','now')
        );
    END;
";

const CREATE_SECRET_TABLES: &str = "
    CREATE TABLE IF NOT EXISTS secrets (
        id                 TEXT NOT NULL PRIMARY KEY,
        name               TEXT NOT NULL,
        material_kind      TEXT NOT NULL,
        owner_kind         TEXT NOT NULL,
        backend_kind       TEXT NOT NULL,
        scope_kind         TEXT NOT NULL,
        scope_id           TEXT,
        reference          TEXT NOT NULL UNIQUE,
        external_reference TEXT,
        metadata_json      TEXT NOT NULL,
        status             TEXT NOT NULL,
        created_at         TEXT NOT NULL,
        updated_at         TEXT NOT NULL,
        revoked_at         TEXT
    );

    CREATE INDEX IF NOT EXISTS idx_secrets_scope
        ON secrets(scope_kind, scope_id, created_at);
    CREATE INDEX IF NOT EXISTS idx_secrets_material_kind
        ON secrets(material_kind, created_at);
    CREATE INDEX IF NOT EXISTS idx_secrets_status
        ON secrets(status, created_at);

    CREATE TABLE IF NOT EXISTS managed_secret_values (
        secret_id       TEXT NOT NULL PRIMARY KEY REFERENCES secrets(id) ON DELETE CASCADE,
        algorithm       TEXT NOT NULL,
        key_source      TEXT NOT NULL,
        nonce_b64       TEXT NOT NULL,
        ciphertext_b64  TEXT NOT NULL,
        updated_at      TEXT NOT NULL
    );
";

const CREATE_ATTACHMENT_AND_BACKGROUND_JOB_TABLES: &str = "
    CREATE TABLE IF NOT EXISTS attachments (
        id                TEXT NOT NULL PRIMARY KEY,
        scope_kind        TEXT NOT NULL,
        session_id        TEXT REFERENCES sessions(id) ON DELETE CASCADE,
        space_id        TEXT REFERENCES spaces(id) ON DELETE CASCADE,
        kind              TEXT NOT NULL,
        status            TEXT NOT NULL,
        filename          TEXT NOT NULL,
        mime_type         TEXT NOT NULL,
        content_bytes     BLOB NOT NULL,
        byte_count        INTEGER NOT NULL,
        ingested_markdown TEXT,
        created_at        TEXT NOT NULL,
        updated_at        TEXT NOT NULL
    );

    CREATE INDEX IF NOT EXISTS idx_attachments_session_id
        ON attachments(session_id, created_at);
    CREATE INDEX IF NOT EXISTS idx_attachments_space_id
        ON attachments(space_id, created_at);

    CREATE TABLE IF NOT EXISTS background_jobs (
        id             TEXT NOT NULL PRIMARY KEY,
        kind           TEXT NOT NULL,
        state          TEXT NOT NULL,
        attachment_id  TEXT REFERENCES attachments(id) ON DELETE CASCADE,
        payload_json   TEXT NOT NULL,
        result_json    TEXT,
        error_message  TEXT,
        attempt_count  INTEGER NOT NULL,
        created_at     TEXT NOT NULL,
        updated_at     TEXT NOT NULL,
        started_at     TEXT,
        completed_at   TEXT
    );

    CREATE INDEX IF NOT EXISTS idx_background_jobs_state
        ON background_jobs(state, updated_at);
    CREATE INDEX IF NOT EXISTS idx_background_jobs_attachment_id
        ON background_jobs(attachment_id, created_at);
";

const CREATE_MATERIAL_TABLES: &str = "
    CREATE TABLE IF NOT EXISTS materials (
        id                   TEXT NOT NULL PRIMARY KEY,
        scope_kind           TEXT NOT NULL,
        session_id           TEXT REFERENCES sessions(id) ON DELETE CASCADE,
        space_id           TEXT REFERENCES spaces(id) ON DELETE CASCADE,
        source_kind          TEXT NOT NULL,
        source_attachment_id TEXT REFERENCES attachments(id) ON DELETE SET NULL,
        status               TEXT NOT NULL,
        title                TEXT NOT NULL,
        relative_path        TEXT NOT NULL,
        mime_type            TEXT NOT NULL,
        content_text         TEXT NOT NULL,
        created_at           TEXT NOT NULL,
        updated_at           TEXT NOT NULL,
        archived_at          TEXT
    );

    CREATE INDEX IF NOT EXISTS idx_materials_session_id
        ON materials(session_id, created_at);
    CREATE INDEX IF NOT EXISTS idx_materials_space_id
        ON materials(space_id, created_at);
    CREATE UNIQUE INDEX IF NOT EXISTS idx_materials_source_attachment_id
        ON materials(source_attachment_id)
        WHERE source_attachment_id IS NOT NULL;
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
    -- Spaces: name + slug uniqueness. External resources attach through
    -- workspace_attachments so spaces can exist without any workspace.
    CREATE TABLE IF NOT EXISTS spaces (
        id             TEXT NOT NULL PRIMARY KEY,
        name           TEXT NOT NULL UNIQUE,
        description    TEXT NOT NULL DEFAULT '',
        slug           TEXT NOT NULL DEFAULT '',
        is_default     INTEGER NOT NULL DEFAULT 0,
        created_at     TEXT NOT NULL,
        updated_at     TEXT NOT NULL
    );

    CREATE UNIQUE INDEX IF NOT EXISTS idx_spaces_slug ON spaces(slug);
    CREATE UNIQUE INDEX IF NOT EXISTS idx_spaces_single_default
        ON spaces(is_default) WHERE is_default = 1;

    -- Devices: local or paired machines available for tool execution.
    CREATE TABLE IF NOT EXISTS devices (
        device_id              TEXT NOT NULL PRIMARY KEY,
        label                TEXT,
        kind                 TEXT NOT NULL,
        base_url             TEXT NOT NULL,
        shared_token         TEXT NOT NULL,
        workspace_root       TEXT NOT NULL,
        workspace_roots_json TEXT NOT NULL DEFAULT '[]',
        capabilities_json    TEXT NOT NULL,
        registered_at        TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS workspace_attachments (
        id               TEXT NOT NULL PRIMARY KEY,
        space_id         TEXT NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
        label            TEXT NOT NULL,
        path             TEXT NOT NULL,
        permissions_json TEXT NOT NULL,
        is_trusted       INTEGER NOT NULL DEFAULT 0,
        created_at       TEXT NOT NULL,
        updated_at       TEXT NOT NULL
    );

    CREATE INDEX IF NOT EXISTS idx_workspace_attachments_space_id
        ON workspace_attachments(space_id);
    CREATE UNIQUE INDEX IF NOT EXISTS idx_workspace_attachments_space_path
        ON workspace_attachments(space_id, path);

    -- Sessions: belong to a space, track which agent owns the session.
    CREATE TABLE IF NOT EXISTS sessions (
        id         TEXT NOT NULL PRIMARY KEY,
        space_id TEXT NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
        agent_id   TEXT NOT NULL,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL
    );

    CREATE INDEX IF NOT EXISTS idx_sessions_space_id ON sessions(space_id);
    CREATE INDEX IF NOT EXISTS idx_sessions_agent_id   ON sessions(agent_id);

    -- Channel links: durable one-time binding from an external conversation
    -- origin to the space/agent/session it should resume.
    CREATE TABLE IF NOT EXISTS channel_links (
        integration      TEXT NOT NULL,
        account          TEXT NOT NULL,
        conversation     TEXT NOT NULL,
        space_id         TEXT NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
        agent_id         TEXT NOT NULL,
        session_id       TEXT REFERENCES sessions(id) ON DELETE SET NULL,
        home_target_json TEXT,
        linked_at        TEXT NOT NULL,
        updated_at       TEXT NOT NULL,
        PRIMARY KEY (integration, account, conversation)
    );

    CREATE INDEX IF NOT EXISTS idx_channel_links_space_id
        ON channel_links(space_id);
    CREATE INDEX IF NOT EXISTS idx_channel_links_session_id
        ON channel_links(session_id);

    -- Schedules: one durable definition per recurring or one-off piece of
    -- agent work. The session policy is flattened into columns: fresh rows
    -- carry the target space/agent, continuation rows the session each run
    -- returns to.
    CREATE TABLE IF NOT EXISTS schedules (
        id             TEXT NOT NULL PRIMARY KEY,
        message        TEXT NOT NULL,
        status         TEXT NOT NULL,
        kind           TEXT NOT NULL,
        expression     TEXT NOT NULL,
        session_policy TEXT NOT NULL,
        space_id       TEXT REFERENCES spaces(id) ON DELETE CASCADE,
        agent_id       TEXT,
        session_id     TEXT REFERENCES sessions(id) ON DELETE CASCADE,
        -- Provenance: 'user' or 'agent'. creator_agent_id names the agent
        -- for agent-created schedules, and is NULL for user-created ones.
        creator_kind     TEXT NOT NULL,
        creator_agent_id TEXT,
        delivery_json  TEXT NOT NULL,
        gate_model     TEXT,
        gate_criteria  TEXT,
        next_run_at    TEXT,
        last_run_at    TEXT,
        created_at     TEXT NOT NULL,
        updated_at     TEXT NOT NULL
    );

    CREATE INDEX IF NOT EXISTS idx_schedules_next_run
        ON schedules(status, next_run_at);
    CREATE INDEX IF NOT EXISTS idx_schedules_session_id
        ON schedules(session_id);

    -- Schedule runs: the complete record of what the scheduler did with
    -- each due occurrence — executed, gated out, or skipped for overlap.
    -- session_id/turn_id have no FK on purpose: runs are audit records and
    -- outlive the sessions they executed in.
    CREATE TABLE IF NOT EXISTS schedule_runs (
        id             TEXT NOT NULL PRIMARY KEY,
        schedule_id    TEXT NOT NULL REFERENCES schedules(id) ON DELETE CASCADE,
        trigger_kind   TEXT NOT NULL,
        state          TEXT NOT NULL,
        skip_reason    TEXT,
        scheduled_for  TEXT,
        session_id     TEXT,
        turn_id        TEXT,
        gate_model     TEXT,
        gate_verdict   TEXT,
        gate_reasoning TEXT,
        output_text    TEXT,
        error_message  TEXT,
        created_at     TEXT NOT NULL,
        started_at     TEXT,
        completed_at   TEXT
    );

    CREATE INDEX IF NOT EXISTS idx_schedule_runs_schedule_id
        ON schedule_runs(schedule_id, created_at);
    CREATE INDEX IF NOT EXISTS idx_schedule_runs_state
        ON schedule_runs(schedule_id, state);

    -- At most one in-flight run per schedule. This partial unique index is
    -- the overlap guard itself: the pending-run insert either succeeds
    -- (this run owns the schedule) or trips the constraint (a predecessor is
    -- still running), which the scheduler turns into a deliberate skip.
    -- Terminal rows fall outside the predicate, so any number of finished
    -- runs — including skipped-for-overlap ones — coexist freely.
    CREATE UNIQUE INDEX IF NOT EXISTS idx_schedule_runs_one_active
        ON schedule_runs(schedule_id)
        WHERE state IN ('pending', 'running');

    -- Threads: a conversation thread within a session, owned by an agent.
    -- State defaults to 'idle'; transitions are managed by the agent runtime.
    CREATE TABLE IF NOT EXISTS threads (
        id         TEXT NOT NULL PRIMARY KEY,
        session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
        agent_id   TEXT NOT NULL,
        state      TEXT NOT NULL DEFAULT 'idle',
        codex_thread_id TEXT,
        codex_tools_json TEXT,
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
    CREATE UNIQUE INDEX IF NOT EXISTS idx_turns_one_in_flight_per_thread
        ON turns(thread_id)
        WHERE state IN ('pending', 'processing', 'awaiting_approval', 'delegating');

    -- Inbound external-channel messages are durably queued before any turn
    -- begins. Queue rows keep Telegram/update provenance even though the turn
    -- itself receives one composed input string.
    CREATE TABLE IF NOT EXISTS channel_inbound_messages (
        id             TEXT NOT NULL PRIMARY KEY,
        integration    TEXT NOT NULL,
        account        TEXT NOT NULL,
        conversation   TEXT NOT NULL,
        external_id    TEXT NOT NULL,
        sender         TEXT NOT NULL,
        content        TEXT NOT NULL,
        state          TEXT NOT NULL CHECK (state IN ('pending', 'presented', 'discarded')),
        discard_reason TEXT,
        turn_id        TEXT,
        received_at    TEXT NOT NULL,
        presented_at   TEXT,
        UNIQUE (integration, account, external_id)
    );

    CREATE INDEX IF NOT EXISTS idx_channel_inbound_pending_origin
        ON channel_inbound_messages(state, integration, account, conversation, received_at);
    CREATE INDEX IF NOT EXISTS idx_channel_inbound_turn_id
        ON channel_inbound_messages(turn_id);

    CREATE TABLE IF NOT EXISTS channel_cursors (
        integration TEXT NOT NULL,
        account     TEXT NOT NULL,
        cursor      TEXT NOT NULL,
        updated_at  TEXT NOT NULL,
        PRIMARY KEY (integration, account)
    );

    CREATE TABLE IF NOT EXISTS delivery_records (
        id           TEXT NOT NULL PRIMARY KEY,
        session_id   TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
        run_id       TEXT REFERENCES schedule_runs(id) ON DELETE SET NULL,
        turn_id      TEXT REFERENCES turns(id) ON DELETE SET NULL,
        target_json  TEXT NOT NULL,
        content      TEXT NOT NULL,
        status       TEXT NOT NULL CHECK (status IN ('sent', 'failed', 'not_sent')),
        error        TEXT,
        delivered_at TEXT NOT NULL
    );

    CREATE INDEX IF NOT EXISTS idx_delivery_records_session_id
        ON delivery_records(session_id, delivered_at);

    CREATE TABLE IF NOT EXISTS audit_events (
        id           TEXT NOT NULL PRIMARY KEY,
        session_id   TEXT REFERENCES sessions(id) ON DELETE CASCADE,
        entity_kind  TEXT NOT NULL,
        entity_id    TEXT NOT NULL,
        event_kind   TEXT NOT NULL,
        summary      TEXT NOT NULL,
        metadata_json TEXT NOT NULL,
        created_at   TEXT NOT NULL
    );

    CREATE INDEX IF NOT EXISTS idx_audit_events_session_id
        ON audit_events(session_id, created_at);
    CREATE INDEX IF NOT EXISTS idx_audit_events_entity
        ON audit_events(entity_kind, entity_id, created_at);
    CREATE INDEX IF NOT EXISTS idx_audit_events_created_at
        ON audit_events(created_at);

    -- Messages: individual messages within a thread.
    CREATE TABLE IF NOT EXISTS messages (
        id           TEXT    NOT NULL PRIMARY KEY,
        thread_id    TEXT    NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
        role         TEXT    NOT NULL CHECK (role IN ('system', 'user', 'assistant', 'tool')),
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

    CREATE TABLE IF NOT EXISTS workflow_scripts (
        id                  TEXT NOT NULL PRIMARY KEY,
        space_id            TEXT NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
        name                TEXT NOT NULL,
        description         TEXT NOT NULL,
        delivery_grant_json TEXT,
        created_at          TEXT NOT NULL,
        updated_at          TEXT NOT NULL,
        UNIQUE (space_id, name)
    );

    CREATE TABLE IF NOT EXISTS workflow_script_versions (
        id         TEXT NOT NULL PRIMARY KEY,
        script_id  TEXT NOT NULL REFERENCES workflow_scripts(id) ON DELETE CASCADE,
        version    INTEGER NOT NULL,
        source     TEXT NOT NULL,
        created_at TEXT NOT NULL,
        UNIQUE (script_id, version)
    );

    CREATE TABLE IF NOT EXISTS workflow_runs (
        id                  TEXT NOT NULL PRIMARY KEY,
        space_id            TEXT NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
        script_version_id   TEXT REFERENCES workflow_script_versions(id),
        inline_workflow_id  TEXT,
        inline_workflow_name TEXT,
        inline_workflow_version INTEGER,
        carrying_skill      TEXT,
        source              TEXT,
        source_hash         TEXT NOT NULL,
        args_json           TEXT NOT NULL,
        budget_json         TEXT NOT NULL,
        state               TEXT NOT NULL CHECK (state IN ('running', 'completed', 'failed', 'cancelled', 'interrupted')),
        final_output_json   TEXT,
        handover_json       TEXT,
        error_message       TEXT,
        engine_version      TEXT NOT NULL,
        created_at          TEXT NOT NULL,
        completed_at        TEXT,
        dispatch_bound_agent_ids_json TEXT NOT NULL DEFAULT '[]',
        dispatch_bound_task_definition_ids_json TEXT NOT NULL DEFAULT '[]',
        workflow_call_tools_json TEXT,
        inherited_worker_placement_json TEXT
    );

    CREATE INDEX IF NOT EXISTS idx_workflow_runs_space_created
        ON workflow_runs(space_id, created_at DESC);
    CREATE INDEX IF NOT EXISTS idx_workflow_runs_state
        ON workflow_runs(state);

    CREATE TABLE IF NOT EXISTS workflow_journal (
        id                TEXT NOT NULL PRIMARY KEY,
        run_id            TEXT NOT NULL REFERENCES workflow_runs(id) ON DELETE CASCADE,
        seq               INTEGER NOT NULL,
        completed_order   INTEGER,
        kind              TEXT NOT NULL CHECK (kind IN ('agent_call', 'effect', 'log', 'phase')),
        status            TEXT NOT NULL CHECK (status IN ('started', 'completed', 'failed')),
        args_hash         TEXT NOT NULL,
        request_json      TEXT NOT NULL,
        result_json       TEXT,
        error_message     TEXT,
        effect_kind       TEXT,
        idempotency_key   TEXT,
        target_kind       TEXT,
        started_at        TEXT NOT NULL,
        completed_at      TEXT,
        UNIQUE (run_id, seq),
        UNIQUE (run_id, completed_order)
    );

    CREATE INDEX IF NOT EXISTS idx_workflow_journal_run_seq
        ON workflow_journal(run_id, seq);
";

const ADD_WORKSPACE_ATTACHMENT_CANONICAL_STATE: &str = "
    ALTER TABLE workspace_attachments
        ADD COLUMN canonical_path TEXT NOT NULL DEFAULT '';
    UPDATE workspace_attachments
        SET canonical_path = path
        WHERE canonical_path = '';
    ALTER TABLE workspace_attachments
        ADD COLUMN is_missing INTEGER NOT NULL DEFAULT 0;
    CREATE UNIQUE INDEX IF NOT EXISTS idx_workspace_attachments_space_canonical_path
        ON workspace_attachments(space_id, canonical_path);
";

const ADD_WORKSPACE_ATTACHMENT_ACTIVATION: &str = "
    ALTER TABLE workspace_attachments
        ADD COLUMN is_default INTEGER NOT NULL DEFAULT 0;
    CREATE UNIQUE INDEX IF NOT EXISTS idx_workspace_attachments_one_default_per_space
        ON workspace_attachments(space_id)
        WHERE is_default = 1;
    ALTER TABLE sessions
        ADD COLUMN active_workspace_attachment_id TEXT REFERENCES workspace_attachments(id) ON DELETE SET NULL;
    CREATE INDEX IF NOT EXISTS idx_sessions_active_workspace_attachment_id
        ON sessions(active_workspace_attachment_id);
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

const ADD_SESSION_SELECTED_MODEL: &str = "
    ALTER TABLE sessions ADD COLUMN selected_model_id TEXT;
";

const ADD_TOOL_CALL_MODEL_REPLAY: &str = "
    ALTER TABLE tool_calls ADD COLUMN model_arguments TEXT;
    ALTER TABLE tool_calls ADD COLUMN model_result TEXT;
";

const ADD_TOOL_CALL_PROVIDER_ROUND: &str = "
    ALTER TABLE tool_calls ADD COLUMN provider_round INTEGER NOT NULL DEFAULT 0;
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

/// Structural default-space identity.
///
/// The default space is no longer identified by the mutable display name.
/// Delegation chain tracking — nullable parent reference on turns.
const ADD_TURN_PARENT_TURN_ID: &str = "
    ALTER TABLE turns ADD COLUMN parent_turn_id TEXT;

    CREATE INDEX IF NOT EXISTS idx_turns_parent_turn_id
        ON turns(parent_turn_id);
";

const CREATE_ONE_IN_FLIGHT_TURN_INDEX: &str = "
    UPDATE turns
    SET state = 'failed',
        completed_at = COALESCE(completed_at, strftime('%Y-%m-%dT%H:%M:%fZ','now'))
    WHERE state IN ('pending', 'processing', 'awaiting_approval', 'delegating')
      AND EXISTS (
          SELECT 1
          FROM turns AS earlier
          WHERE earlier.thread_id = turns.thread_id
            AND earlier.state IN ('pending', 'processing', 'awaiting_approval', 'delegating')
            AND (
                earlier.created_at < turns.created_at
                OR (earlier.created_at = turns.created_at AND earlier.id < turns.id)
            )
      );

    CREATE UNIQUE INDEX IF NOT EXISTS idx_turns_one_in_flight_per_thread
        ON turns(thread_id)
        WHERE state IN ('pending', 'processing', 'awaiting_approval', 'delegating');
";

/// Existing SQLite tables cannot gain `CHECK` constraints through `ALTER
/// TABLE`, so already-migrated databases get equivalent write-boundary
/// triggers. Fresh databases also receive the triggers; they are redundant
/// with the table checks but keep enforcement consistent across installs.
const CONSTRAIN_MESSAGE_ENUMS: &str = "
    CREATE TRIGGER IF NOT EXISTS trg_messages_role_valid_insert
        BEFORE INSERT ON messages
        WHEN NEW.role NOT IN ('system', 'user', 'assistant', 'tool')
    BEGIN
        SELECT RAISE(ABORT, 'messages.role must be one of system, user, assistant, tool');
    END;

    CREATE TRIGGER IF NOT EXISTS trg_messages_role_valid_update
        BEFORE UPDATE OF role ON messages
        WHEN NEW.role NOT IN ('system', 'user', 'assistant', 'tool')
    BEGIN
        SELECT RAISE(ABORT, 'messages.role must be one of system, user, assistant, tool');
    END;

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

/// The knowledge base: a space's cumulative store of gathered external
/// evidence. The unit of storage is the source observation — one immutable
/// capture of a source — with extracted text as separately versioned
/// derivations so re-extraction never disturbs a version citations pin.
///
/// Unlike the rest of the schema, this family is NOT wipe-and-rebuild:
/// observations are not regenerable (sources change and disappear), so
/// later migrations here may reshape freely but must preserve raw bodies
/// and origin metadata. Raw bodies live as content-addressed files under
/// the space data root; these tables hold metadata and extractions only.
///
/// `extraction_fts` is an external-content FTS5 index over extraction
/// text, kept in sync by triggers so no store method can forget it.
const CREATE_KNOWLEDGE_BASE_TABLES: &str = "
    CREATE TABLE IF NOT EXISTS source_observations (
        id                TEXT NOT NULL PRIMARY KEY,
        space_id          TEXT NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
        origin_url        TEXT,
        origin_local_path TEXT,
        retrieved_at      TEXT NOT NULL,
        body_sha256       TEXT NOT NULL,
        body_bytes        INTEGER NOT NULL,
        media_type        TEXT NOT NULL,
        created_at        TEXT NOT NULL,
        CHECK (origin_url IS NOT NULL OR origin_local_path IS NOT NULL)
    );

    CREATE UNIQUE INDEX IF NOT EXISTS idx_source_observations_space_body
        ON source_observations(space_id, body_sha256);
    CREATE INDEX IF NOT EXISTS idx_source_observations_space
        ON source_observations(space_id, retrieved_at DESC);

    CREATE TABLE IF NOT EXISTS extraction_derivations (
        id             TEXT NOT NULL PRIMARY KEY,
        observation_id TEXT NOT NULL REFERENCES source_observations(id) ON DELETE CASCADE,
        version        INTEGER NOT NULL,
        extracted_text TEXT NOT NULL,
        extractor      TEXT NOT NULL,
        created_at     TEXT NOT NULL,
        UNIQUE (observation_id, version)
    );

    CREATE VIRTUAL TABLE IF NOT EXISTS extraction_fts USING fts5(
        extracted_text,
        content='extraction_derivations',
        content_rowid='rowid'
    );

    CREATE TRIGGER IF NOT EXISTS trg_extraction_fts_insert
    AFTER INSERT ON extraction_derivations
    BEGIN
        INSERT INTO extraction_fts (rowid, extracted_text)
        VALUES (NEW.rowid, NEW.extracted_text);
    END;

    CREATE TRIGGER IF NOT EXISTS trg_extraction_fts_delete
    AFTER DELETE ON extraction_derivations
    BEGIN
        INSERT INTO extraction_fts (extraction_fts, rowid, extracted_text)
        VALUES ('delete', OLD.rowid, OLD.extracted_text);
    END;
";

/// Append-only observation-level judgements. This is a sibling of the
/// knowledge-base tables introduced by migration 043: it never reshapes or
/// mutates the evidence ledger itself.
const CREATE_KNOWLEDGE_CURATION_ASSERTIONS: &str = "
    CREATE TABLE IF NOT EXISTS knowledge_curation_assertions (
        id                    TEXT NOT NULL PRIMARY KEY,
        space_id              TEXT NOT NULL REFERENCES spaces(id),
        observation_id        TEXT NOT NULL REFERENCES source_observations(id),
        kind                   TEXT NOT NULL CHECK (kind IN ('duplicate-of', 'superseded-by', 'stale')),
        target_observation_id TEXT REFERENCES source_observations(id),
        created_at             TEXT NOT NULL,
        CHECK (
            (kind = 'stale' AND target_observation_id IS NULL) OR
            (kind IN ('duplicate-of', 'superseded-by') AND target_observation_id IS NOT NULL)
        ),
        CHECK (target_observation_id IS NULL OR target_observation_id <> observation_id)
    );

    CREATE INDEX IF NOT EXISTS idx_knowledge_curation_observation
        ON knowledge_curation_assertions(observation_id, created_at DESC);

    CREATE TRIGGER IF NOT EXISTS trg_knowledge_curation_no_update
    BEFORE UPDATE ON knowledge_curation_assertions
    BEGIN
        SELECT RAISE(ABORT, 'knowledge curation assertions are append-only');
    END;

    CREATE TRIGGER IF NOT EXISTS trg_knowledge_curation_no_delete
    BEFORE DELETE ON knowledge_curation_assertions
    BEGIN
        SELECT RAISE(ABORT, 'knowledge curation assertions are append-only');
    END;
";

/// Append-only citations pinned to an exact extraction derivation.
const CREATE_KNOWLEDGE_CITATIONS: &str = "
    CREATE TABLE IF NOT EXISTS knowledge_citations (
        id                 TEXT NOT NULL PRIMARY KEY,
        space_id           TEXT NOT NULL REFERENCES spaces(id),
        observation_id     TEXT NOT NULL,
        derivation_version INTEGER NOT NULL,
        start_offset       INTEGER NOT NULL CHECK (start_offset >= 0),
        end_offset         INTEGER NOT NULL CHECK (end_offset > start_offset),
        quoted_text        TEXT NOT NULL CHECK (length(quoted_text) > 0),
        created_at         TEXT NOT NULL,
        FOREIGN KEY (observation_id, derivation_version)
            REFERENCES extraction_derivations(observation_id, version)
    );

    CREATE INDEX IF NOT EXISTS idx_knowledge_citations_observation
        ON knowledge_citations(observation_id, derivation_version, created_at DESC);

    CREATE TRIGGER IF NOT EXISTS trg_knowledge_citations_no_update
    BEFORE UPDATE ON knowledge_citations
    BEGIN
        SELECT RAISE(ABORT, 'knowledge citations are append-only');
    END;

    CREATE TRIGGER IF NOT EXISTS trg_knowledge_citations_no_delete
    BEFORE DELETE ON knowledge_citations
    BEGIN
        SELECT RAISE(ABORT, 'knowledge citations are append-only');
    END;
";

/// Immutable, agent-authored orientation over observations. The deferred
/// circular foreign keys make a card without at least one evidence relation
/// impossible at transaction commit while retaining a proper many-to-many
/// derivation relation.
const CREATE_KNOWLEDGE_ORIENTATION_CARDS: &str = "
    CREATE UNIQUE INDEX idx_source_observations_id_space
        ON source_observations(id, space_id);

    CREATE TABLE orientation_cards (
        id                 TEXT NOT NULL PRIMARY KEY,
        space_id           TEXT NOT NULL REFERENCES spaces(id),
        title              TEXT NOT NULL CHECK (length(trim(title)) > 0),
        content            TEXT NOT NULL CHECK (length(trim(content)) > 0),
        authored_by        TEXT NOT NULL CHECK (length(trim(authored_by)) > 0),
        evidence_anchor_id TEXT NOT NULL UNIQUE,
        created_at         TEXT NOT NULL,
        UNIQUE (id, space_id),
        FOREIGN KEY (evidence_anchor_id, id, space_id)
            REFERENCES orientation_card_observations(id, card_id, space_id)
            DEFERRABLE INITIALLY DEFERRED
    );

    CREATE TABLE orientation_card_observations (
        id             TEXT NOT NULL PRIMARY KEY,
        card_id        TEXT NOT NULL,
        space_id       TEXT NOT NULL,
        observation_id TEXT NOT NULL,
        UNIQUE (id, card_id, space_id),
        UNIQUE (card_id, observation_id),
        FOREIGN KEY (card_id, space_id) REFERENCES orientation_cards(id, space_id)
            DEFERRABLE INITIALLY DEFERRED,
        FOREIGN KEY (observation_id, space_id)
            REFERENCES source_observations(id, space_id)
    );

    CREATE TABLE orientation_card_lifecycle (
        id                  TEXT NOT NULL PRIMARY KEY,
        card_id             TEXT NOT NULL UNIQUE REFERENCES orientation_cards(id),
        action              TEXT NOT NULL CHECK (action IN ('superseded', 'retired')),
        replacement_card_id TEXT REFERENCES orientation_cards(id),
        actor               TEXT NOT NULL CHECK (length(trim(actor)) > 0),
        created_at          TEXT NOT NULL,
        CHECK ((action = 'superseded' AND replacement_card_id IS NOT NULL)
            OR (action = 'retired' AND replacement_card_id IS NULL)),
        CHECK (replacement_card_id IS NULL OR replacement_card_id <> card_id)
    );

    CREATE VIRTUAL TABLE orientation_card_fts USING fts5(
        title, content, content='orientation_cards', content_rowid='rowid'
    );
    CREATE TRIGGER orientation_card_fts_insert AFTER INSERT ON orientation_cards BEGIN
        INSERT INTO orientation_card_fts(rowid, title, content)
        VALUES (new.rowid, new.title, new.content);
    END;
    CREATE TRIGGER orientation_cards_no_update BEFORE UPDATE ON orientation_cards BEGIN
        SELECT RAISE(ABORT, 'orientation cards are immutable');
    END;
    CREATE TRIGGER orientation_cards_no_delete BEFORE DELETE ON orientation_cards BEGIN
        SELECT RAISE(ABORT, 'orientation cards are append-only');
    END;
    CREATE TRIGGER orientation_card_observations_no_update
    BEFORE UPDATE ON orientation_card_observations BEGIN
        SELECT RAISE(ABORT, 'orientation card evidence is immutable');
    END;
    CREATE TRIGGER orientation_card_observations_no_delete
    BEFORE DELETE ON orientation_card_observations BEGIN
        SELECT RAISE(ABORT, 'orientation card evidence is append-only');
    END;
    CREATE TRIGGER orientation_card_lifecycle_no_update
    BEFORE UPDATE ON orientation_card_lifecycle BEGIN
        SELECT RAISE(ABORT, 'orientation card lifecycle is append-only');
    END;
    CREATE TRIGGER orientation_card_lifecycle_no_delete
    BEFORE DELETE ON orientation_card_lifecycle BEGIN
        SELECT RAISE(ABORT, 'orientation card lifecycle is append-only');
    END;
";

/// Durable agent definitions — the bundle that gives a session its entire
/// character, selected at session start by agent id.
///
/// `id` is the stable agent id sessions already carry; `space_id` NULL means
/// a global definition, and a space-scoped row with the same id shadows it.
/// `tool_permissions_json` is a permission overlay that intersects with (and
/// never expands) the resolved config's grants.
const CREATE_AGENT_DEFINITION_TABLE: &str = "
    CREATE TABLE IF NOT EXISTS agent_definitions (
        id                    TEXT NOT NULL,
        space_id              TEXT REFERENCES spaces(id) ON DELETE CASCADE,
        display_name          TEXT NOT NULL,
        description           TEXT NOT NULL DEFAULT '',
        instructions          TEXT NOT NULL DEFAULT '',
        model_id              TEXT,
        tool_permissions_json TEXT,
        skills_json           TEXT NOT NULL DEFAULT '[]',
        output_schema_json    TEXT,
        output_max_retries    INTEGER,
        created_at            TEXT NOT NULL,
        updated_at            TEXT NOT NULL,
        PRIMARY KEY (id, space_id)
    );

    CREATE UNIQUE INDEX IF NOT EXISTS idx_agent_definitions_global
        ON agent_definitions(id) WHERE space_id IS NULL;
    CREATE INDEX IF NOT EXISTS idx_agent_definitions_space
        ON agent_definitions(space_id);
";

const CREATE_TASK_DEFINITION_TABLE: &str = "
    CREATE TABLE IF NOT EXISTS task_definitions (
        id TEXT PRIMARY KEY,
        instructions TEXT NOT NULL DEFAULT '',
        model_id TEXT,
        tool_permissions_json TEXT,
        output_schema_json TEXT,
        output_max_retries INTEGER,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL
    );
";

/// Apply all pending migrations to `conn`.
///
/// Migrations are tracked by name in `_swage_migrations`. Already-applied
/// migrations are skipped, so this is safe to call on every startup.
pub fn run_migrations(conn: &Connection) -> Result<(), StorageError> {
    // Ensure the tracking table exists first.
    conn.execute_batch(CREATE_MIGRATIONS_TABLE)
        .map_err(|e| StorageError::MigrationFailed(e.to_string()))?;

    // SQLite's table-rebuild recipe: run the migration pass with foreign-key
    // enforcement OFF. With it on, dropping a rebuilt table performs an
    // implicit DELETE whose ON DELETE actions fire — a parent-table rebuild
    // would cascade-wipe its children (the workflow journal and event queue,
    // for workflow_runs) while appearing to succeed. The pragma is a no-op
    // inside a transaction, so it must live here, around the per-migration
    // transactions; a foreign_key_check before re-enabling keeps the
    // integrity guarantee the enforcement normally provides.
    conn.execute_batch("PRAGMA foreign_keys=OFF;")
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

        if name == "023_encrypt_managed_secrets_at_rest" {
            super::secret::migrate_managed_secret_storage(conn)?;
        } else if name == "028_deduplicate_attachment_ingestion_content" {
            super::attachment::migrate_attachment_ingested_markdown_dedup(conn)?;
        } else if name == "041_tool_call_model_replay" {
            migrate_tool_call_model_replay(conn)?;
        } else {
            // Apply migration inside a transaction for atomicity.
            conn.execute_batch(&format!("BEGIN; {sql} COMMIT;"))
                .map_err(|e| {
                    StorageError::MigrationFailed(format!("migration '{name}' failed: {e}"))
                })?;
        }

        conn.execute(
            "INSERT INTO _swage_migrations (name, applied_at) VALUES (?1, datetime('now'))",
            params![name],
        )
        .map_err(|e| StorageError::MigrationFailed(e.to_string()))?;
    }

    let violations: i64 = conn
        .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .map_err(|e| StorageError::MigrationFailed(e.to_string()))?;
    if violations > 0 {
        return Err(StorageError::MigrationFailed(format!(
            "{violations} foreign-key violation(s) after migrations; refusing to continue"
        )));
    }
    conn.execute_batch("PRAGMA foreign_keys=ON;")
        .map_err(|e| StorageError::MigrationFailed(e.to_string()))?;

    // Seed the default space if it does not already exist.
    seed_default_space(conn)
}

fn migrate_tool_call_model_replay(conn: &Connection) -> Result<(), StorageError> {
    conn.execute_batch(&format!("BEGIN; {ADD_TOOL_CALL_MODEL_REPLAY} COMMIT;"))
        .map_err(|e| {
            StorageError::MigrationFailed(format!(
                "migration '041_tool_call_model_replay' failed: {e}"
            ))
        })?;

    let legacy_rows = {
        let mut stmt = conn
            .prepare(
                "SELECT id, turn_id, call_id, arguments, result, status
                 FROM tool_calls
                 WHERE model_arguments IS NULL
                    OR (result IS NOT NULL AND model_result IS NULL)",
            )
            .map_err(|e| StorageError::MigrationFailed(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                ))
            })
            .map_err(|e| StorageError::MigrationFailed(e.to_string()))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| StorageError::MigrationFailed(e.to_string()))?
    };

    for (id, turn_id, call_id, arguments, result, status) in legacy_rows {
        let argument_value = serde_json::from_str(&arguments)
            .unwrap_or_else(|_| serde_json::Value::String(arguments.clone()));
        let model_arguments = compact_tool_arguments_for_model(&argument_value);
        let model_result = result.as_ref().map(|result| {
            compact_tool_result_for_model_with_reference(
                result,
                status.as_deref() != Some("success"),
                tool_result_reference(&turn_id, &call_id),
            )
        });

        conn.execute(
            "UPDATE tool_calls
             SET model_arguments = ?1, model_result = ?2
             WHERE id = ?3",
            params![model_arguments, model_result, id],
        )
        .map_err(|e| StorageError::MigrationFailed(e.to_string()))?;
    }

    Ok(())
}

/// Insert the default space on first run. Idempotent via INSERT OR IGNORE.
fn seed_default_space(conn: &Connection) -> Result<(), StorageError> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();

    conn.execute(
        "INSERT OR IGNORE INTO spaces (id, name, is_default, slug, created_at, updated_at)
         VALUES (?1, 'default', 1, 'default', ?2, ?2)",
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

    fn open_migrated_until_exclusive(stop_name: &str) -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory DB");
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        conn.execute_batch(super::CREATE_MIGRATIONS_TABLE)
            .expect("create migrations table");

        for (name, sql) in super::migrations() {
            if name == stop_name {
                break;
            }

            if name == "023_encrypt_managed_secrets_at_rest" {
                super::super::secret::migrate_managed_secret_storage(&conn)
                    .expect("managed secret migration");
            } else if name == "028_deduplicate_attachment_ingestion_content" {
                super::super::attachment::migrate_attachment_ingested_markdown_dedup(&conn)
                    .expect("attachment dedup migration");
            } else {
                conn.execute_batch(&format!("BEGIN; {sql} COMMIT;"))
                    .unwrap_or_else(|error| panic!("migration {name} should apply: {error}"));
            }

            conn.execute(
                "INSERT INTO _swage_migrations (name, applied_at) VALUES (?1, datetime('now'))",
                rusqlite::params![name],
            )
            .expect("record applied migration");
        }

        super::seed_default_space(&conn).expect("seed default space");
        conn
    }

    // Convenience: insert a session using the new schema (requires agent_id).
    fn insert_session(conn: &Connection, space_id: &str, now: &str) -> String {
        let session_id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO sessions (id, space_id, agent_id, created_at, updated_at)
             VALUES (?1,?2,'agent-test',?3,?3)",
            rusqlite::params![session_id, space_id, now],
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

    fn default_space_id(conn: &Connection) -> String {
        conn.query_row("SELECT id FROM spaces WHERE name='default'", [], |r| {
            r.get(0)
        })
        .expect("default space")
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
        let space_id = default_space_id(&conn);
        let now = chrono::Utc::now().to_rfc3339();

        let session_id = insert_session(&conn, &space_id, &now);
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
        let space_id = default_space_id(&conn);
        let now = chrono::Utc::now().to_rfc3339();

        let session_id = insert_session(&conn, &space_id, &now);
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
    // Spaces: slug column
    // ---------------------------------------------------------------------------

    #[test]
    fn slug_column_exists_after_migration() {
        let conn = open_migrated();
        let col_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('spaces') WHERE name='slug'",
                [],
                |r| r.get(0),
            )
            .expect("pragma_table_info query");
        assert_eq!(col_count, 1, "slug column should exist after migration");
    }

    #[test]
    fn default_space_has_slug_default() {
        let conn = open_migrated();
        let slug: String = conn
            .query_row("SELECT slug FROM spaces WHERE name = 'default'", [], |r| {
                r.get(0)
            })
            .expect("default space should exist with slug");
        assert_eq!(slug, "default", "default space slug should be 'default'");
    }

    #[test]
    fn slug_uniqueness_constraint_enforced() {
        let conn = open_migrated();
        let now = chrono::Utc::now().to_rfc3339();

        conn.execute(
            "INSERT INTO spaces (id, name, slug, created_at, updated_at)
             VALUES (?1, 'alpha', 'alpha', ?2, ?2)",
            rusqlite::params![uuid::Uuid::new_v4().to_string(), now],
        )
        .expect("first insert should succeed");

        let result = conn.execute(
            "INSERT INTO spaces (id, name, slug, created_at, updated_at)
             VALUES (?1, 'beta', 'alpha', ?2, ?2)",
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
                "SELECT COUNT(*) FROM pragma_table_info('spaces') WHERE name='slug'",
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

        let space_id = default_space_id(&conn);
        let now = chrono::Utc::now().to_rfc3339();
        let session_id = insert_session(&conn, &space_id, &now);

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
        let space_id = default_space_id(&conn);
        let now = chrono::Utc::now().to_rfc3339();
        let session_id = insert_session(&conn, &space_id, &now);
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
    fn messages_role_rejects_unknown_values() {
        let conn = open_migrated();
        let space_id = default_space_id(&conn);
        let now = chrono::Utc::now().to_rfc3339();
        let session_id = insert_session(&conn, &space_id, &now);
        let thread_id = insert_thread(&conn, &session_id, &now);

        conn.execute(
            "INSERT INTO messages
             (id, thread_id, role, content, position, agent_id, created_at)
             VALUES (?1, ?2, 'assistant', 'ok', 0, 'agent-test', ?3)",
            rusqlite::params![uuid::Uuid::new_v4().to_string(), thread_id, now],
        )
        .expect("known role should insert");

        let result = conn.execute(
            "INSERT INTO messages
             (id, thread_id, role, content, position, agent_id, created_at)
             VALUES (?1, ?2, 'moderator', 'bad', 1, 'agent-test', ?3)",
            rusqlite::params![uuid::Uuid::new_v4().to_string(), thread_id, now],
        );
        assert!(result.is_err(), "unknown messages.role should fail closed");
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
    fn tool_calls_replay_columns_exist() {
        let conn = open_migrated();
        for column in &["model_arguments", "model_result", "provider_round"] {
            let col_count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('tool_calls') WHERE name=?1",
                    rusqlite::params![column],
                    |r| r.get(0),
                )
                .expect("pragma_table_info query");
            assert_eq!(col_count, 1, "tool_calls.{column} column should exist");
        }
    }

    #[test]
    fn tool_call_model_replay_migration_backfills_legacy_rows() {
        let conn = open_migrated_until_exclusive("041_tool_call_model_replay");
        let space_id = default_space_id(&conn);
        let now = chrono::Utc::now().to_rfc3339();
        let session_id = insert_session(&conn, &space_id, &now);
        let thread_id = insert_thread(&conn, &session_id, &now);
        let turn_id = insert_turn(&conn, &thread_id, &now);
        let call_id = "legacy_call";

        conn.execute(
            "INSERT INTO tool_calls
             (id, turn_id, call_id, tool, arguments, agent_id, result, status,
              duration_ms, truncated, total_bytes, position, created_at)
             VALUES (?1, ?2, ?3, 'shell', ?4, 'agent-test', ?5, 'error',
                     12, 0, 17, 0, ?6)",
            rusqlite::params![
                uuid::Uuid::new_v4().to_string(),
                &turn_id,
                call_id,
                r#"{"cmd":"missing-command"}"#,
                "command not found",
                now,
            ],
        )
        .expect("insert legacy tool call");

        super::run_migrations(&conn).expect("remaining migrations should apply");

        let (model_arguments, model_result): (String, String) = conn
            .query_row(
                "SELECT model_arguments, model_result
                 FROM tool_calls
                 WHERE call_id = ?1",
                rusqlite::params![call_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("select backfilled replay columns");

        let arguments: swage_core::CompactToolReplay =
            serde_json::from_str(&model_arguments).expect("arguments compact replay");
        assert_eq!(arguments.kind, "tool_arguments");
        assert_eq!(arguments.preview, r#"{"cmd":"missing-command"}"#);

        let result: swage_core::CompactToolReplay =
            serde_json::from_str(&model_result).expect("result compact replay");
        assert_eq!(result.kind, "tool_result");
        assert_eq!(result.preview, "command not found");
        assert_eq!(result.is_error, Some(true));
        let expected_ref = swage_core::tool_result_reference(&turn_id, call_id);
        assert_eq!(
            result.full_result_ref.as_deref(),
            Some(expected_ref.as_str())
        );
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
        let space_id = default_space_id(&conn);
        let now = chrono::Utc::now().to_rfc3339();
        let session_id = insert_session(&conn, &space_id, &now);
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
        let space_id = default_space_id(&conn);
        let now = chrono::Utc::now().to_rfc3339();
        let session_id = insert_session(&conn, &space_id, &now);
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
        let space_id = default_space_id(&conn);
        let now = chrono::Utc::now().to_rfc3339();
        let session_id = insert_session(&conn, &space_id, &now);
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

    #[test]
    fn deleting_turn_orphans_but_keeps_messages() {
        let conn = open_migrated();
        let space_id = default_space_id(&conn);
        let now = chrono::Utc::now().to_rfc3339();
        let session_id = insert_session(&conn, &space_id, &now);
        let thread_id = insert_thread(&conn, &session_id, &now);
        let turn_id = insert_turn(&conn, &thread_id, &now);
        let message_id = uuid::Uuid::new_v4().to_string();

        conn.execute(
            "INSERT INTO messages (id, thread_id, turn_id, role, content, position, agent_id, created_at)
             VALUES (?1,?2,?3,'assistant','hello',0,'agent-test',?4)",
            rusqlite::params![message_id, thread_id, turn_id, now],
        )
        .expect("insert message");

        conn.execute("DELETE FROM turns WHERE id=?1", rusqlite::params![turn_id])
            .expect("delete turn");

        let retained_turn_id: Option<String> = conn
            .query_row(
                "SELECT turn_id FROM messages WHERE id=?1",
                rusqlite::params![message_id],
                |r| r.get(0),
            )
            .expect("message should survive turn deletion");
        assert!(
            retained_turn_id.is_none(),
            "deleting a turn should retain its messages with turn_id set to NULL"
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
        let space_id = default_space_id(&conn);
        let now = chrono::Utc::now().to_rfc3339();
        let session_id = insert_session(&conn, &space_id, &now);
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
        let space_id = default_space_id(&conn);
        let now = chrono::Utc::now().to_rfc3339();
        let session_id = insert_session(&conn, &space_id, &now);
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
        let space_id = default_space_id(&conn);
        let now = chrono::Utc::now().to_rfc3339();
        let session_id = insert_session(&conn, &space_id, &now);
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
        let space_id = default_space_id(&conn);
        let now = chrono::Utc::now().to_rfc3339();

        let session_id = insert_session(&conn, &space_id, &now);
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
    fn approval_memory_table_created() {
        let conn = open_migrated();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM approval_memory", [], |r| r.get(0))
            .expect("approval_memory table should exist after migration");
        assert_eq!(count, 0);
    }
}
