PRAGMA defer_foreign_keys = ON;

DROP TRIGGER repository_snapshot_evidence_association;
DROP TRIGGER checkpoints_snapshot_task_binding;

ALTER TABLE checkpoints
ADD COLUMN project_event_sequence INTEGER NOT NULL DEFAULT 0
CHECK (project_event_sequence >= 0);

CREATE TEMP TABLE recovery_0004_snapshot_evidence AS
SELECT snapshot_id, evidence_id, role, ordinal
FROM repository_snapshot_evidence;

CREATE TEMP TABLE recovery_0004_checkpoints AS
SELECT checkpoint_id, task_id, session_id, snapshot_id, project_event_sequence,
       objective, created_at, digest
FROM checkpoints;

CREATE TEMP TABLE recovery_0004_checkpoint_items AS
SELECT checkpoint_id, section, ordinal, classification, text
FROM checkpoint_items;

CREATE TEMP TABLE recovery_0004_checkpoint_item_evidence AS
SELECT checkpoint_id, section, item_ordinal, evidence_id, evidence_ordinal
FROM checkpoint_item_evidence;

DROP TRIGGER repository_snapshot_evidence_reject_delete;
DROP TRIGGER checkpoints_reject_delete;
DROP TRIGGER checkpoint_items_reject_delete;
DROP TRIGGER checkpoint_item_evidence_reject_delete;

DELETE FROM checkpoint_item_evidence;
DELETE FROM checkpoint_items;
DELETE FROM checkpoints;
DELETE FROM repository_snapshot_evidence;

CREATE TABLE repository_snapshots_v2 (
    snapshot_id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    task_id TEXT NOT NULL REFERENCES tasks(task_id),
    workspace_id TEXT NOT NULL REFERENCES workspaces(workspace_id),
    head_oid TEXT NOT NULL CHECK (length(head_oid) IN (40, 64)),
    branch TEXT,
    worktree_status_porcelain_v2 TEXT NOT NULL,
    capture_format_version INTEGER NOT NULL CHECK (capture_format_version IN (1, 2)),
    index_digest TEXT NOT NULL CHECK (length(index_digest) = 64),
    tracked_worktree_digest TEXT NOT NULL CHECK (length(tracked_worktree_digest) = 64),
    capture_manifest_digest TEXT NOT NULL CHECK (length(capture_manifest_digest) = 64),
    is_dirty INTEGER NOT NULL CHECK (
        is_dirty IN (0, 1)
        AND is_dirty = (length(worktree_status_porcelain_v2) > 0)
    ),
    captured_at TEXT NOT NULL,
    canonical_digest TEXT NOT NULL CHECK (length(canonical_digest) = 64)
) STRICT;

INSERT INTO repository_snapshots_v2 (
    snapshot_id,
    project_id,
    task_id,
    workspace_id,
    head_oid,
    branch,
    worktree_status_porcelain_v2,
    capture_format_version,
    index_digest,
    tracked_worktree_digest,
    capture_manifest_digest,
    is_dirty,
    captured_at,
    canonical_digest
)
SELECT
    snapshot_id,
    project_id,
    task_id,
    workspace_id,
    head_oid,
    branch,
    worktree_status_porcelain_v2,
    1,
    printf('%064d', 0),
    printf('%064d', 0),
    printf('%064d', 0),
    is_dirty,
    captured_at,
    canonical_digest
FROM repository_snapshots;

DROP TABLE repository_snapshots;
ALTER TABLE repository_snapshots_v2 RENAME TO repository_snapshots;

CREATE INDEX repository_snapshots_task_order_idx
ON repository_snapshots (task_id, captured_at, snapshot_id);

CREATE TRIGGER repository_snapshots_project_binding
BEFORE INSERT ON repository_snapshots
WHEN NEW.project_id != (SELECT project_id FROM state_metadata WHERE metadata_key = 1)
BEGIN
    SELECT RAISE(ABORT, 'RepositorySnapshot project_id does not match state_metadata');
