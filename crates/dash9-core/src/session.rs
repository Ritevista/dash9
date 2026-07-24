//! Shared session-log entry shape. See `docs/specs/assist.md` Section
//! C.3: every assistant-originated command must land in the same
//! session log as a human-typed one, marked as assistant-originated —
//! but no live session log exists yet (`dash9 open`'s interactive
//! session isn't built). Rather than let an assistant integration
//! invent its own log shape now and reconcile it with `open`'s later,
//! both key off this one small, shared type added ahead of either.
//!
//! `dash9-core` does not own a live, growing log — that's session
//! state, owned by whatever runs the interactive loop. It only owns
//! the entry shape so every producer agrees on it from day one.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandSource {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionLogEntry {
    pub source: CommandSource,
    pub command_text: String,
    pub timestamp_ms: i64,
}

/// One line in an interactive session's displayed log: either a
/// command that was issued (human-typed or, later, assistant-proposed
/// — see [`SessionLogEntry`]) or the text outcome of one (a query
/// result, an error, a confirmation). Kept distinct from
/// `SessionLogEntry` because an outcome isn't itself a command and
/// has no `CommandSource`.
#[derive(Debug, Clone, PartialEq)]
pub enum LogLine {
    Command(SessionLogEntry),
    Result(String),
}
