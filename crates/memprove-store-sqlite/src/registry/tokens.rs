//! Client-token persistence in `registry.sqlite`.
//!
//! Only the SHA-256 verifier of a token secret is stored; the plaintext secret
//! and the full bearer credential never reach this module. Every mutation
//! commits its token state, idempotency record, and audit event in one
//! `BEGIN IMMEDIATE` transaction, which also closes the last-control-token race.

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use super::RegistryStore;
use crate::{Idempotent, StoreError, map_sqlite_error};

const CONTROL_SCOPE: &str = "control";
const CREATE_OPERATION: &str = "createClientToken";
const REVOKE_OPERATION: &str = "revokeClientToken";

/// Persisted token lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenState {
    /// Issued offline but not yet delivered; never authenticates.
    Pending,
    /// Authenticates when its verifier matches.
    Active,
    /// Revoked; never authenticates again.
    Revoked,
    /// A pending token whose delivery failed or was abandoned.
    Discarded,
}

impl TokenState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Active => "active",
            Self::Revoked => "revoked",
            Self::Discarded => "discarded",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "active" => Some(Self::Active),
            "revoked" => Some(Self::Revoked),
            "discarded" => Some(Self::Discarded),
            _ => None,
        }
    }
}

/// Non-sensitive token metadata. The verifier is deliberately absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredToken {
    /// Stable `UUIDv7` token identifier.
    pub token_id: String,
    /// Client label chosen at creation.
    pub client_label: String,
    /// Granted scopes in contract order.
    pub scopes: Vec<String>,
    /// Lifecycle state.
    pub state: TokenState,
    /// Creation timestamp (RFC 3339 UTC).
    pub created_at: String,
    /// Revocation timestamp, present only for revoked tokens.
    pub revoked_at: Option<String>,
}

impl StoredToken {
    /// Whether the token grants `scope`.
    #[must_use]
    pub fn has_scope(&self, scope: &str) -> bool {
        self.scopes.iter().any(|held| held == scope)
    }
}

/// A token to insert. `verifier_sha256` is the lower-case hex SHA-256 of the
/// secret; the secret itself is never passed to the store.
#[derive(Debug, Clone, Copy)]
pub struct NewClientToken<'a> {
    /// Fresh `UUIDv7` token identifier.
    pub token_id: &'a str,
    /// Client label.
    pub client_label: &'a str,
    /// Granted scopes; each must be in the frozen scope vocabulary.
    pub scopes: &'a [&'a str],
    /// Lower-case hex SHA-256 of the secret.
    pub verifier_sha256: &'a str,
    /// Creation timestamp (RFC 3339 UTC).
    pub created_at: &'a str,
}

/// One page of active and revoked tokens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenPage {
    /// Tokens in `token_id` order.
    pub items: Vec<StoredToken>,
    /// Whether more tokens follow the last item.
    pub has_more: bool,
}

/// Idempotency namespace and digest of one authenticated token mutation.
#[derive(Debug, Clone, Copy)]
pub struct TokenCommand<'a> {
    /// Token id of the authenticated caller.
    pub principal_token_id: &'a str,
    /// Exact `Idempotency-Key` header value.
    pub idempotency_key: &'a str,
    /// Lower-case hex SHA-256 of the canonical typed request.
    pub request_sha256: &'a str,
    /// Command timestamp (RFC 3339 UTC).
    pub now: &'a str,
}

