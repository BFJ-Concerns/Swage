-- Every object in the store, in dependency order. This file is the whole
-- schema definition: a store is built from it in one transaction, and an
-- existing store is refused on open unless its definitions match it.

CREATE TABLE turn_queue (
    id          TEXT    NOT NULL PRIMARY KEY,
    session_id  TEXT    NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    thread_id   TEXT    NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    seq         INTEGER NOT NULL,
    origin      TEXT    NOT NULL CHECK (origin IN ('user', 'assistant', 'tool', 'delegation', 'schedule', 'workflow', 'platform')),
    content     TEXT    NOT NULL,
    attachments TEXT    NOT NULL DEFAULT '[]',
    created_at  TEXT    NOT NULL,
    delivery_targets TEXT NOT NULL DEFAULT '[]',
    schedule_run_id TEXT REFERENCES schedule_runs(id) ON DELETE CASCADE,
    attended    INTEGER NOT NULL CHECK (attended IN (0, 1)),
    client_id   TEXT,
    UNIQUE (session_id, seq)
);

CREATE TABLE turn_queue_channel_messages (
    queued_id TEXT NOT NULL REFERENCES turn_queue(id) ON DELETE CASCADE,
    message_id TEXT NOT NULL UNIQUE REFERENCES channel_inbound_messages(id) ON DELETE CASCADE
);

CREATE TABLE topics (
    id             TEXT NOT NULL PRIMARY KEY,
    name           TEXT NOT NULL UNIQUE,
    description    TEXT NOT NULL DEFAULT '',
    slug           TEXT NOT NULL DEFAULT '',
    is_general     INTEGER NOT NULL DEFAULT 0,
    created_at     TEXT NOT NULL,
    updated_at     TEXT NOT NULL,
    tags TEXT NOT NULL DEFAULT '[]'
);

CREATE UNIQUE INDEX idx_topics_slug ON topics(slug);

CREATE UNIQUE INDEX idx_topics_single_general
    ON topics(is_general) WHERE is_general = 1;

CREATE TABLE devices (
    device_id              TEXT NOT NULL PRIMARY KEY,
    label                TEXT,
    kind                 TEXT NOT NULL,
    shared_token         TEXT NOT NULL,
    workspace_root       TEXT NOT NULL,
    workspace_roots_json TEXT NOT NULL DEFAULT '[]',
    capabilities_json    TEXT NOT NULL,
    registered_at        TEXT NOT NULL
);

