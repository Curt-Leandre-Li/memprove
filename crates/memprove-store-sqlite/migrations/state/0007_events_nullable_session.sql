-- ADR 0012 D7 (Owner decision, P2-C): rebuild `events` so that `session_id`
-- and `sequence` may be NULL, for the Events continuityd produces for a
-- change no Session made (task.created, task.transitioned,
-- workspace.registered). Nothing else about the table changes: every other
-- column keeps its name, position, type, and constraint, and every existing
-- row is copied unchanged, so every stored digest still recomputes. No other
-- table references `events`, so no other table is touched.
--
-- The triggers are dropped with the old table and recreated after the copy,
-- so pre-envelope rows (ADR 0010 D7) are preserved byte for byte as before.
-- They keep their names and rules; only the NULL cases are new: a row has a
-- Session sequence exactly when it has a Session, a row without a Session is
-- daemon-produced (producer continuityd, privacy internal, producer sequence
-- equal to its Project sequence), and the Session-scoped rules apply only to
-- rows with a Session. This migration runs in the runner's IMMEDIATE
-- transaction: it applies completely or not at all.

CREATE TABLE events_0007 (
    event_id TEXT PRIMARY KEY,
    session_id TEXT REFERENCES sessions(session_id),
    sequence INTEGER CHECK (sequence >= 1),
    event_type TEXT NOT NULL CHECK (length(event_type) > 0),
    payload TEXT NOT NULL,
    payload_sha256 TEXT NOT NULL CHECK (length(payload_sha256) = 64),
    occurred_at TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    previous_event_sha256 TEXT,
    event_sha256 TEXT NOT NULL UNIQUE CHECK (length(event_sha256) = 64),
    envelope_version TEXT
        CHECK (envelope_version IS NULL OR envelope_version = '1.0'),
    project_id TEXT,
    workspace_id TEXT REFERENCES workspaces(workspace_id),
    payload_schema TEXT
        CHECK (payload_schema IS NULL OR length(payload_schema) > 0),
    producer_name TEXT
        CHECK (producer_name IS NULL OR length(producer_name) > 0),
    producer_version TEXT
        CHECK (producer_version IS NULL OR length(producer_version) > 0),
    producer_sequence INTEGER
        CHECK (producer_sequence IS NULL OR producer_sequence BETWEEN 0 AND 9007199254740991),
    correlation_id TEXT,
    causation_id TEXT,
    privacy_class TEXT
        CHECK (privacy_class IS NULL OR privacy_class IN ('public', 'internal', 'private', 'secret')),
    producer_occurred_at TEXT,
    project_sequence INTEGER
        CHECK (project_sequence IS NULL OR project_sequence BETWEEN 1 AND 9007199254740991),
    event_digest_version INTEGER
        CHECK (event_digest_version IS NULL OR event_digest_version = 2),
    UNIQUE (session_id, sequence)
) STRICT;

INSERT INTO events_0007 (
    event_id, session_id, sequence, event_type, payload, payload_sha256,
    occurred_at, recorded_at, previous_event_sha256, event_sha256,
    envelope_version, project_id, workspace_id, payload_schema,
    producer_name, producer_version, producer_sequence, correlation_id,
    causation_id, privacy_class, producer_occurred_at, project_sequence,
    event_digest_version
)
SELECT
    event_id, session_id, sequence, event_type, payload, payload_sha256,
    occurred_at, recorded_at, previous_event_sha256, event_sha256,
    envelope_version, project_id, workspace_id, payload_schema,
    producer_name, producer_version, producer_sequence, correlation_id,
    causation_id, privacy_class, producer_occurred_at, project_sequence,
    event_digest_version
FROM events
ORDER BY rowid;

DROP TABLE events;
ALTER TABLE events_0007 RENAME TO events;

CREATE INDEX events_order_idx ON events (session_id, sequence, event_id);
CREATE UNIQUE INDEX events_project_sequence_idx ON events (project_sequence);

-- 0002, unchanged: no replacement of an Event, its Session position, or its
-- digest. With a NULL Session the position clause matches nothing.
CREATE TRIGGER events_reject_replace
BEFORE INSERT ON events
WHEN EXISTS (SELECT 1 FROM events WHERE event_id = NEW.event_id)
OR EXISTS (
    SELECT 1 FROM events
    WHERE session_id = NEW.session_id AND sequence = NEW.sequence
)
OR EXISTS (SELECT 1 FROM events WHERE event_sha256 = NEW.event_sha256)
BEGIN
    SELECT RAISE(ABORT, 'events are append-only');
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

-- 0006, with the NULL cases: a Session sequence exactly with a Session; the
-- Workspace is the Session's when there is one; without a Session the Event
-- is continuityd's own.
CREATE TRIGGER events_require_envelope
BEFORE INSERT ON events
WHEN NEW.event_digest_version IS NOT 2
OR NEW.envelope_version IS NULL
OR NEW.project_id IS NULL
OR NEW.workspace_id IS NULL
OR NEW.payload_schema IS NULL
OR NEW.producer_name IS NULL
OR NEW.producer_version IS NULL
OR NEW.producer_sequence IS NULL
OR NEW.privacy_class IS NULL
OR NEW.project_sequence IS NULL
OR NEW.project_id IS NOT (SELECT project_id FROM state_metadata WHERE metadata_key = 1)
OR (NEW.session_id IS NULL) != (NEW.sequence IS NULL)
OR (
    NEW.session_id IS NOT NULL
    AND NEW.workspace_id IS NOT (SELECT workspace_id FROM sessions WHERE session_id = NEW.session_id)
)
OR (
    NEW.session_id IS NULL
    AND (
        NEW.producer_name IS NOT 'continuityd'
        OR NEW.privacy_class IS NOT 'internal'
        OR NEW.producer_sequence IS NOT NEW.project_sequence
    )
)
BEGIN
    SELECT RAISE(ABORT, 'event envelope is incomplete or unbound');
END;

CREATE TRIGGER events_reject_pre_envelope_ledger
BEFORE INSERT ON events
WHEN EXISTS (SELECT 1 FROM events WHERE event_digest_version IS NULL)
BEGIN
    SELECT RAISE(ABORT, 'pre-envelope Event rows are present');
END;

-- 0006, with the NULL case: the per-Session sequence is contiguous for rows
-- with a Session; the Project chain covers every row.
CREATE TRIGGER events_enforce_chain
BEFORE INSERT ON events
WHEN (
    NEW.session_id IS NOT NULL
    AND NEW.sequence IS NOT COALESCE(
        (SELECT MAX(sequence) + 1 FROM events WHERE session_id = NEW.session_id),
        1
    )
)
OR NEW.project_sequence IS NOT COALESCE((SELECT MAX(project_sequence) + 1 FROM events), 1)
OR (
    NEW.project_sequence = 1 AND NEW.previous_event_sha256 IS NOT NULL
)
OR (
    NEW.project_sequence > 1 AND NEW.previous_event_sha256 IS NOT (
        SELECT event_sha256
        FROM events
        WHERE project_sequence = NEW.project_sequence - 1
    )
)
BEGIN
    SELECT RAISE(ABORT, 'event sequence or hash chain is invalid');
END;
