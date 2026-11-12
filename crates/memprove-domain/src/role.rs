//! U9 R1 (Owner O4; ADR 0020): the writes a Session that declared the role
//! `sub_agent` may not make. The role is the one the Session declared at
//! `createSession` (U8 A1), which the Task Capsule's ledger view reads
//! (`sessions[].agent`); a claim's author is the Session that recorded it.
//! Nothing here reads anything else.
//!
//! The rule guards against a mistake, not a pretence: an agent declares its
//! own role, and the token does not bind it (ADR 0020, "What it does not
//! do").

use crate::{
    AgentRole, ClaimId, ClaimKind, EdgeRelation, ProposedEdge, Session, SessionId, TaskCapsuleView,
    capsule::readable_claim_kinds,
};

/// Which of the three writes was refused; each has one fixed `detail`
/// that states the rule and who can make the write instead, and never a
/// statement's text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoleRule {
    /// (i) `proposeDecision`.
    Decision,
    /// (ii) `transitionTask` to `completed` whose `session_id` names a
    /// `sub_agent` Session, whatever runs it cites (U9 SF1-A, the Owner's
    /// O8 exception; a request without one is not judged).
    Completion,
    /// (iii) a `supersedes` edge whose target another agent wrote and which
    /// is not an open item, or a `withdraws` edge whose target another agent
    /// wrote (an open item too; U9 S-R round 1).
    Edge,
}

impl RoleRule {
    /// The `detail` of the `403 role_forbidden` Problem.
    #[must_use]
    pub const fn detail(self) -> &'static str {
        match self {
            Self::Decision => {
                "a Session declared as sub_agent cannot record a Decision; the orchestrating session \
                 or a person does it"
            }
            Self::Completion => {
                "a Session declared as sub_agent cannot move the Task to completed; the \
                 orchestrating session or a person does it, and they can cite any Session's runs"
            }
            Self::Edge => {
                "a Session declared as sub_agent can supersede open items and the statements \
                 written by Sessions of the same agent name, and can withdraw only statements \
                 written by Sessions of the same agent name; superseding another agent's facts, \
                 next actions, and Decisions, and withdrawing any statement another agent wrote, \
                 is for the orchestrating session or a person"
            }
        }
    }
}

/// What a Session writes that the rule looks at.
#[derive(Debug, Clone, Copy)]
pub enum RoleWrite<'a> {
    /// A Decision (with any edges it declares).
    Decision,
    /// The edges a Checkpoint declares.
    Edges(&'a [ProposedEdge]),
}

/// Whether `session` declared the role `sub_agent`.
#[must_use]
pub fn is_sub_agent(session: &Session) -> bool {
    session
        .agent
        .as_ref()
        .is_some_and(|agent| agent.role == AgentRole::SubAgent)
}

/// The rule `write` by `session_id` breaks, `None` when it breaks none. A
/// Session that is not in the view, declared no agent, or declared another
/// role is not limited. For a `sub_agent`, an edge's target is allowed when
/// its author Session declared the same agent name, and a `supersedes`
/// target also when it is an open item (the exception is for superseding
/// only: U8 O2 lets a sub-agent supersede its stage's open items, and
/// withdrawing one takes it off the list, the mistake R1 guards against);
/// an author that is not in the view or declared no agent is another
/// agent. A target that is no readable claim is not judged here
/// ([`crate::check_edges`] refuses it first).
#[must_use]
pub fn role_refusal(
    view: &TaskCapsuleView,
    session_id: &SessionId,
    write: RoleWrite<'_>,
) -> Option<RoleRule> {
    let session_of = |id: &SessionId| {
        view.sessions
            .iter()
            .map(|record| &record.session)
            .find(|session| &session.session_id == id)
    };
    let writer = session_of(session_id).filter(|session| is_sub_agent(session))?;
    let name = writer.agent.as_ref().map(|agent| agent.name.as_str());
    let edges = match write {
        RoleWrite::Decision => return Some(RoleRule::Decision),
        RoleWrite::Edges(edges) => edges,
    };
    let kinds = readable_claim_kinds(view);
    let author = |target: &ClaimId| match target {
        ClaimId::Statement(claim) => view
            .checkpoints
            .iter()
            .find(|record| record.checkpoint.checkpoint_id() == &claim.checkpoint_id)
            .map(|record| record.checkpoint.session_id().clone()),
        ClaimId::Decision(event_id) => view
            .decisions
            .iter()
            .find(|decision| &decision.decision_id == event_id)
            .map(|decision| decision.session_id.clone()),
    };
    edges
        .iter()
        .filter(|edge| kinds.contains_key(&edge.target))
        .any(|edge| {
            let open_item_superseded = edge.relation == EdgeRelation::Supersedes
                && kinds.get(&edge.target) == Some(&ClaimKind::OpenItem);
            !open_item_superseded
                && author(&edge.target)
                    .as_ref()
                    .and_then(session_of)
                    .and_then(|author| author.agent.as_ref())
                    .map(|agent| agent.name.as_str())
                    != name
        })
        .then_some(RoleRule::Edge)
}