CREATE TABLE workspace_attachments (
    id               TEXT NOT NULL PRIMARY KEY,
    topic_id         TEXT NOT NULL REFERENCES topics(id) ON DELETE CASCADE,
    label            TEXT NOT NULL,
    path             TEXT NOT NULL,
    permissions_json TEXT NOT NULL,
    is_trusted       INTEGER NOT NULL DEFAULT 0,
    created_at       TEXT NOT NULL,
    updated_at       TEXT NOT NULL,
    canonical_path TEXT NOT NULL DEFAULT '',
    is_missing INTEGER NOT NULL DEFAULT 0,
    is_default INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX idx_workspace_attachments_topic_id
    ON workspace_attachments(topic_id);

CREATE UNIQUE INDEX idx_workspace_attachments_topic_path
    ON workspace_attachments(topic_id, path);

CREATE TABLE sessions (
    id         TEXT NOT NULL PRIMARY KEY,
    topic_id TEXT NOT NULL REFERENCES topics(id) ON DELETE CASCADE,
    agent_id   TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    metadata_json TEXT,
    active_workspace_attachment_id TEXT REFERENCES workspace_attachments(id) ON DELETE SET NULL,
    selected_model_id TEXT,
    origin_kind TEXT NOT NULL DEFAULT 'direct',
    origin_source_id TEXT,
    archived_at TEXT
);

CREATE INDEX idx_sessions_topic_id ON sessions(topic_id);

CREATE INDEX idx_sessions_agent_id   ON sessions(agent_id);

CREATE TABLE channel_link_codes (
    code TEXT NOT NULL PRIMARY KEY,
    integration TEXT NOT NULL,
    topic_id TEXT,
    agent_id TEXT,
    home_target_json TEXT,
    expires_at TEXT NOT NULL
);

CREATE TABLE channel_links (
    integration      TEXT NOT NULL,
    account          TEXT NOT NULL,
    conversation     TEXT NOT NULL,
    topic_id         TEXT NOT NULL REFERENCES topics(id) ON DELETE CASCADE,
    agent_id         TEXT NOT NULL,
    session_id       TEXT REFERENCES sessions(id) ON DELETE SET NULL,
    home_target_json TEXT,
    linked_at        TEXT NOT NULL,
    updated_at       TEXT NOT NULL,
    PRIMARY KEY (integration, account, conversation)
);

CREATE INDEX idx_channel_links_topic_id
    ON channel_links(topic_id);

CREATE INDEX idx_channel_links_session_id
    ON channel_links(session_id);

CREATE TABLE schedules (
    id             TEXT NOT NULL PRIMARY KEY,
    message        TEXT NOT NULL,
    status         TEXT NOT NULL,
    kind           TEXT NOT NULL,
    expression     TEXT NOT NULL,
    session_policy TEXT NOT NULL,
    topic_id       TEXT REFERENCES topics(id) ON DELETE CASCADE,
    agent_id       TEXT,
    session_id     TEXT REFERENCES sessions(id) ON DELETE CASCADE,
    -- Provenance: 'user' or 'agent'. creator_agent_id names the agent
    -- for agent-created schedules, and is NULL for user-created ones.
    creator_kind     TEXT NOT NULL,
    creator_agent_id TEXT,
    delivery_json  TEXT NOT NULL,
    gate_model     TEXT,
    gate_criteria  TEXT,
    gate_observations TEXT,
    next_run_at    TEXT,
    last_run_at    TEXT,
    created_at     TEXT NOT NULL,
    updated_at     TEXT NOT NULL
);

CREATE INDEX idx_schedules_next_run
    ON schedules(status, next_run_at);

CREATE INDEX idx_schedules_session_id
    ON schedules(session_id);

CREATE TABLE schedule_runs (
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

CREATE INDEX idx_schedule_runs_schedule_id
    ON schedule_runs(schedule_id, created_at);

CREATE INDEX idx_schedule_runs_state
    ON schedule_runs(schedule_id, state);

CREATE UNIQUE INDEX idx_schedule_runs_one_active
    ON schedule_runs(schedule_id)
    WHERE state IN ('pending', 'running');

CREATE TABLE threads (
    id         TEXT NOT NULL PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    agent_id   TEXT NOT NULL,
    state      TEXT NOT NULL DEFAULT 'idle',
    codex_thread_id TEXT,
    codex_tools_json TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX idx_threads_session_id ON threads(session_id);

CREATE INDEX idx_threads_agent_id   ON threads(agent_id);

CREATE TABLE turns (
    id                 TEXT    NOT NULL PRIMARY KEY,
    thread_id          TEXT    NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    agent_id           TEXT    NOT NULL,
    user_input         TEXT,
    assistant_response TEXT,
    input_tokens       INTEGER,
    output_tokens      INTEGER,
    state              TEXT    NOT NULL DEFAULT 'pending',
    created_at         TEXT    NOT NULL,
    completed_at       TEXT,
    cost_microcents INTEGER,
    structured_output TEXT,
    parent_turn_id TEXT,
    inbound_audit TEXT,
    delegated_input_tokens INTEGER,
    delegated_output_tokens INTEGER,
    isolation_kind TEXT,
    isolation_detail TEXT,
    failure_message TEXT
);

CREATE INDEX idx_turns_thread_id ON turns(thread_id);

CREATE INDEX idx_turns_agent_id  ON turns(agent_id);

CREATE INDEX idx_turns_state     ON turns(state);

CREATE UNIQUE INDEX idx_turns_one_in_flight_per_thread
    ON turns(thread_id)
    WHERE state IN ('pending', 'processing', 'awaiting_approval', 'delegating');

CREATE TABLE channel_inbound_messages (
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

CREATE INDEX idx_channel_inbound_pending_origin
    ON channel_inbound_messages(state, integration, account, conversation, received_at);

CREATE INDEX idx_channel_inbound_turn_id
    ON channel_inbound_messages(turn_id);

CREATE TABLE channel_cursors (
    integration TEXT NOT NULL,
    account     TEXT NOT NULL,
    cursor      TEXT NOT NULL,
    updated_at  TEXT NOT NULL,
    PRIMARY KEY (integration, account)
);

CREATE TABLE delivery_records (
    id           TEXT NOT NULL PRIMARY KEY,
    session_id   TEXT REFERENCES sessions(id) ON DELETE CASCADE,
    run_id       TEXT REFERENCES schedule_runs(id) ON DELETE SET NULL,
    turn_id      TEXT REFERENCES turns(id) ON DELETE SET NULL,
    target_json  TEXT NOT NULL,
    content      TEXT NOT NULL,
    status       TEXT NOT NULL CHECK (status IN ('sent', 'failed', 'not_sent', 'not_delivered')),
    error        TEXT,
    delivered_at TEXT NOT NULL
);

CREATE INDEX idx_delivery_records_session_id
    ON delivery_records(session_id, delivered_at);

CREATE TABLE audit_events (
    id           TEXT NOT NULL PRIMARY KEY,
    session_id   TEXT REFERENCES sessions(id) ON DELETE CASCADE,
    entity_kind  TEXT NOT NULL,
    entity_id    TEXT NOT NULL,
    event_kind   TEXT NOT NULL,
    summary      TEXT NOT NULL,
    metadata_json TEXT NOT NULL,
    created_at   TEXT NOT NULL
);

CREATE INDEX idx_audit_events_session_id
    ON audit_events(session_id, created_at);

CREATE INDEX idx_audit_events_entity
    ON audit_events(entity_kind, entity_id, created_at);

CREATE INDEX idx_audit_events_created_at
    ON audit_events(created_at);

CREATE TABLE messages (
    id           TEXT    NOT NULL PRIMARY KEY,
    thread_id    TEXT    NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    role         TEXT    NOT NULL CHECK (role IN ('system', 'user', 'assistant', 'tool')),
    -- Who authored the row: a person, the model, a tool, or the platform
    -- path that composed it. Keyed by the UI's author label; a row with
    -- no recorded origin is refused rather than guessed from its role.
    origin       TEXT    NOT NULL CHECK (origin IN ('user', 'assistant', 'tool', 'delegation', 'schedule', 'workflow', 'platform')),
    content      TEXT    NOT NULL,
    -- Content parts submitted beside the text, as a JSON array of the
    -- client wire's tagged attachment values; '[]' for text-only rows.
    attachments  TEXT    NOT NULL DEFAULT '[]',
    position     INTEGER NOT NULL,
    agent_id     TEXT    NOT NULL,
    tool_call_id TEXT,
    tool_name    TEXT,
    created_at   TEXT    NOT NULL,
    turn_id TEXT REFERENCES turns(id) ON DELETE SET NULL
);

CREATE INDEX idx_messages_thread_id ON messages(thread_id);

CREATE INDEX idx_messages_agent_id  ON messages(agent_id);

CREATE UNIQUE INDEX idx_messages_thread_position
    ON messages(thread_id, position);

CREATE TABLE tool_calls (
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
    created_at     TEXT    NOT NULL,
    model_arguments TEXT,
    model_result TEXT,
    provider_round INTEGER NOT NULL DEFAULT 0,
    display_summary TEXT
);

CREATE INDEX idx_tool_calls_turn_id  ON tool_calls(turn_id);

CREATE INDEX idx_tool_calls_call_id  ON tool_calls(call_id);

CREATE INDEX idx_tool_calls_agent_id ON tool_calls(agent_id);

CREATE TABLE workflow_scripts (
    id                  TEXT NOT NULL PRIMARY KEY,
    topic_id            TEXT NOT NULL REFERENCES topics(id) ON DELETE CASCADE,
    name                TEXT NOT NULL,
    description         TEXT NOT NULL,
    delivery_grant_json TEXT,
    captured_sources_grant INTEGER NOT NULL DEFAULT 0,
    authoring_session_id TEXT,
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL,
    UNIQUE (topic_id, name)
);

CREATE TABLE workflow_script_versions (
    id         TEXT NOT NULL PRIMARY KEY,
    script_id  TEXT NOT NULL REFERENCES workflow_scripts(id) ON DELETE CASCADE,
    version    INTEGER NOT NULL,
    source     TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE (script_id, version)
);

CREATE TABLE context_snapshots (
    turn_id     TEXT NOT NULL PRIMARY KEY REFERENCES turns(id) ON DELETE CASCADE,
    report_json TEXT NOT NULL,
    created_at  TEXT NOT NULL
);

CREATE TABLE active_skills (
    session_id    TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    skill_name    TEXT NOT NULL,
    activated_at  TEXT NOT NULL,
    PRIMARY KEY (session_id, skill_name)
);

CREATE TABLE approval_requests (
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

CREATE INDEX idx_approval_requests_turn_id  ON approval_requests(turn_id);

CREATE INDEX idx_approval_requests_agent_id ON approval_requests(agent_id);

CREATE INDEX idx_approval_requests_state    ON approval_requests(state);

CREATE TABLE agent_messages (
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

CREATE INDEX idx_agent_messages_recipient_status
    ON agent_messages(recipient_agent_id, status);

CREATE INDEX idx_agent_messages_correlation
    ON agent_messages(correlation_id);

CREATE INDEX idx_agent_messages_sender
    ON agent_messages(sender_agent_id);

CREATE INDEX idx_turns_parent_turn_id
    ON turns(parent_turn_id);

CREATE TABLE approval_memory (
    session_id           TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    tool_name            TEXT NOT NULL,
    request_fingerprint  TEXT NOT NULL,
    created_at           TEXT NOT NULL,
    PRIMARY KEY (session_id, tool_name, request_fingerprint)
);

CREATE INDEX idx_approval_memory_session_id ON approval_memory(session_id);

CREATE TABLE thread_branches (
    child_thread_id          TEXT NOT NULL PRIMARY KEY REFERENCES threads(id) ON DELETE CASCADE,
    parent_thread_id         TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    branched_from_message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    created_at               TEXT NOT NULL
);

CREATE INDEX idx_thread_branches_parent_thread_id ON thread_branches(parent_thread_id);

CREATE INDEX idx_messages_turn_id ON messages(turn_id);

CREATE TRIGGER trg_turns_state_audit
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

CREATE TRIGGER trg_approval_state_audit
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

CREATE TRIGGER trg_schedule_runs_state_audit
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

CREATE TRIGGER trg_delivery_record_audit
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

CREATE TRIGGER trg_channel_inbound_state_audit
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

CREATE TABLE secrets (
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

CREATE INDEX idx_secrets_scope
    ON secrets(scope_kind, scope_id, created_at);

CREATE INDEX idx_secrets_material_kind
    ON secrets(material_kind, created_at);

CREATE INDEX idx_secrets_status
    ON secrets(status, created_at);

CREATE TABLE managed_secret_values (
    secret_id       TEXT NOT NULL PRIMARY KEY REFERENCES secrets(id) ON DELETE CASCADE,
    algorithm       TEXT NOT NULL,
    key_source      TEXT NOT NULL,
    nonce_b64       TEXT NOT NULL,
    ciphertext_b64  TEXT NOT NULL,
    updated_at      TEXT NOT NULL
);

CREATE TABLE attachments (
    id                TEXT NOT NULL PRIMARY KEY,
    scope_kind        TEXT NOT NULL,
    session_id        TEXT REFERENCES sessions(id) ON DELETE CASCADE,
    topic_id        TEXT REFERENCES topics(id) ON DELETE CASCADE,
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

CREATE INDEX idx_attachments_session_id
    ON attachments(session_id, created_at);

CREATE INDEX idx_attachments_topic_id
    ON attachments(topic_id, created_at);

CREATE TABLE background_jobs (
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

CREATE INDEX idx_background_jobs_state
    ON background_jobs(state, updated_at);

CREATE INDEX idx_background_jobs_attachment_id
    ON background_jobs(attachment_id, created_at);

CREATE TABLE materials (
    id                   TEXT NOT NULL PRIMARY KEY,
    scope_kind           TEXT NOT NULL,
    session_id           TEXT REFERENCES sessions(id) ON DELETE CASCADE,
    topic_id           TEXT REFERENCES topics(id) ON DELETE CASCADE,
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

CREATE INDEX idx_materials_session_id
    ON materials(session_id, created_at);

CREATE INDEX idx_materials_topic_id
    ON materials(topic_id, created_at);

CREATE UNIQUE INDEX idx_materials_source_attachment_id
    ON materials(source_attachment_id)
    WHERE source_attachment_id IS NOT NULL;

CREATE UNIQUE INDEX idx_workspace_attachments_topic_canonical_path
    ON workspace_attachments(topic_id, canonical_path);

CREATE UNIQUE INDEX idx_workspace_attachments_one_default_per_topic
    ON workspace_attachments(topic_id)
    WHERE is_default = 1;

CREATE INDEX idx_sessions_active_workspace_attachment_id
    ON sessions(active_workspace_attachment_id);

CREATE TRIGGER trg_messages_role_valid_insert
    BEFORE INSERT ON messages
    WHEN NEW.role NOT IN ('system', 'user', 'assistant', 'tool')
BEGIN
    SELECT RAISE(ABORT, 'messages.role must be one of system, user, assistant, tool');
END;

CREATE TRIGGER trg_messages_role_valid_update
    BEFORE UPDATE OF role ON messages
    WHEN NEW.role NOT IN ('system', 'user', 'assistant', 'tool')
BEGIN
    SELECT RAISE(ABORT, 'messages.role must be one of system, user, assistant, tool');
END;

CREATE TABLE source_observations (
    id                TEXT NOT NULL PRIMARY KEY,
    topic_id          TEXT NOT NULL REFERENCES topics(id) ON DELETE CASCADE,
    origin_url        TEXT,
    origin_local_path TEXT,
    retrieved_at      TEXT NOT NULL,
    body_sha256       TEXT NOT NULL,
    body_bytes        INTEGER NOT NULL,
    media_type        TEXT NOT NULL,
    capture_scope     TEXT,
    created_at        TEXT NOT NULL,
    CHECK (origin_url IS NOT NULL OR origin_local_path IS NOT NULL)
);

CREATE UNIQUE INDEX idx_source_observations_topic_body
    ON source_observations(topic_id, body_sha256);

CREATE INDEX idx_source_observations_topic
    ON source_observations(topic_id, retrieved_at DESC);

CREATE TABLE source_captures (
    id                TEXT NOT NULL PRIMARY KEY,
    observation_id    TEXT NOT NULL REFERENCES source_observations(id) ON DELETE CASCADE,
    topic_id          TEXT NOT NULL REFERENCES topics(id) ON DELETE CASCADE,
    origin_url        TEXT,
    origin_local_path TEXT,
    capture_scope     TEXT NOT NULL,
    captured_at       TEXT NOT NULL,
    CHECK (origin_url IS NOT NULL OR origin_local_path IS NOT NULL)
);

CREATE UNIQUE INDEX idx_source_captures_scope_origin
    ON source_captures(
        topic_id,
        capture_scope,
        COALESCE(origin_url, ''),
        COALESCE(origin_local_path, '')
    );

CREATE INDEX idx_source_captures_topic_scope
    ON source_captures(topic_id, capture_scope);

CREATE TABLE extraction_derivations (
    id             TEXT NOT NULL PRIMARY KEY,
    observation_id TEXT NOT NULL REFERENCES source_observations(id) ON DELETE CASCADE,
    version        INTEGER NOT NULL,
    extracted_text TEXT NOT NULL,
    extractor      TEXT NOT NULL,
    created_at     TEXT NOT NULL,
    UNIQUE (observation_id, version)
);

CREATE VIRTUAL TABLE extraction_fts USING fts5(
    extracted_text,
    content='extraction_derivations',
    content_rowid='rowid'
);

CREATE TRIGGER trg_extraction_fts_insert
AFTER INSERT ON extraction_derivations
BEGIN
    INSERT INTO extraction_fts (rowid, extracted_text)
    VALUES (NEW.rowid, NEW.extracted_text);
END;

CREATE TRIGGER trg_extraction_fts_delete
AFTER DELETE ON extraction_derivations
BEGIN
    INSERT INTO extraction_fts (extraction_fts, rowid, extracted_text)
    VALUES ('delete', OLD.rowid, OLD.extracted_text);
END;

CREATE TABLE agent_definitions (
    id                    TEXT NOT NULL,
    topic_id              TEXT REFERENCES topics(id) ON DELETE CASCADE,
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
    PRIMARY KEY (id, topic_id)
);

CREATE UNIQUE INDEX idx_agent_definitions_global
    ON agent_definitions(id) WHERE topic_id IS NULL;

CREATE INDEX idx_agent_definitions_topic
    ON agent_definitions(topic_id);

CREATE TABLE knowledge_curation_assertions (
    id                    TEXT NOT NULL PRIMARY KEY,
    topic_id              TEXT NOT NULL REFERENCES topics(id),
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

CREATE INDEX idx_knowledge_curation_observation
    ON knowledge_curation_assertions(observation_id, created_at DESC);

CREATE TRIGGER trg_knowledge_curation_no_update
BEFORE UPDATE ON knowledge_curation_assertions
BEGIN
    SELECT RAISE(ABORT, 'knowledge curation assertions are append-only');
END;

CREATE TRIGGER trg_knowledge_curation_no_delete
BEFORE DELETE ON knowledge_curation_assertions
BEGIN
    SELECT RAISE(ABORT, 'knowledge curation assertions are append-only');
END;

CREATE TABLE knowledge_citations (
    id                 TEXT NOT NULL PRIMARY KEY,
    topic_id           TEXT NOT NULL REFERENCES topics(id),
    observation_id     TEXT NOT NULL,
    derivation_version INTEGER NOT NULL,
    start_offset       INTEGER NOT NULL CHECK (start_offset >= 0),
    end_offset         INTEGER NOT NULL CHECK (end_offset > start_offset),
    quoted_text        TEXT NOT NULL CHECK (length(quoted_text) > 0),
    created_at         TEXT NOT NULL,
    FOREIGN KEY (observation_id, derivation_version)
        REFERENCES extraction_derivations(observation_id, version)
);

CREATE INDEX idx_knowledge_citations_observation
    ON knowledge_citations(observation_id, derivation_version, created_at DESC);

CREATE TRIGGER trg_knowledge_citations_no_update
BEFORE UPDATE ON knowledge_citations
BEGIN
    SELECT RAISE(ABORT, 'knowledge citations are append-only');
END;

CREATE TRIGGER trg_knowledge_citations_no_delete
BEFORE DELETE ON knowledge_citations
BEGIN
    SELECT RAISE(ABORT, 'knowledge citations are append-only');
END;

CREATE UNIQUE INDEX idx_source_observations_id_topic
    ON source_observations(id, topic_id);

CREATE TABLE orientation_cards (
    id                 TEXT NOT NULL PRIMARY KEY,
    topic_id           TEXT NOT NULL REFERENCES topics(id),
    title              TEXT NOT NULL CHECK (length(trim(title)) > 0),
    content            TEXT NOT NULL CHECK (length(trim(content)) > 0),
    authored_by        TEXT NOT NULL CHECK (length(trim(authored_by)) > 0),
    evidence_anchor_id TEXT NOT NULL UNIQUE,
    created_at         TEXT NOT NULL,
    UNIQUE (id, topic_id),
    FOREIGN KEY (evidence_anchor_id, id, topic_id)
        REFERENCES orientation_card_observations(id, card_id, topic_id)
        DEFERRABLE INITIALLY DEFERRED
);

CREATE TABLE orientation_card_observations (
    id             TEXT NOT NULL PRIMARY KEY,
    card_id        TEXT NOT NULL,
    topic_id       TEXT NOT NULL,
    observation_id TEXT NOT NULL,
    UNIQUE (id, card_id, topic_id),
    UNIQUE (card_id, observation_id),
    FOREIGN KEY (card_id, topic_id) REFERENCES orientation_cards(id, topic_id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (observation_id, topic_id)
        REFERENCES source_observations(id, topic_id)
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

CREATE TABLE workflow_run_acknowledgements (
    run_id TEXT NOT NULL REFERENCES workflow_runs(id) ON DELETE CASCADE,
    consumer TEXT NOT NULL,
    acknowledged_at TEXT NOT NULL,
    PRIMARY KEY (consumer, run_id)
);

CREATE TABLE workflow_run_events (
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

CREATE INDEX idx_workflow_run_events_pending
    ON workflow_run_events(run_id, seq)
    WHERE delivered_at IS NULL;

CREATE TABLE "workflow_runs" (
    id                   TEXT NOT NULL PRIMARY KEY,
    topic_id             TEXT NOT NULL REFERENCES topics(id) ON DELETE CASCADE,
    script_version_id    TEXT REFERENCES workflow_script_versions(id),
    inline_workflow_id   TEXT,
    inline_workflow_name TEXT,
    inline_workflow_version INTEGER,
    carrying_skill       TEXT,
    task                 TEXT,
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
    inherited_worker_placement_json TEXT,
    -- The terminal account a run leaves when it ends by any path: the
    -- ending, the partition of its workers, and the completed work
    -- recoverable from a run that never returned. Generic JSON like
    -- every other run blob; the contract lives in `swage-protocol` and
    -- is enforced at the wire boundary.
    terminal_account_json TEXT,
    -- Every gateway interruption the run lived through, as a JSON list
    -- of { detected_at, run_state } entries written by the boot that
    -- found it running or parked with no gateway alive to serve it.
    interruptions_json   TEXT NOT NULL DEFAULT '[]',
    dispatching_session_id TEXT
);

CREATE INDEX idx_workflow_runs_topic_created
    ON workflow_runs(topic_id, created_at DESC);

CREATE INDEX idx_workflow_runs_state
    ON workflow_runs(state);

CREATE UNIQUE INDEX idx_workflow_runs_parent_call
    ON workflow_runs(parent_run_id, parent_seq)
    WHERE parent_run_id IS NOT NULL;

CREATE TABLE task_definitions (
    id TEXT PRIMARY KEY,
    instructions TEXT NOT NULL DEFAULT '',
    model_id TEXT,
    tool_permissions_json TEXT,
    output_schema_json TEXT,
    output_max_retries INTEGER,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE session_variables (
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    value TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (session_id, name)
);

CREATE TABLE workflow_run_holds (
    id                TEXT NOT NULL PRIMARY KEY,
    run_id            TEXT NOT NULL REFERENCES workflow_runs(id) ON DELETE CASCADE,
    journal_entry_id  TEXT NOT NULL UNIQUE,
    seq               INTEGER NOT NULL,
    content           TEXT NOT NULL,
    verdicts_json     TEXT NOT NULL,
    state             TEXT NOT NULL CHECK (state IN ('open', 'answered', 'closed')),
    verdict           TEXT,
    answer_surface    TEXT,
    answer_agent_id   TEXT,
    answer_session_id TEXT,
    answer_turn_id    TEXT,
    raised_at         TEXT NOT NULL,
    answered_at       TEXT,
    UNIQUE (run_id, seq)
);

CREATE INDEX idx_workflow_run_holds_open
    ON workflow_run_holds(run_id, seq)
    WHERE state = 'open';

CREATE TABLE workflow_run_hold_rejections (
    id                TEXT NOT NULL PRIMARY KEY,
    hold_id           TEXT NOT NULL REFERENCES workflow_run_holds(id) ON DELETE CASCADE,
    attempted_verdict TEXT NOT NULL,
    feedback          TEXT NOT NULL,
    source_surface    TEXT NOT NULL,
    source_agent_id   TEXT,
    source_session_id TEXT,
    source_turn_id    TEXT,
    rejected_at       TEXT NOT NULL
);

CREATE INDEX idx_workflow_run_hold_rejections_hold
    ON workflow_run_hold_rejections(hold_id, rejected_at);

CREATE TABLE "workflow_journal" (
    id                TEXT NOT NULL PRIMARY KEY,
    run_id            TEXT NOT NULL REFERENCES workflow_runs(id) ON DELETE CASCADE,
    seq               INTEGER NOT NULL,
    completed_order   INTEGER,
    kind              TEXT NOT NULL CHECK (kind IN ('agent_call', 'worker_placement', 'effect', 'log', 'phase', 'workflow_call', 'timer', 'event', 'await_event', 'gate', 'hold', 'budget', 'terminal_budget', 'captured_sources', 'wind_down', 'worker')),
    status            TEXT NOT NULL CHECK (status IN ('started', 'completed', 'failed')),
    args_hash         TEXT NOT NULL,
    request_json      TEXT NOT NULL,
    result_json       TEXT,
    terminal_metadata_json TEXT,
    error_message     TEXT,
    effect_kind       TEXT,
    idempotency_key   TEXT,
    target_kind       TEXT,
    started_at        TEXT NOT NULL,
    completed_at      TEXT, dispatch_json TEXT, dispatch_attempts_json TEXT, termination_json TEXT,
    UNIQUE (run_id, seq),
    UNIQUE (run_id, completed_order)
);

CREATE INDEX idx_workflow_journal_run_seq
    ON workflow_journal(run_id, seq);

CREATE TABLE provider_rounds (
    id TEXT NOT NULL PRIMARY KEY,
    turn_id TEXT NOT NULL REFERENCES turns(id) ON DELETE CASCADE,
    round INTEGER NOT NULL,
    started_at TEXT NOT NULL,
    completed_at TEXT,
    outcome_json TEXT NOT NULL,
    UNIQUE (turn_id, round)
);

CREATE INDEX idx_provider_rounds_turn ON provider_rounds(turn_id, round);

CREATE INDEX idx_sessions_origin ON sessions(origin_kind, origin_source_id);

CREATE INDEX idx_sessions_archived ON sessions(archived_at);

CREATE INDEX idx_workflow_runs_dispatching_session
    ON workflow_runs(dispatching_session_id);

CREATE TABLE surfaces (
    id               TEXT NOT NULL PRIMARY KEY,
    session_id       TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    title            TEXT NOT NULL,
    library_entry_id TEXT REFERENCES surface_library_entries(id) ON DELETE SET NULL,
    seeded_from_name TEXT,
    shape_only       INTEGER NOT NULL DEFAULT 0 CHECK (shape_only IN (0, 1)),
    seeded_at        TEXT,
    created_at       TEXT NOT NULL,
    updated_at       TEXT NOT NULL
);

CREATE INDEX idx_surfaces_session ON surfaces(session_id, created_at);

CREATE TABLE surface_modules (
    id            TEXT NOT NULL PRIMARY KEY,
    surface_id    TEXT NOT NULL REFERENCES surfaces(id) ON DELETE CASCADE,
    kind          TEXT NOT NULL,
    position      INTEGER NOT NULL,
    source        TEXT NOT NULL,
    revision      INTEGER NOT NULL DEFAULT 0,
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL,
    -- The run or turn that delivered the module, as JSON of the wire
    -- shape; NULL for a module written by hand.
    delivery_json TEXT
);

CREATE INDEX idx_surface_modules_surface ON surface_modules(surface_id);

CREATE TABLE surface_annotations (
    id          TEXT NOT NULL PRIMARY KEY,
    surface_id  TEXT NOT NULL REFERENCES surfaces(id) ON DELETE CASCADE,
    module_id   TEXT NOT NULL REFERENCES surface_modules(id) ON DELETE CASCADE,
    body        TEXT NOT NULL,
    tags        TEXT NOT NULL,
    author      TEXT NOT NULL,
    resolved_at TEXT,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);

CREATE INDEX idx_surface_annotations_surface ON surface_annotations(surface_id);

CREATE INDEX idx_surface_annotations_module ON surface_annotations(module_id);

CREATE TABLE surface_library_entries (
    id                    TEXT NOT NULL PRIMARY KEY,
    name                  TEXT NOT NULL,
    description           TEXT NOT NULL,
    tags                  TEXT NOT NULL,
    origin_topic_id       TEXT NOT NULL,
    saved_from_surface_id TEXT,
    created_at            TEXT NOT NULL,
    updated_at            TEXT NOT NULL
);

CREATE TABLE surface_library_entry_modules (
    id          TEXT NOT NULL PRIMARY KEY,
    entry_id    TEXT NOT NULL REFERENCES surface_library_entries(id) ON DELETE CASCADE,
    kind        TEXT NOT NULL,
    position    INTEGER NOT NULL,
    source      TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);

CREATE INDEX idx_surface_library_entry_modules_entry
    ON surface_library_entry_modules(entry_id, position);

CREATE TABLE topic_default_surfaces (
    topic_id      TEXT NOT NULL REFERENCES topics(id) ON DELETE CASCADE,
    entry_id      TEXT NOT NULL REFERENCES surface_library_entries(id) ON DELETE CASCADE,
    designated_at TEXT NOT NULL,
    PRIMARY KEY (topic_id, entry_id)
);
