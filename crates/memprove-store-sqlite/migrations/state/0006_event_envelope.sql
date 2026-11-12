-- ADR 0010: Event envelope persistence and the version 2 Event digest.
-- Columns are added nullable (append-only migration); rows written by this
-- schema must carry every required envelope column, enforced below and by
-- the store. Rows with a NULL event_digest_version predate the envelope.
ALTER TABLE events ADD COLUMN envelope_version TEXT
    CHECK (envelope_version IS NULL OR envelope_version = '1.0');
ALTER TABLE events ADD COLUMN project_id TEXT;
ALTER TABLE events ADD COLUMN workspace_id TEXT REFERENCES workspaces(workspace_id);
ALTER TABLE events ADD COLUMN payload_schema TEXT
    CHECK (payload_schema IS NULL OR length(payload_schema) > 0);
ALTER TABLE events ADD COLUMN producer_name TEXT
    CHECK (producer_name IS NULL OR length(producer_name) > 0);
ALTER TABLE events ADD COLUMN producer_version TEXT
    CHECK (producer_version IS NULL OR length(producer_version) > 0);
ALTER TABLE events ADD COLUMN producer_sequence INTEGER
    CHECK (producer_sequence IS NULL OR producer_sequence BETWEEN 0 AND 9007199254740991);
ALTER TABLE events ADD COLUMN correlation_id TEXT;
ALTER TABLE events ADD COLUMN causation_id TEXT;
ALTER TABLE events ADD COLUMN privacy_class TEXT
    CHECK (privacy_class IS NULL OR privacy_class IN ('public', 'internal', 'private', 'secret'));
-- Producer-declared occurrence time (wire `occurred_at`). The 0001 column
-- `occurred_at` stays NOT NULL; envelope rows store their receipt time there
-- and it is never read back or exposed.
ALTER TABLE events ADD COLUMN producer_occurred_at TEXT;
ALTER TABLE events ADD COLUMN project_sequence INTEGER
    CHECK (project_sequence IS NULL OR project_sequence BETWEEN 1 AND 9007199254740991);
ALTER TABLE events ADD COLUMN event_digest_version INTEGER
    CHECK (event_digest_version IS NULL OR event_digest_version = 2);

CREATE UNIQUE INDEX events_project_sequence_idx ON events (project_sequence);

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
OR NEW.workspace_id IS NOT (SELECT workspace_id FROM sessions WHERE session_id = NEW.session_id)
BEGIN
    SELECT RAISE(ABORT, 'event envelope is incomplete or unbound');
END;

CREATE TRIGGER events_reject_pre_envelope_ledger
BEFORE INSERT ON events
WHEN EXISTS (SELECT 1 FROM events WHERE event_digest_version IS NULL)
BEGIN
    SELECT RAISE(ABORT, 'pre-envelope Event rows are present');
END;

-- The chain is per Project (ADR 0010 D9); the per-Session sequence stays
-- contiguous.
DROP TRIGGER events_enforce_chain;

CREATE TRIGGER events_enforce_chain
BEFORE INSERT ON events
WHEN NEW.sequence != COALESCE(
        (SELECT MAX(sequence) + 1 FROM events WHERE session_id = NEW.session_id),
        1
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
