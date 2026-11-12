CREATE TABLE client_tokens (
    token_id TEXT PRIMARY KEY,
    client_label TEXT NOT NULL CHECK (length(client_label) BETWEEN 1 AND 128),
    verifier_sha256 TEXT NOT NULL UNIQUE
        CHECK (length(verifier_sha256) = 64 AND verifier_sha256 NOT GLOB '*[^0-9a-f]*'),
    status TEXT NOT NULL CHECK (status IN ('pending', 'active', 'revoked', 'discarded')),
    origin TEXT NOT NULL CHECK (origin IN ('bootstrap', 'api')),
    created_by_token_id TEXT REFERENCES client_tokens(token_id),
    created_at TEXT NOT NULL,
    revoked_at TEXT,
    CHECK ((status = 'revoked') = (revoked_at IS NOT NULL)),
    CHECK ((origin = 'api') = (created_by_token_id IS NOT NULL))
) STRICT;

CREATE TABLE client_token_scopes (
    token_id TEXT NOT NULL REFERENCES client_tokens(token_id),
    scope TEXT NOT NULL
        CHECK (scope IN ('control', 'ingest-only', 'mcp-read', 'mcp-write', 'proxy-client')),
    PRIMARY KEY (token_id, scope)
) STRICT;

CREATE TABLE token_idempotency_records (
    principal_token_id TEXT NOT NULL REFERENCES client_tokens(token_id),
    operation_id TEXT NOT NULL
        CHECK (operation_id IN ('createClientToken', 'revokeClientToken')),
    idempotency_key TEXT NOT NULL,
    canonical_request_sha256 TEXT NOT NULL,
    result_token_id TEXT NOT NULL REFERENCES client_tokens(token_id),
    created_at TEXT NOT NULL,
    PRIMARY KEY (principal_token_id, operation_id, idempotency_key)
) STRICT;

CREATE TABLE token_audit_events (
    audit_seq INTEGER PRIMARY KEY,
    occurred_at TEXT NOT NULL,
    action TEXT NOT NULL CHECK (
        action IN (
            'bootstrap_issued', 'bootstrap_activated', 'bootstrap_discarded',
            'created', 'revoked'
        )
    ),
    actor_token_id TEXT REFERENCES client_tokens(token_id),
    target_token_id TEXT NOT NULL REFERENCES client_tokens(token_id)
) STRICT;

CREATE TRIGGER token_audit_events_reject_update
BEFORE UPDATE ON token_audit_events
BEGIN
    SELECT RAISE(ABORT, 'token audit events are append-only');
END;

CREATE TRIGGER token_audit_events_reject_delete
BEFORE DELETE ON token_audit_events
BEGIN
    SELECT RAISE(ABORT, 'token audit events are append-only');
END;

CREATE INDEX client_tokens_status_idx ON client_tokens (status, token_id);
CREATE INDEX client_token_scopes_scope_idx ON client_token_scopes (scope, token_id);
