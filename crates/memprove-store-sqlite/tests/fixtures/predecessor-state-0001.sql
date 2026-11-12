CREATE TABLE schema_migrations (
    migration_id INTEGER PRIMARY KEY,
    migration_checksum TEXT NOT NULL,
    applied_at TEXT NOT NULL
) STRICT;

CREATE TABLE state_metadata (
    metadata_key INTEGER PRIMARY KEY CHECK (metadata_key = 1),
    project_id TEXT NOT NULL,
    repository_common_dir_sha256 TEXT NOT NULL,
    schema_generation INTEGER NOT NULL CHECK (schema_generation = 1),
    created_at TEXT NOT NULL
) STRICT;

CREATE TABLE workspaces (
    workspace_id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    canonical_worktree_path TEXT NOT NULL UNIQUE,
    git_dir TEXT NOT NULL UNIQUE,
    head_commit TEXT NOT NULL,
    branch_or_detached_state TEXT NOT NULL,
    created_at TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision >= 1)
) STRICT;

CREATE TRIGGER workspaces_project_binding
BEFORE INSERT ON workspaces
WHEN NEW.project_id != (SELECT project_id FROM state_metadata WHERE metadata_key = 1)
BEGIN
    SELECT RAISE(ABORT, 'workspace project_id does not match state_metadata');
END;

CREATE TABLE tasks (
    task_id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    title TEXT NOT NULL CHECK (length(title) > 0),
    status TEXT NOT NULL
        CHECK (status IN ('proposed', 'ready', 'in_progress', 'blocked', 'completed', 'cancelled')),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision >= 1)
) STRICT;

CREATE INDEX tasks_order_idx ON tasks (created_at, task_id);

CREATE TRIGGER tasks_project_binding
BEFORE INSERT ON tasks
WHEN NEW.project_id != (SELECT project_id FROM state_metadata WHERE metadata_key = 1)
BEGIN
    SELECT RAISE(ABORT, 'task project_id does not match state_metadata');
END;

CREATE TABLE sessions (
    session_id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES workspaces(workspace_id),
    agent_id TEXT NOT NULL,
    started_at TEXT NOT NULL,
    ended_at TEXT,
    revision INTEGER NOT NULL CHECK (revision >= 1)
) STRICT;

CREATE INDEX sessions_order_idx ON sessions (started_at, session_id);

CREATE TABLE task_sessions (
    task_id TEXT NOT NULL REFERENCES tasks(task_id),
    session_id TEXT NOT NULL REFERENCES sessions(session_id),
    linked_at TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision >= 1),
    PRIMARY KEY (task_id, session_id),
    UNIQUE (session_id)
) STRICT;

CREATE INDEX task_sessions_order_idx ON task_sessions (task_id, linked_at, session_id);

CREATE TABLE events (
    event_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(session_id),
    sequence INTEGER NOT NULL CHECK (sequence >= 1),
    event_type TEXT NOT NULL CHECK (length(event_type) > 0),
    payload TEXT NOT NULL,
    payload_sha256 TEXT NOT NULL CHECK (length(payload_sha256) = 64),
    occurred_at TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    previous_event_sha256 TEXT,
    event_sha256 TEXT NOT NULL UNIQUE CHECK (length(event_sha256) = 64),
    UNIQUE (session_id, sequence)
) STRICT;

CREATE INDEX events_order_idx ON events (session_id, sequence, event_id);

CREATE TRIGGER events_reject_replace
BEFORE INSERT ON events
WHEN EXISTS (SELECT 1 FROM events WHERE event_id = NEW.event_id)
BEGIN
    SELECT RAISE(ABORT, 'events are append-only');
END;

CREATE TRIGGER events_enforce_chain
BEFORE INSERT ON events
WHEN NEW.sequence != COALESCE(
        (SELECT MAX(sequence) + 1 FROM events WHERE session_id = NEW.session_id),
        1
    )
OR (
    NEW.sequence = 1 AND NEW.previous_event_sha256 IS NOT NULL
)
OR (
    NEW.sequence > 1 AND NEW.previous_event_sha256 IS NOT (
        SELECT event_sha256
        FROM events
        WHERE session_id = NEW.session_id AND sequence = NEW.sequence - 1
    )
)
BEGIN
    SELECT RAISE(ABORT, 'event sequence or hash chain is invalid');
END;

CREATE TRIGGER events_reject_update
BEFORE UPDATE ON events
BEGIN
    SELECT RAISE(ABORT, 'events are append-only');
END;

CREATE TRIGGER events_reject_delete
BEFORE DELETE ON events
BEGIN
    SELECT RAISE(ABORT, 'events are append-only');
END;

CREATE TABLE state_idempotency_records (
    principal_id TEXT NOT NULL,
    operation_id TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    canonical_request_sha256 TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('IN_PROGRESS', 'SUCCEEDED', 'FAILED')),
    result_record_type TEXT,
    result_record_id TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (principal_id, operation_id, idempotency_key),
    CHECK (
        (state = 'SUCCEEDED' AND result_record_type IS NOT NULL AND result_record_id IS NOT NULL)
        OR state != 'SUCCEEDED'
    )
) STRICT;
