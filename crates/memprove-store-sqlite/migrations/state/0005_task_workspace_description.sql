ALTER TABLE tasks
ADD COLUMN workspace_id TEXT REFERENCES workspaces(workspace_id);

ALTER TABLE tasks
ADD COLUMN description TEXT;

CREATE INDEX tasks_workspace_idx ON tasks (workspace_id, created_at, task_id);
