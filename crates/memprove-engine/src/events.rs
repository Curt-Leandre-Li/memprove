//! `ingestEvent`, `listEvents`, and `getEvent` application service (ADR 0010).
//!
//! The service stamps the daemon's receipt time; the store assigns ledger
//! positions and computes every digest. Persistence is a port implemented by
//! the daemon over `memprove-store-sqlite`.

use memprove_domain::{
    CanonicalJson, CorrelationId, Event, EventId, EventInput, EventProducer, LedgerVerification,
    PrivacyClass, ProjectId, SessionId, UtcTimestamp, WorkspaceId,
};

use crate::catalog::{Command, Page};

/// Why an Event operation did not succeed. Each variant maps to exactly one
/// contract Problem code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventError {
    /// The request cannot be served as asked (for example an unknown cursor).
    /// The text is the `detail` of the `400` Problem: the field or parameter
    /// and the rule it violates, never a value from the request.
    InvalidRequest(String),
    /// No `ACTIVE` Project has the requested identity.
    ProjectNotFound,
    /// The Session or Event does not exist.
    ResourceNotFound,
    /// The Workspace is not the Workspace of the Event's Session.
    ProjectWorkspaceMismatch,
    /// The idempotency key was reused with a different request, or the
    /// `event_id` is already recorded for another request.
    IdempotencyConflict,
    /// The Session is not active; it accepts no further Event.
    InvalidTransition,
    /// P4-H1 (Owner, T20 SF-1/SF-2): a stored Event the read needs does not
    /// decode or recompute, so no retry helps. On the wire it is
    /// `409 capsule_tampered`.
    Tampered,
    /// P4-H1 (review of the first P4-H1 commit): the Project's state
    /// database is not bound to it: its binding (`state_metadata`) or its
    /// migration record (`schema_migrations`) is missing, does not decode,
    /// or differs, so none of it is read as the Project's and no retry
    /// helps. On the wire it is `409 capsule_tampered`.
    UnboundState,
    /// A backing store failed; the operation fails closed.
    Unavailable,
}

/// A validated envelope as received, before the daemon stamps its receipt
/// time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewEvent {
    pub event_id: EventId,
    pub project_id: ProjectId,
    pub workspace_id: WorkspaceId,
    pub session_id: SessionId,
    pub event_type: String,
    pub payload_schema: String,
    pub payload: CanonicalJson,
    pub occurred_at: Option<UtcTimestamp>,
    pub producer: EventProducer,
    pub correlation_id: Option<CorrelationId>,
    pub causation_id: Option<EventId>,
    pub privacy_class: PrivacyClass,
}

/// Persistence port for the Event ledger.
pub trait EventStore: Send + Sync {
    /// Appends (or replays) one Event in its Project.
    ///
    /// # Errors
    ///
    /// [`EventError::ProjectNotFound`], [`EventError::ResourceNotFound`] for an
    /// unknown Session, [`EventError::ProjectWorkspaceMismatch`],
    /// [`EventError::InvalidTransition`] for a Session that is not active,
    /// [`EventError::IdempotencyConflict`], or [`EventError::Unavailable`].
    fn ingest(&self, input: &EventInput, command: &Command) -> Result<Event, EventError>;

    /// Reads one Event of any `ACTIVE` Project.
    ///
    /// # Errors
    ///
    /// [`EventError::ResourceNotFound`] or [`EventError::Unavailable`].
    fn event(&self, event_id: &EventId) -> Result<Event, EventError>;

    /// Reads a page of the Project's Events after `after` in Project order.
    ///
    /// # Errors
    ///
    /// [`EventError::ProjectNotFound`], [`EventError::InvalidRequest`] for an
    /// unknown cursor, or [`EventError::Unavailable`].
    fn list(
        &self,
        project_id: &ProjectId,
        after: Option<&EventId>,
        limit: u32,
    ) -> Result<Page<Event>, EventError>;

    /// Recomputes the Project's ledger: the Event chain and every Evidence,
    /// Snapshot, and Checkpoint digest. Writes nothing.
    ///
    /// # Errors
    ///
    /// [`EventError::ProjectNotFound`] or [`EventError::Unavailable`]; a
    /// ledger that does not recompute is a result, not an error.
    fn verify_ledger(&self, project_id: &ProjectId) -> Result<LedgerVerification, EventError>;
}

impl<T: EventStore + ?Sized> EventStore for std::sync::Arc<T> {
    fn ingest(&self, input: &EventInput, command: &Command) -> Result<Event, EventError> {
        (**self).ingest(input, command)
    }

    fn event(&self, event_id: &EventId) -> Result<Event, EventError> {
        (**self).event(event_id)
    }

    fn list(
        &self,
        project_id: &ProjectId,
        after: Option<&EventId>,
        limit: u32,
    ) -> Result<Page<Event>, EventError> {
        (**self).list(project_id, after, limit)
    }

    fn verify_ledger(&self, project_id: &ProjectId) -> Result<LedgerVerification, EventError> {
        (**self).verify_ledger(project_id)
    }
}

