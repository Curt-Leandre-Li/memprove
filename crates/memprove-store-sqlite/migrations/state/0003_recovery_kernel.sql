CREATE TABLE evidence (
    evidence_id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL REFERENCES tasks(task_id),
    workspace_id TEXT NOT NULL REFERENCES workspaces(workspace_id),
    kind TEXT NOT NULL CHECK (
        kind IN ('git_head', 'git_worktree', 'file_digest', 'command_result', 'test_result')
    ),
    subject TEXT NOT NULL,
    observed_at TEXT NOT NULL,
    collector_name TEXT NOT NULL CHECK (length(collector_name) > 0),
    collector_version TEXT NOT NULL CHECK (length(collector_version) > 0),
    payload TEXT NOT NULL,
    payload_digest TEXT NOT NULL CHECK (length(payload_digest) = 64),
    canonical_digest TEXT NOT NULL UNIQUE CHECK (length(canonical_digest) = 64)
) STRICT;

CREATE INDEX evidence_task_workspace_kind_idx
ON evidence (task_id, workspace_id, kind, evidence_id);

CREATE TRIGGER evidence_task_workspace_association
BEFORE INSERT ON evidence
WHEN (SELECT project_id FROM tasks WHERE task_id = NEW.task_id) IS NOT
     (SELECT project_id FROM workspaces WHERE workspace_id = NEW.workspace_id)
BEGIN
    SELECT RAISE(ABORT, 'Evidence task/workspace association is invalid');
END;

CREATE TRIGGER evidence_reject_replace
BEFORE INSERT ON evidence
WHEN EXISTS (SELECT 1 FROM evidence WHERE evidence_id = NEW.evidence_id)
OR EXISTS (SELECT 1 FROM evidence WHERE canonical_digest = NEW.canonical_digest)
BEGIN
    SELECT RAISE(ABORT, 'Evidence is append-only');
END;

CREATE TRIGGER evidence_reject_update
BEFORE UPDATE ON evidence
BEGIN
    SELECT RAISE(ABORT, 'Evidence is append-only');
END;

CREATE TRIGGER evidence_reject_delete
BEFORE DELETE ON evidence
BEGIN
    SELECT RAISE(ABORT, 'Evidence is append-only');
END;

CREATE TABLE repository_snapshots (
    snapshot_id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    task_id TEXT NOT NULL REFERENCES tasks(task_id),
    workspace_id TEXT NOT NULL REFERENCES workspaces(workspace_id),
    head_oid TEXT NOT NULL CHECK (length(head_oid) IN (40, 64)),
    branch TEXT,
    worktree_status_porcelain_v2 TEXT NOT NULL,
    is_dirty INTEGER NOT NULL CHECK (
        is_dirty IN (0, 1)
        AND is_dirty = (length(worktree_status_porcelain_v2) > 0)
    ),
    captured_at TEXT NOT NULL,
    canonical_digest TEXT NOT NULL UNIQUE CHECK (length(canonical_digest) = 64)
) STRICT;

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
OR EXISTS (SELECT 1 FROM repository_snapshots WHERE canonical_digest = NEW.canonical_digest)
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

CREATE TABLE repository_snapshot_evidence (
    snapshot_id TEXT NOT NULL REFERENCES repository_snapshots(snapshot_id),
    evidence_id TEXT NOT NULL REFERENCES evidence(evidence_id),
    role TEXT NOT NULL CHECK (role IN ('git_head', 'git_worktree', 'file', 'command', 'test')),
    ordinal INTEGER NOT NULL CHECK (
        ordinal >= 0
        AND (role NOT IN ('git_head', 'git_worktree') OR ordinal = 0)
    ),
    PRIMARY KEY (snapshot_id, evidence_id),
    UNIQUE (snapshot_id, role, ordinal)
) STRICT;

CREATE INDEX repository_snapshot_evidence_role_idx
ON repository_snapshot_evidence (snapshot_id, role, ordinal, evidence_id);

CREATE TRIGGER repository_snapshot_evidence_role_kind
BEFORE INSERT ON repository_snapshot_evidence
WHEN NEW.role != CASE (SELECT kind FROM evidence WHERE evidence_id = NEW.evidence_id)
    WHEN 'git_head' THEN 'git_head'
    WHEN 'git_worktree' THEN 'git_worktree'
    WHEN 'file_digest' THEN 'file'
    WHEN 'command_result' THEN 'command'
    WHEN 'test_result' THEN 'test'
