//! Assistant-level failures. See `docs/specs/assist.md` Section J:
//! shown to the user verbatim, never silently retried beyond the
//! repair budget, never papered over with a guessed command.

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum AssistError {
    #[error("assistant endpoint unreachable: {0}")]
    EndpointUnreachable(String),
    #[error("assistant request timed out")]
    Timeout,
    #[error("assistant returned a non-JSON response: {0}")]
    NonJsonResponse(String),
    #[error("assistant response had no choices")]
    EmptyResponse,
    #[error("contract violation after {attempts} attempt(s): {last_error}")]
    ContractViolationAfterRepairs {
        attempts: u8,
        last_reply: String,
        last_error: dash9_core::CommandError,
    },
}