END;

CREATE TRIGGER repository_snapshots_reject_replace
BEFORE INSERT ON repository_snapshots
WHEN EXISTS (SELECT 1 FROM repository_snapshots WHERE snapshot_id = NEW.snapshot_id)
BEGIN
    SELECT RAISE(ABORT, 'RepositorySnapshot is append-only');
END;

CREATE TRIGGER repository_snapshots_reject_update
BEFORE UPDATE ON repository_snapshots
BEGIN
    SELECT RAISE(ABORT, 'RepositorySnapshot is append-only');
END;

CREATE TRIGGER repository_snapshots_reject_delete
BEFORE DELETE ON repository_snapshots
BEGIN
    SELECT RAISE(ABORT, 'RepositorySnapshot is append-only');
END;

CREATE TRIGGER repository_snapshot_evidence_association
BEFORE INSERT ON repository_snapshot_evidence
WHEN (SELECT task_id FROM evidence WHERE evidence_id = NEW.evidence_id) IS NOT
     (SELECT task_id FROM repository_snapshots WHERE snapshot_id = NEW.snapshot_id)
OR (SELECT workspace_id FROM evidence WHERE evidence_id = NEW.evidence_id) IS NOT
   (SELECT workspace_id FROM repository_snapshots WHERE snapshot_id = NEW.snapshot_id)
BEGIN
    SELECT RAISE(ABORT, 'RepositorySnapshot Evidence association is invalid');
END;

CREATE TRIGGER checkpoints_snapshot_task_binding
BEFORE INSERT ON checkpoints
WHEN NEW.task_id IS NOT (
    SELECT task_id FROM repository_snapshots WHERE snapshot_id = NEW.snapshot_id
)
BEGIN
    SELECT RAISE(ABORT, 'Checkpoint task does not match RepositorySnapshot');
END;

INSERT INTO repository_snapshot_evidence (snapshot_id, evidence_id, role, ordinal)
SELECT snapshot_id, evidence_id, role, ordinal
FROM recovery_0004_snapshot_evidence;

INSERT INTO checkpoints (
    checkpoint_id, task_id, session_id, snapshot_id, project_event_sequence,
    objective, created_at, digest
)
SELECT checkpoint_id, task_id, session_id, snapshot_id, project_event_sequence,
       objective, created_at, digest
FROM recovery_0004_checkpoints;

INSERT INTO checkpoint_items (checkpoint_id, section, ordinal, classification, text)
SELECT checkpoint_id, section, ordinal, classification, text
FROM recovery_0004_checkpoint_items;

INSERT INTO checkpoint_item_evidence (
    checkpoint_id, section, item_ordinal, evidence_id, evidence_ordinal
)
SELECT checkpoint_id, section, item_ordinal, evidence_id, evidence_ordinal
FROM recovery_0004_checkpoint_item_evidence;

DROP TABLE recovery_0004_snapshot_evidence;
DROP TABLE recovery_0004_checkpoints;
DROP TABLE recovery_0004_checkpoint_items;
DROP TABLE recovery_0004_checkpoint_item_evidence;

CREATE TRIGGER repository_snapshot_evidence_reject_delete
BEFORE DELETE ON repository_snapshot_evidence
BEGIN
    SELECT RAISE(ABORT, 'RepositorySnapshot Evidence refs are append-only');
END;

CREATE TRIGGER checkpoints_reject_delete
BEFORE DELETE ON checkpoints
BEGIN
    SELECT RAISE(ABORT, 'Checkpoint is append-only');
END;

CREATE TRIGGER checkpoint_items_reject_delete
BEFORE DELETE ON checkpoint_items
BEGIN
    SELECT RAISE(ABORT, 'Checkpoint items are append-only');
END;

CREATE TRIGGER checkpoint_item_evidence_reject_delete
BEFORE DELETE ON checkpoint_item_evidence
BEGIN
    SELECT RAISE(ABORT, 'Checkpoint item Evidence refs are append-only');
END;