END
BEGIN
    SELECT RAISE(ABORT, 'RepositorySnapshot Evidence role does not match kind');
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

CREATE TRIGGER repository_snapshot_evidence_reject_replace
BEFORE INSERT ON repository_snapshot_evidence
WHEN EXISTS (
    SELECT 1 FROM repository_snapshot_evidence
    WHERE snapshot_id = NEW.snapshot_id AND evidence_id = NEW.evidence_id
)
OR EXISTS (
    SELECT 1 FROM repository_snapshot_evidence
    WHERE snapshot_id = NEW.snapshot_id AND role = NEW.role AND ordinal = NEW.ordinal
)
BEGIN
    SELECT RAISE(ABORT, 'RepositorySnapshot Evidence refs are append-only');
END;

CREATE TRIGGER repository_snapshot_evidence_reject_update
BEFORE UPDATE ON repository_snapshot_evidence
BEGIN
    SELECT RAISE(ABORT, 'RepositorySnapshot Evidence refs are append-only');
END;

CREATE TRIGGER repository_snapshot_evidence_reject_delete
BEFORE DELETE ON repository_snapshot_evidence
BEGIN
    SELECT RAISE(ABORT, 'RepositorySnapshot Evidence refs are append-only');
END;

CREATE TABLE checkpoints (
    checkpoint_id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL REFERENCES tasks(task_id),
    session_id TEXT NOT NULL REFERENCES sessions(session_id),
    snapshot_id TEXT NOT NULL REFERENCES repository_snapshots(snapshot_id),
    objective TEXT NOT NULL CHECK (length(objective) > 0),
    created_at TEXT NOT NULL,
    digest TEXT NOT NULL UNIQUE CHECK (length(digest) = 64)
) STRICT;

CREATE INDEX checkpoints_task_order_idx
ON checkpoints (task_id, created_at, checkpoint_id);

CREATE TRIGGER checkpoints_snapshot_task_binding
BEFORE INSERT ON checkpoints
WHEN NEW.task_id IS NOT (
    SELECT task_id FROM repository_snapshots WHERE snapshot_id = NEW.snapshot_id
)
BEGIN
    SELECT RAISE(ABORT, 'Checkpoint task does not match RepositorySnapshot');
END;

CREATE TRIGGER checkpoints_session_task_binding
BEFORE INSERT ON checkpoints
WHEN NOT EXISTS (
    SELECT 1 FROM task_sessions
    WHERE task_id = NEW.task_id AND session_id = NEW.session_id
)
BEGIN
    SELECT RAISE(ABORT, 'Checkpoint Session is not bound to Task');
END;

CREATE TRIGGER checkpoints_reject_replace
BEFORE INSERT ON checkpoints
WHEN EXISTS (SELECT 1 FROM checkpoints WHERE checkpoint_id = NEW.checkpoint_id)
OR EXISTS (SELECT 1 FROM checkpoints WHERE digest = NEW.digest)
BEGIN
    SELECT RAISE(ABORT, 'Checkpoint is append-only');
END;

CREATE TRIGGER checkpoints_reject_update
BEFORE UPDATE ON checkpoints
BEGIN
    SELECT RAISE(ABORT, 'Checkpoint is append-only');
END;

CREATE TRIGGER checkpoints_reject_delete
BEFORE DELETE ON checkpoints
BEGIN
    SELECT RAISE(ABORT, 'Checkpoint is append-only');
END;

CREATE TABLE checkpoint_items (
    checkpoint_id TEXT NOT NULL REFERENCES checkpoints(checkpoint_id),
    section TEXT NOT NULL CHECK (
        section IN ('verified_completed_work', 'open_items', 'next_action')
    ),
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    classification TEXT NOT NULL CHECK (
        classification IN ('verified_fact', 'user_supplied_context', 'recommendation', 'unknown')
    ),
    text TEXT NOT NULL CHECK (length(text) > 0),
    PRIMARY KEY (checkpoint_id, section, ordinal)
) STRICT;

