//! Frozen Phase 1 operation bindings and contract validation.

mod registry;
mod types;
mod validation;

pub use registry::{
    AccessDecision, AccessRule, AccessRuleKind, MappingState, OperationBinding,
    PHASE1_BINDING_COUNT, all_bindings, binding_for,
};
pub use types::*;
pub use validation::{
    CLAIM_TEXT_MAX_CHARS, MAX_OUTPUT_TAIL_CHARS, PROBLEM_DETAIL_MAX_CHARS, ValidationError,
    ValidationErrorKind, validate_problem, validate_problem_detail, validate_request,
    validate_response, validate_www_authenticate,
};
