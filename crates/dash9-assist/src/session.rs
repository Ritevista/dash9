//! `AssistSession`: the contract loop (Section G) tied to a running
//! session's state — history, the on/off toggle, and cumulative token
//! usage (Section L). `ask()` is a method here, not a bare function,
//! because something has to hold that state across calls, and that
//! something is the session, not the caller.

use std::path::PathBuf;

use dash9_core::Command;

use crate::classify::{classify, ProposedCommand};
use crate::client::{ChatMessage, LlmClient, TokenUsage};
use crate::config::AssistConfig;
use crate::context::AssistContext;
use crate::contract::{process_reply, ProcessedReply};
use crate::error::AssistError;
use crate::prompt::system_prompt;
use crate::status::{AssistStatusModel, ConnectivityState};

/// Two repair turns, three attempts total (Section G step 6).
const MAX_REPAIRS: u8 = 2;

#[derive(Debug, Clone, PartialEq)]
pub struct AssistTurn {
    pub intent_sentence: Option<String>,
    pub commands: Vec<ProposedCommand>,
    pub raw_reply: String,
    /// Summed across every attempt this turn made, including repairs.
    pub usage: Option<TokenUsage>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AssistOutcome {
    Turn(AssistTurn),
    Refusal(String),
    Failed(AssistError),
}

pub struct AssistSession<C: LlmClient> {
    client: C,
    model: String,
    endpoint_host: String,
    enabled: bool,
    connectivity: ConnectivityState,
    session_total_usage: TokenUsage,
    history: Vec<ChatMessage>,
    workspace_root: PathBuf,
}

impl<C: LlmClient> AssistSession<C> {
    pub fn new(client: C, config: &AssistConfig, workspace_root: PathBuf) -> Self {
        Self {
            client,
            model: config.model.clone(),
            endpoint_host: config.endpoint_host(),
            enabled: true,
            connectivity: ConnectivityState::Idle,
            session_total_usage: TokenUsage::default(),
            history: Vec::new(),
            workspace_root,
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Number of messages (user + assistant, repair exchanges included)
    /// in the running conversation history — `/ai context`
    /// (`docs/specs/open.md` Section D)'s window into otherwise-opaque
    /// session state, so "why doesn't it remember X" is answerable
    /// without guessing.
    pub fn history_len(&self) -> usize {
        self.history.len()
    }

    /// `/ai clear` — resets the conversation, independent of model or
    /// endpoint (unlike switching models, which resets history only as
    /// a side effect of rebuilding the whole session for a different
    /// model/client).
    pub fn clear_history(&mut self) {
        self.history.clear();
    }

    /// Distinct from the `assist` Cargo feature (Section B), which
    /// controls whether this code is compiled in at all. This toggles
    /// whether *this session* currently calls out to the LLM: turning
    /// it off makes `ask()` an immediate no-op refusal, not just a
    /// hidden UI element. The model/endpoint from construction resume
    /// unchanged when turned back on — there is no runtime model or
    /// endpoint switching (Section N).
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        self.connectivity = if enabled {
            ConnectivityState::Idle
        } else {
            ConnectivityState::Disabled
        };
    }

    pub fn status(&self) -> AssistStatusModel {
        AssistStatusModel {
            model: self.model.clone(),
            endpoint_host: self.endpoint_host.clone(),
            connectivity: self.connectivity.clone(),
            last_turn_usage: None,
            session_total_usage: self.session_total_usage,
        }
    }

    /// Runs the full contract loop for one user `request`: builds the
    /// prompt, calls the LLM, validates the reply, repairs up to two
    /// times, and classifies the result. `focused_panel`
    /// describes current session state the caller owns (dash9-assist
    /// never touches TUI state itself, per its one-effector rule) —
    /// `dash9 demo --assist` always passes `true`, since its one panel
    /// is always the thing being addressed.
    ///
    /// If the session is disabled, returns `AssistOutcome::Refusal`
    /// immediately and makes no network call at all.
    pub async fn ask(
        &mut self,
        context: &AssistContext,
        request: &str,
        focused_panel: bool,
    ) -> AssistOutcome {
        if !self.enabled {
            return AssistOutcome::Refusal("the assistant is turned off".to_string());
        }

        let mut messages = vec![ChatMessage::system(system_prompt(context))];
        messages.extend(self.history.iter().cloned());
        messages.push(ChatMessage::user(request.to_string()));

        let mut turn_usage = TokenUsage::default();
        let mut had_usage = false;

        for attempt in 0..=MAX_REPAIRS {
            self.connectivity = ConnectivityState::Waiting;

            let reply = match self.client.chat(&messages).await {
                Ok(reply) => reply,
                Err(error) => {
                    self.connectivity = ConnectivityState::Error(error.to_string());
                    return AssistOutcome::Failed(error);
                }
            };
            if let Some(usage) = reply.usage {
                turn_usage += usage;
                had_usage = true;
            }
            messages.push(ChatMessage::assistant(reply.content.clone()));

            match process_reply(&reply.content, focused_panel, &self.workspace_root) {
                Ok(ProcessedReply::Refusal(sentence)) => {
                    self.finish_turn(&messages, had_usage, turn_usage);
                    return AssistOutcome::Refusal(sentence);
                }
                Ok(ProcessedReply::Commands {
                    intent_sentence,
                    commands,
                }) => {
                    self.finish_turn(&messages, had_usage, turn_usage);
                    let proposed = commands_into_proposals(commands);
                    return AssistOutcome::Turn(AssistTurn {
                        intent_sentence,
                        commands: proposed,
                        raw_reply: reply.content,
                        usage: had_usage.then_some(turn_usage),
                    });
                }
                Err(failure) => {
                    if attempt == MAX_REPAIRS {
                        let error = AssistError::ContractViolationAfterRepairs {
                            attempts: attempt + 1,
                            last_reply: reply.content,
                            last_error: failure.into_command_error(),
                        };
                        self.connectivity = ConnectivityState::Error(error.to_string());
                        return AssistOutcome::Failed(error);
                    }
                    messages.push(ChatMessage::user(failure.repair_message()));
                }
            }
        }

        unreachable!(
            "the loop above always returns by the attempt == MAX_REPAIRS branch at the latest"
        )
    }

    fn finish_turn(&mut self, messages: &[ChatMessage], had_usage: bool, turn_usage: TokenUsage) {
        self.connectivity = ConnectivityState::Idle;
        // Drop the system prompt (index 0); keep the rest as history
        // for the next turn's conversation continuity.
        self.history = messages[1..].to_vec();
        if had_usage {
            self.session_total_usage += turn_usage;
        }
    }
}

fn commands_into_proposals(commands: Vec<Command>) -> Vec<ProposedCommand> {
    commands.into_iter().map(classify).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{ChatReply, Fixture, FixtureLlmClient};
    use crate::context::TimeRangeSummary;

    fn empty_context() -> AssistContext {
        AssistContext {
            datasources: vec![],
            active_datasource_metadata: None,
            dashboard_toml: None,
            time_range: TimeRangeSummary {
                start_ms: 0,
                end_ms: 1000,
            },
        }
    }

    fn config() -> AssistConfig {
        AssistConfig::from_toml_str(
            "base_url = \"http://demo.invalid\"\nmodel = \"demo-fixture\"\n",
        )
        .unwrap()
    }

    #[tokio::test]
    async fn successful_turn_classifies_and_records_no_usage_when_fixture_reports_none() {
        let client = FixtureLlmClient::new(vec![Fixture {
            request: "show cpu".to_string(),
            reply: "Sure.\n```\nq up\n```".to_string(),
        }]);
        let mut session = AssistSession::new(client, &config(), std::env::temp_dir());
        let outcome = session.ask(&empty_context(), "show cpu", true).await;
        match outcome {
            AssistOutcome::Turn(turn) => {
                assert_eq!(turn.intent_sentence.as_deref(), Some("Sure."));
                assert_eq!(turn.commands.len(), 1);
                assert!(matches!(turn.commands[0], ProposedCommand::AutoRun(_)));
                assert!(turn.usage.is_none());
            }
            other => panic!("expected Turn, got {other:?}"),
        }
        assert_eq!(session.status().session_total_usage, TokenUsage::default());
    }

    #[tokio::test]
    async fn disabled_session_never_calls_the_client() {
        let client = FixtureLlmClient::new(vec![]); // would error if ever called
        let mut session = AssistSession::new(client, &config(), std::env::temp_dir());
        session.set_enabled(false);
        let outcome = session.ask(&empty_context(), "anything", true).await;
        assert_eq!(
            outcome,
            AssistOutcome::Refusal("the assistant is turned off".to_string())
        );
        assert_eq!(session.status().connectivity, ConnectivityState::Disabled);
    }

    #[tokio::test]
    async fn refusal_reply_returns_refusal_outcome() {
        let client = FixtureLlmClient::new(vec![Fixture {
            request: "delete all my data".to_string(),
            reply: "I can't do that with the commands I have.".to_string(),
        }]);
        let mut session = AssistSession::new(client, &config(), std::env::temp_dir());
        let outcome = session
            .ask(&empty_context(), "delete all my data", true)
            .await;
        assert_eq!(
            outcome,
            AssistOutcome::Refusal("I can't do that with the commands I have.".to_string())
        );
    }

    #[tokio::test]
    async fn missing_fixture_fails_immediately_without_repairs() {
        let client = FixtureLlmClient::new(vec![]);
        let mut session = AssistSession::new(client, &config(), std::env::temp_dir());
        let outcome = session.ask(&empty_context(), "unknown", true).await;
        assert!(matches!(
            outcome,
            AssistOutcome::Failed(AssistError::EndpointUnreachable(_))
        ));
    }

    /// A minimal client that always returns an invalid command, to
    /// exercise the repair-then-fail path end to end.
    struct AlwaysInvalidClient;
    impl LlmClient for AlwaysInvalidClient {
        async fn chat(&self, _messages: &[ChatMessage]) -> Result<ChatReply, AssistError> {
            Ok(ChatReply {
                content: "```\nnot a real verb\n```".to_string(),
                usage: None,
            })
        }
    }

    #[tokio::test]
    async fn contract_violation_surviving_all_repairs_fails_with_attempt_count() {
        let mut session = AssistSession::new(AlwaysInvalidClient, &config(), std::env::temp_dir());
        let outcome = session.ask(&empty_context(), "do something", true).await;
        match outcome {
            AssistOutcome::Failed(AssistError::ContractViolationAfterRepairs {
                attempts, ..
            }) => {
                assert_eq!(attempts, MAX_REPAIRS + 1);
            }
            other => panic!("expected ContractViolationAfterRepairs, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn re_enabling_keeps_the_original_model_and_endpoint() {
        let client = FixtureLlmClient::new(vec![]);
        let mut session = AssistSession::new(client, &config(), std::env::temp_dir());
        session.set_enabled(false);
        session.set_enabled(true);
        let status = session.status();
        assert_eq!(status.model, "demo-fixture");
        assert_eq!(status.connectivity, ConnectivityState::Idle);
    }

    #[tokio::test]
    async fn history_len_is_zero_before_any_turn() {
        let client = FixtureLlmClient::new(vec![]);
        let session = AssistSession::new(client, &config(), std::env::temp_dir());
        assert_eq!(session.history_len(), 0);
    }

    #[tokio::test]
    async fn a_completed_turn_grows_history_and_clear_history_resets_it() {
        let client = FixtureLlmClient::new(vec![Fixture {
            request: "show cpu".to_string(),
            reply: "Sure.\n```\nq up\n```".to_string(),
        }]);
        let mut session = AssistSession::new(client, &config(), std::env::temp_dir());
        session.ask(&empty_context(), "show cpu", true).await;
        assert!(
            session.history_len() > 0,
            "a completed turn should leave the user request + assistant reply in history"
        );

        session.clear_history();
        assert_eq!(session.history_len(), 0);
    }
}
