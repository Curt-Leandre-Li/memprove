CREATE TABLE schema_migrations (
    migration_id INTEGER PRIMARY KEY,
    migration_checksum TEXT NOT NULL,
    applied_at TEXT NOT NULL
) STRICT;

CREATE TABLE projects (
    project_id TEXT PRIMARY KEY,
    repository_common_dir TEXT NOT NULL UNIQUE,
    object_format TEXT NOT NULL CHECK (object_format IN ('sha1', 'sha256')),
    state_db_path TEXT NOT NULL UNIQUE,
    provisioning_state TEXT NOT NULL
        CHECK (provisioning_state IN ('PROVISIONING', 'ACTIVE', 'FAILED')),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision >= 1)
) STRICT;

CREATE TABLE registry_idempotency_records (
    principal_id TEXT NOT NULL,
    operation_id TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    canonical_request_sha256 TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('IN_PROGRESS', 'SUCCEEDED', 'FAILED')),
    provisioning_record_id TEXT NOT NULL REFERENCES projects(project_id),
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

CREATE INDEX registry_projects_provisioning_state_idx
    ON projects (provisioning_state, project_id);
