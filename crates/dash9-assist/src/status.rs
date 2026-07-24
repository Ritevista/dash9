//! Observable session status. See `docs/specs/assist.md` Section L:
//! plain data, no network or terminal access needed to read it —
//! `AssistStatusModel::render_text()` follows the same
//! no-Ratatui-dependency pattern as `dash9_tui::chart::ChartModel`.

use crate::client::TokenUsage;

#[derive(Debug, Clone, PartialEq)]
pub enum ConnectivityState {
    /// Toggled off; `ask()` will not make a network call.
    Disabled,
    /// Enabled, nothing in flight; the last call (if any) succeeded.
    Idle,
    /// A request is currently in flight.
    Waiting,
    /// The last call failed; a short, human-readable summary of why.
    Error(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct AssistStatusModel {
    pub model: String,
    pub endpoint_host: String,
    pub connectivity: ConnectivityState,
    /// From the most recently completed turn, including its repairs.
    pub last_turn_usage: Option<TokenUsage>,
    /// Cumulative across every turn this session. Stays at zero,
    /// never estimated, if the endpoint never reports `usage`.
    pub session_total_usage: TokenUsage,
}

impl AssistStatusModel {
    pub fn render_text(&self) -> String {
        let connectivity = match &self.connectivity {
            ConnectivityState::Disabled => "off".to_string(),
            ConnectivityState::Idle => "idle".to_string(),
            ConnectivityState::Waiting => "waiting".to_string(),
            ConnectivityState::Error(message) => format!("error: {message}"),
        };
        format!(
            "{} @ {} — {} — {} tokens this session",
            self.model, self.endpoint_host, connectivity, self.session_total_usage.total_tokens
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_text_reports_every_field() {
        let status = AssistStatusModel {
            model: "llama3".to_string(),
            endpoint_host: "localhost:11434".to_string(),
            connectivity: ConnectivityState::Idle,
            last_turn_usage: None,
            session_total_usage: TokenUsage {
                prompt_tokens: 100,
                completion_tokens: 20,
                total_tokens: 120,
            },
        };
        let text = status.render_text();
        assert!(text.contains("llama3"));
        assert!(text.contains("localhost:11434"));
        assert!(text.contains("idle"));
        assert!(text.contains("120 tokens"));
    }

    #[test]
    fn error_state_surfaces_the_message() {
        let status = AssistStatusModel {
            model: "llama3".to_string(),
            endpoint_host: "localhost:11434".to_string(),
            connectivity: ConnectivityState::Error("timed out".to_string()),
            last_turn_usage: None,
            session_total_usage: TokenUsage::default(),
        };
        assert!(status.render_text().contains("error: timed out"));
    }
}