impl RegistryStore {
    /// Returns the active token whose verifier matches, if any. Pending,
    /// discarded, revoked, and unknown verifiers all yield `None`.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the registry cannot be read.
    pub fn authenticate_token(
        &self,
        verifier_sha256: &str,
    ) -> Result<Option<StoredToken>, StoreError> {
        let token_id = self
            .connection
            .query_row(
                "SELECT token_id FROM client_tokens
                 WHERE verifier_sha256 = ?1 AND status = 'active'",
                [verifier_sha256],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        token_id
            .map(|token_id| load_token(&self.connection, &token_id))
            .transpose()
            .map(Option::flatten)
    }

    /// U5 P2 (Owner O4 b): whether this registry ever stored a token whose
    /// verifier is `verifier_sha256`, in any state (pending, active,
    /// revoked, or discarded): a token this daemon issued.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the registry cannot be read.
    pub fn token_verifier_issued(&self, verifier_sha256: &str) -> Result<bool, StoreError> {
        Ok(self.connection.query_row(
            "SELECT EXISTS (SELECT 1 FROM client_tokens WHERE verifier_sha256 = ?1)",
            [verifier_sha256],
            |row| row.get::<_, bool>(0),
        )?)
    }

    /// Creates an active token on behalf of an authenticated caller, or
    /// replays the result of an earlier request with the same key.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::IdempotencyConflict`] when the key was used with a
    /// different request digest, or another [`StoreError`] on failure.
    pub fn create_client_token(
        &mut self,
        command: &TokenCommand<'_>,
        token: &NewClientToken<'_>,
    ) -> Result<Idempotent<StoredToken>, StoreError> {
        self.capability_root.verify_identity()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(replayed) = replay(&transaction, CREATE_OPERATION, command)? {
            transaction.commit()?;
            return Ok(Idempotent::Replayed(replayed));
        }
        insert_token(
            &transaction,
            token,
            TokenState::Active,
            Some(command.principal_token_id),
        )?;
        audit(
            &transaction,
            command.now,
            "created",
            Some(command.principal_token_id),
            token.token_id,
        )?;
        record_idempotency(&transaction, CREATE_OPERATION, command, token.token_id)?;
        let created = require_token(&transaction, token.token_id)?;
        self.capability_root.verify_identity()?;
        transaction.commit()?;
        Ok(Idempotent::Created(created))
    }

    /// Lists active and revoked tokens after `after` in `token_id` order.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the registry cannot be read.
    pub fn list_client_tokens(
        &self,
        after: Option<&str>,
        limit: u32,
    ) -> Result<TokenPage, StoreError> {
        let mut ids = self
            .connection
            .prepare(
                "SELECT token_id FROM client_tokens
                 WHERE status IN ('active', 'revoked') AND (?1 IS NULL OR token_id > ?1)
                 ORDER BY token_id LIMIT ?2",
            )?
            .query_map(params![after, i64::from(limit) + 1], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let has_more = ids.len() > limit as usize;
        ids.truncate(limit as usize);
        let items = ids
            .iter()
            .map(|token_id| require_token(&self.connection, token_id))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(TokenPage { items, has_more })
    }

    /// Revokes `target_token_id`, returns an already revoked target unchanged,
    /// or replays the result of an earlier request with the same key.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::NotFound`] for unknown, pending, and discarded
    /// targets alike, [`StoreError::LastControlToken`] when the target is the
    /// last active control token, [`StoreError::IdempotencyConflict`] for a
    /// reused key with a different digest, or another [`StoreError`].
    pub fn revoke_client_token(
        &mut self,
        command: &TokenCommand<'_>,
        target_token_id: &str,
    ) -> Result<Idempotent<StoredToken>, StoreError> {
        self.capability_root.verify_identity()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(replayed) = replay(&transaction, REVOKE_OPERATION, command)? {
            transaction.commit()?;
            return Ok(Idempotent::Replayed(replayed));
        }
        let target = load_token(&transaction, target_token_id)?
            .filter(|token| matches!(token.state, TokenState::Active | TokenState::Revoked))
            .ok_or(StoreError::NotFound("client token"))?;
        if target.state == TokenState::Active {
            if target.has_scope(CONTROL_SCOPE) && active_control_count(&transaction)? <= 1 {
                return Err(StoreError::LastControlToken);
            }
            transaction.execute(
                "UPDATE client_tokens SET status = 'revoked', revoked_at = ?2
                 WHERE token_id = ?1 AND status = 'active'",
                params![target_token_id, command.now],
            )?;
            audit(
                &transaction,
                command.now,
                "revoked",
                Some(command.principal_token_id),
                target_token_id,
            )?;
        }
        record_idempotency(&transaction, REVOKE_OPERATION, command, target_token_id)?;
        let revoked = require_token(&transaction, target_token_id)?;
        self.capability_root.verify_identity()?;
        transaction.commit()?;
        Ok(Idempotent::Created(revoked))
    }

    /// Offline bootstrap, step one: records a pending control token.
    ///
    /// Refuses when an active control token exists. Pending tokens left by an
    /// interrupted earlier bootstrap are discarded first. Returns how many
    /// stale pending tokens were discarded.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::ControlTokenExists`] when bootstrap is not
    /// needed, or another [`StoreError`] on failure.
    pub fn begin_bootstrap_token(&mut self, token: &NewClientToken<'_>) -> Result<u64, StoreError> {
        if token.scopes != [CONTROL_SCOPE] {
            return Err(StoreError::ConstraintViolation(
                "a bootstrap token must hold exactly the control scope".to_owned(),
            ));
        }
        self.capability_root.verify_identity()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if active_control_count(&transaction)? > 0 {
            return Err(StoreError::ControlTokenExists);
        }
        let stale = transaction
            .prepare(
                "SELECT token_id FROM client_tokens WHERE status = 'pending' ORDER BY token_id",
            )?
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for token_id in &stale {
            transaction.execute(
                "UPDATE client_tokens SET status = 'discarded' WHERE token_id = ?1",
                [token_id],
            )?;
            audit(
                &transaction,
                token.created_at,
                "bootstrap_discarded",
                None,
                token_id,
            )?;
        }
        insert_token(&transaction, token, TokenState::Pending, None)?;
        audit(
            &transaction,
            token.created_at,
            "bootstrap_issued",
            None,
            token.token_id,
        )?;
        self.capability_root.verify_identity()?;
        transaction.commit()?;
        Ok(stale.len() as u64)
    }

    /// Offline bootstrap, step two: activates the delivered pending token.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::NotFound`] unless the token is pending.
    pub fn activate_bootstrap_token(
        &mut self,
        token_id: &str,
        now: &str,
    ) -> Result<(), StoreError> {
        self.finish_bootstrap(token_id, now, TokenState::Active, "bootstrap_activated")
    }

    /// Offline bootstrap failure path: discards an undelivered pending token.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::NotFound`] unless the token is pending.
    pub fn discard_bootstrap_token(&mut self, token_id: &str, now: &str) -> Result<(), StoreError> {
        self.finish_bootstrap(token_id, now, TokenState::Discarded, "bootstrap_discarded")
    }

    fn finish_bootstrap(
        &mut self,
        token_id: &str,
        now: &str,
        state: TokenState,
        action: &str,
    ) -> Result<(), StoreError> {
        self.capability_root.verify_identity()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE client_tokens SET status = ?2
             WHERE token_id = ?1 AND status = 'pending' AND origin = 'bootstrap'",
            params![token_id, state.as_str()],
        )?;
        if changed != 1 {
            return Err(StoreError::NotFound("pending bootstrap token"));
        }
        audit(&transaction, now, action, None, token_id)?;
        self.capability_root.verify_identity()?;
        transaction.commit()?;
        Ok(())
    }
}

fn replay(
    connection: &Connection,
    operation: &str,
    command: &TokenCommand<'_>,
) -> Result<Option<StoredToken>, StoreError> {
    let Some((digest, result_token_id)) = connection
        .query_row(
            "SELECT canonical_request_sha256, result_token_id FROM token_idempotency_records
             WHERE principal_token_id = ?1 AND operation_id = ?2 AND idempotency_key = ?3",
            params![
                command.principal_token_id,
                operation,
                command.idempotency_key
            ],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?
    else {
        return Ok(None);
    };
    if digest != command.request_sha256 {
        return Err(StoreError::IdempotencyConflict);
    }
    require_token(connection, &result_token_id).map(Some)
}

fn record_idempotency(
    connection: &Connection,
    operation: &str,
    command: &TokenCommand<'_>,
    result_token_id: &str,
) -> Result<(), StoreError> {
    connection
        .execute(
            "INSERT INTO token_idempotency_records (
                principal_token_id, operation_id, idempotency_key,
                canonical_request_sha256, result_token_id, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                command.principal_token_id,
                operation,
                command.idempotency_key,
                command.request_sha256,
                result_token_id,
                command.now
            ],
        )
        .map(drop)
        .map_err(map_sqlite_error)
}

fn insert_token(
    connection: &Connection,
    token: &NewClientToken<'_>,
    state: TokenState,
    created_by: Option<&str>,
) -> Result<(), StoreError> {
    let origin = if created_by.is_some() {
        "api"
    } else {
        "bootstrap"
    };
    connection
        .execute(
            "INSERT INTO client_tokens (
                token_id, client_label, verifier_sha256, status, origin,
                created_by_token_id, created_at, revoked_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL)",
            params![
                token.token_id,
                token.client_label,
                token.verifier_sha256,
                state.as_str(),
                origin,
                created_by,
                token.created_at
            ],
        )
        .map_err(map_sqlite_error)?;
    for scope in token.scopes {
        connection
            .execute(
                "INSERT INTO client_token_scopes (token_id, scope) VALUES (?1, ?2)",
                params![token.token_id, scope],
            )
            .map_err(map_sqlite_error)?;
    }
    Ok(())
}

