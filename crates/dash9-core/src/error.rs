//! Stable error codes for the command grammar and dashboard TOML
//! validation. See `SPEC.md` Section B.5.

use std::fmt;

/// Stable for the lifetime of the grammar: a code is never repurposed
/// (SPEC.md B.1, B.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    /// Empty command (blank or whitespace-only line).
    E001,
    /// Unknown verb.
    E002,
    /// Unknown subverb.
    E003,
    /// Unterminated quoted string.
    E004,
    /// Arity mismatch — wrong number of arguments.
    E005,
    /// Argument fails value-level validation (bad duration, bad enum
    /// value, non-numeric threshold, malformed dashboard TOML content).
    E006,
    /// Unknown datasource reference.
    E101,
    /// Duplicate datasource name in `ds add` / `[[datasources]]`.
    E102,
    /// `panel *` verb issued with no panel focused.
    E103,
    /// `dash open` path does not exist or is not readable.
    E104,
    /// `dash save` path is not writable.
    E105,
    /// Query execution failed (wraps the datasource adapter's error).
    E106,
    /// `dash save` / `dash open` path resolves outside the workspace
    /// root (absolute path, `..` traversal, or a symlink escape).
    /// Added for `docs/specs/assist.md`; append-only per SPEC.md B.1/B.5
    /// — does not change any existing code's meaning.
    E107,
    /// A panel imported from Grafana JSON that couldn't be resolved to
    /// a runnable query (unmapped panel type, non-Prometheus
    /// datasource, or an unresolved template variable) was polled
    /// anyway — an internal invariant violation, since `executable ==
    /// false` panels are never supposed to reach a live datasource
    /// call (`docs/specs/grafana-dashboards.md` Section A/B). Doubles
    /// as the placeholder shown in the panel's own pane instead of a
    /// live result.
    E108,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::E001 => "E001",
            ErrorCode::E002 => "E002",
            ErrorCode::E003 => "E003",
            ErrorCode::E004 => "E004",
            ErrorCode::E005 => "E005",
            ErrorCode::E006 => "E006",
            ErrorCode::E101 => "E101",
            ErrorCode::E102 => "E102",
            ErrorCode::E103 => "E103",
            ErrorCode::E104 => "E104",
            ErrorCode::E105 => "E105",
            ErrorCode::E106 => "E106",
            ErrorCode::E107 => "E107",
            ErrorCode::E108 => "E108",
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Every parse or validation failure in the command grammar (and, per
/// SPEC.md C.3, dashboard TOML validation where the failure is
/// semantically the same kind of error) is one of these.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{code}: {message}")]
pub struct CommandError {
    pub code: ErrorCode,
    pub message: String,
    /// Byte offsets into the input line, when the error can be
    /// localized (currently only `E004`).
    pub span: Option<(usize, usize)>,
}

impl CommandError {
    pub fn new(code: ErrorCode, message: impl Into<String>, span: Option<(usize, usize)>) -> Self {
        Self {
            code,
            message: message.into(),
            span,
        }
    }
}