/// Application service for the three Event operations.
pub struct EventLedger {
    store: Box<dyn EventStore>,
}

impl EventLedger {
    /// Creates the service over its store port.
    #[must_use]
    pub fn new(store: Box<dyn EventStore>) -> Self {
        Self { store }
    }

    /// `ingestEvent`: stamps the receipt time and appends (or replays) the
    /// Event. A replay returns the Event recorded by the first request.
    ///
    /// # Errors
    ///
    /// The store's [`EventError`].
    pub fn ingest(&self, new: NewEvent, command: &Command) -> Result<Event, EventError> {
        let input = EventInput {
            event_id: new.event_id,
            project_id: new.project_id,
            workspace_id: new.workspace_id,
            session_id: Some(new.session_id),
            event_type: new.event_type,
            payload_schema: new.payload_schema,
            payload: new.payload,
            occurred_at: new.occurred_at,
            recorded_at: UtcTimestamp::now(),
            producer: new.producer,
            correlation_id: new.correlation_id,
            causation_id: new.causation_id,
            privacy_class: new.privacy_class,
        };
        self.store.ingest(&input, command)
    }

    /// `getEvent`.
    ///
    /// # Errors
    ///
    /// [`EventError::ResourceNotFound`] or [`EventError::Unavailable`].
    pub fn get_event(&self, event_id: &EventId) -> Result<Event, EventError> {
        self.store.event(event_id)
    }

    /// `listEvents` of one Project.
    ///
    /// # Errors
    ///
    /// [`EventError::ProjectNotFound`], [`EventError::InvalidRequest`] for an
    /// unknown cursor, or [`EventError::Unavailable`].
    pub fn list_events(
        &self,
        project_id: &ProjectId,
        after: Option<&EventId>,
        limit: u32,
    ) -> Result<Page<Event>, EventError> {
        self.store.list(project_id, after, limit)
    }

    /// `verifyLedger` (P2-C): recomputes the Project's ledger. Read-only.
    ///
    /// # Errors
    ///
    /// [`EventError::ProjectNotFound`] or [`EventError::Unavailable`].
    pub fn verify_ledger(&self, project_id: &ProjectId) -> Result<LedgerVerification, EventError> {
        self.store.verify_ledger(project_id)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use memprove_domain::{IdempotencyContext, IdempotencyKey, OperationId, PrincipalId};

    use super::*;

    #[derive(Default)]
    struct RecordingStore {
        seen: Mutex<Vec<EventInput>>,
    }

    impl EventStore for RecordingStore {
        fn ingest(&self, input: &EventInput, _command: &Command) -> Result<Event, EventError> {
            self.seen.lock().unwrap().push(input.clone());
            Event::new(input.clone(), Some(1), 1, None)
                .map_err(|error| EventError::InvalidRequest(error.to_string()))
        }

        fn event(&self, _event_id: &EventId) -> Result<Event, EventError> {
            Err(EventError::ResourceNotFound)
        }

        fn list(
            &self,
            _project_id: &ProjectId,
            _after: Option<&EventId>,
            _limit: u32,
        ) -> Result<Page<Event>, EventError> {
            Err(EventError::ProjectNotFound)
        }

        fn verify_ledger(&self, _project_id: &ProjectId) -> Result<LedgerVerification, EventError> {
            Err(EventError::ProjectNotFound)
        }
    }

    #[test]
    fn ingest_stamps_a_receipt_time_and_keeps_the_declared_occurrence() {
        let store = std::sync::Arc::new(RecordingStore::default());
        let ledger = EventLedger::new(Box::new(std::sync::Arc::clone(&store)));
        let new = NewEvent {
            event_id: EventId::new(),
            project_id: ProjectId::new(),
            workspace_id: WorkspaceId::new(),
            session_id: SessionId::new(),
            event_type: "turn.started".to_owned(),
            payload_schema: "turn.started@1".to_owned(),
            payload: CanonicalJson::parse("{}").unwrap(),
            occurred_at: None,
            producer: EventProducer {
                name: "hook".to_owned(),
                version: "1".to_owned(),
                sequence: 4,
            },
            correlation_id: None,
            causation_id: None,
            privacy_class: PrivacyClass::Public,
        };
        let command = Command {
            context: IdempotencyContext {
                principal_id: PrincipalId::parse("principal").unwrap(),
                operation_id: OperationId::parse("ingestEvent").unwrap(),
                key: IdempotencyKey::parse("key-0000000000001").unwrap(),
            },
            request_digest: memprove_domain::Sha256Digest::of_bytes(b"request"),
        };
        let event = ledger.ingest(new.clone(), &command).unwrap();
        assert_eq!(event.occurred_at(), None);
        assert_eq!(event.producer(), &new.producer);
        assert_eq!(store.seen.lock().unwrap().len(), 1);
        assert_eq!(
            ledger.get_event(&new.event_id),
            Err(EventError::ResourceNotFound)
        );
        assert_eq!(
            ledger.list_events(&new.project_id, None, 1),
            Err(EventError::ProjectNotFound)
        );
    }
}
