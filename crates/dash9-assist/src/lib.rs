//! Optional AI assistant for dash9. See `docs/specs/assist.md`.
//!
//! Exactly one effector: it emits dash9 command-grammar text, which
//! is validated by the same `dash9_core::parse()` every other command
//! source goes through, then classified by the same read-only /
//! state-changing execution policy a human's input would follow. If a
//! capability cannot be expressed as a command, the assistant does
//! not have that capability.
//!
//! This crate never depends on `dash9-tui` or `dash9-prom`, and its
//! only network access, ever, is its own configured LLM endpoint —
//! fetching anything else (datasource metadata, dashboard file
//! contents) is the composition root's job, handed in as
//! already-fetched data via [`context::AssistContext`].

pub mod classify;
pub mod client;
pub mod config;
pub mod context;
pub mod contract;
pub mod error;
pub mod prompt;
pub mod session;
pub mod status;

/// The canned request/reply fixtures `dash9 demo --assist` and
/// `tests/fixtures_replay.rs` both use — one copy, embedded at
/// compile time, so the binary never needs to know this crate's
/// on-disk layout. See `docs/specs/assist.md` Section K.
pub const DEMO_FIXTURES_JSON: &str = include_str!("../fixtures/demo.json");

pub use classify::ProposedCommand;
pub use client::{
    ChatMessage, ChatReply, ChatRole, Fixture, FixtureLlmClient, HttpLlmClient, LlmClient,
    TokenUsage,
};
pub use config::{AssistConfig, ConfigError};
pub use context::{ActiveDatasourceMetadata, AssistContext, DatasourceSummary, TimeRangeSummary};
pub use error::AssistError;
pub use session::{AssistOutcome, AssistSession, AssistTurn};
pub use status::{AssistStatusModel, ConnectivityState};