CREATE INDEX checkpoint_items_order_idx
ON checkpoint_items (checkpoint_id, section, ordinal);

CREATE TRIGGER checkpoint_items_section_contract
BEFORE INSERT ON checkpoint_items
WHEN (NEW.section = 'verified_completed_work' AND NEW.classification != 'verified_fact')
OR (NEW.section = 'next_action' AND (
    NEW.ordinal != 0 OR NEW.classification NOT IN ('recommendation', 'unknown')
))
BEGIN
    SELECT RAISE(ABORT, 'Checkpoint item classification is invalid for its section');
END;

CREATE TRIGGER checkpoint_items_reject_replace
BEFORE INSERT ON checkpoint_items
WHEN EXISTS (
    SELECT 1 FROM checkpoint_items
    WHERE checkpoint_id = NEW.checkpoint_id
      AND section = NEW.section
      AND ordinal = NEW.ordinal
)
BEGIN
    SELECT RAISE(ABORT, 'Checkpoint items are append-only');
END;

CREATE TRIGGER checkpoint_items_reject_update
BEFORE UPDATE ON checkpoint_items
BEGIN
    SELECT RAISE(ABORT, 'Checkpoint items are append-only');
END;

CREATE TRIGGER checkpoint_items_reject_delete
BEFORE DELETE ON checkpoint_items
BEGIN
    SELECT RAISE(ABORT, 'Checkpoint items are append-only');
END;

CREATE TABLE checkpoint_item_evidence (
    checkpoint_id TEXT NOT NULL,
    section TEXT NOT NULL,
    item_ordinal INTEGER NOT NULL,
    evidence_id TEXT NOT NULL REFERENCES evidence(evidence_id),
    evidence_ordinal INTEGER NOT NULL CHECK (evidence_ordinal >= 0),
    PRIMARY KEY (checkpoint_id, section, item_ordinal, evidence_id),
    UNIQUE (checkpoint_id, section, item_ordinal, evidence_ordinal),
    FOREIGN KEY (checkpoint_id, section, item_ordinal)
        REFERENCES checkpoint_items(checkpoint_id, section, ordinal)
) STRICT;

CREATE INDEX checkpoint_item_evidence_order_idx
ON checkpoint_item_evidence (
    checkpoint_id, section, item_ordinal, evidence_ordinal, evidence_id
);

CREATE TRIGGER checkpoint_item_evidence_snapshot_membership
BEFORE INSERT ON checkpoint_item_evidence
WHEN NOT EXISTS (
    SELECT 1
    FROM checkpoints AS checkpoint
    JOIN repository_snapshot_evidence AS snapshot_evidence
      ON snapshot_evidence.snapshot_id = checkpoint.snapshot_id
    WHERE checkpoint.checkpoint_id = NEW.checkpoint_id
      AND snapshot_evidence.evidence_id = NEW.evidence_id
)
BEGIN
    SELECT RAISE(ABORT, 'Checkpoint Evidence is outside RepositorySnapshot');
END;

CREATE TRIGGER checkpoint_item_evidence_reject_replace
BEFORE INSERT ON checkpoint_item_evidence
WHEN EXISTS (
    SELECT 1 FROM checkpoint_item_evidence
    WHERE checkpoint_id = NEW.checkpoint_id
      AND section = NEW.section
      AND item_ordinal = NEW.item_ordinal
      AND evidence_id = NEW.evidence_id
)
OR EXISTS (
    SELECT 1 FROM checkpoint_item_evidence
    WHERE checkpoint_id = NEW.checkpoint_id
      AND section = NEW.section
      AND item_ordinal = NEW.item_ordinal
      AND evidence_ordinal = NEW.evidence_ordinal
)
BEGIN
    SELECT RAISE(ABORT, 'Checkpoint item Evidence refs are append-only');
END;

CREATE TRIGGER checkpoint_item_evidence_reject_update
BEFORE UPDATE ON checkpoint_item_evidence
BEGIN
    SELECT RAISE(ABORT, 'Checkpoint item Evidence refs are append-only');
END;

CREATE TRIGGER checkpoint_item_evidence_reject_delete
BEFORE DELETE ON checkpoint_item_evidence
BEGIN
    SELECT RAISE(ABORT, 'Checkpoint item Evidence refs are append-only');
END;