fn audit(
    connection: &Connection,
    now: &str,
    action: &str,
    actor_token_id: Option<&str>,
    target_token_id: &str,
) -> Result<(), StoreError> {
    connection
        .execute(
            "INSERT INTO token_audit_events (occurred_at, action, actor_token_id, target_token_id)
             VALUES (?1, ?2, ?3, ?4)",
            params![now, action, actor_token_id, target_token_id],
        )
        .map(drop)
        .map_err(map_sqlite_error)
}

fn active_control_count(connection: &Connection) -> Result<u64, StoreError> {
    connection
        .query_row(
            "SELECT COUNT(*) FROM client_tokens AS token
             JOIN client_token_scopes AS grant_row ON grant_row.token_id = token.token_id
             WHERE token.status = 'active' AND grant_row.scope = ?1",
            [CONTROL_SCOPE],
            |row| row.get(0),
        )
        .map_err(StoreError::from)
}

fn require_token(connection: &Connection, token_id: &str) -> Result<StoredToken, StoreError> {
    load_token(connection, token_id)?.ok_or(StoreError::NotFound("client token"))
}

fn load_token(connection: &Connection, token_id: &str) -> Result<Option<StoredToken>, StoreError> {
    let Some((client_label, state, created_at, revoked_at)) = connection
        .query_row(
            "SELECT client_label, status, created_at, revoked_at
             FROM client_tokens WHERE token_id = ?1",
            [token_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            },
        )
        .optional()?
    else {
        return Ok(None);
    };
    let state = TokenState::parse(&state)
        .ok_or_else(|| StoreError::InvalidPersistedValue(format!("token status {state:?}")))?;
    let scopes = connection
        .prepare("SELECT scope FROM client_token_scopes WHERE token_id = ?1 ORDER BY scope")?
        .query_map([token_id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if scopes.is_empty() {
        return Err(StoreError::InvalidPersistedValue(
            "client token without scopes".to_owned(),
        ));
    }
    Ok(Some(StoredToken {
        token_id: token_id.to_owned(),
        client_label,
        scopes,
        state,
        created_at,
        revoked_at,
    }))
}
