//! The LLM client. One OpenAI-compatible code path
//! (`POST {base_url}/v1/chat/completions`) serves Ollama, vLLM, LM
//! Studio, and on-prem gateways — no vendor-specific branches, no SDK
//! dependency. See `docs/specs/assist.md` Section D.
//!
//! `LlmClient` is a plain (non-`dyn`) async trait: `AssistSession` is
//! generic over it rather than boxing, since which client is in use
//! (`HttpLlmClient` for a real session, `FixtureLlmClient` for
//! `dash9 demo --assist` and its tests) is always known at compile
//! time — there is no runtime switch between them.

use serde::{Deserialize, Serialize};

use crate::config::AssistConfig;
use crate::error::AssistError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatRole {
    System,
    User,
    Assistant,
}

impl ChatRole {
    fn as_wire_str(self) -> &'static str {
        match self {
            ChatRole::System => "system",
            ChatRole::User => "user",
            ChatRole::Assistant => "assistant",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: String,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::System,
            content: content.into(),
        }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::User,
            content: content.into(),
        }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::Assistant,
            content: content.into(),
        }
    }
}

/// Parsed from the OpenAI-compatible `usage` object
/// (`{"prompt_tokens", "completion_tokens", "total_tokens"}`). Not
/// every compatible server populates it reliably, so callers always
/// see this behind an `Option`, never a value assumed present.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TokenUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

impl std::ops::AddAssign for TokenUsage {
    fn add_assign(&mut self, other: Self) {
        self.prompt_tokens += other.prompt_tokens;
        self.completion_tokens += other.completion_tokens;
        self.total_tokens += other.total_tokens;
    }
}

#[derive(Debug)]
pub struct ChatReply {
    pub content: String,
    pub usage: Option<TokenUsage>,
}

pub trait LlmClient: Sync {
    fn chat(
        &self,
        messages: &[ChatMessage],
    ) -> impl std::future::Future<Output = Result<ChatReply, AssistError>> + Send;
}

pub struct HttpLlmClient {
    config: AssistConfig,
    client: reqwest::Client,
}

impl HttpLlmClient {
    pub fn new(config: AssistConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(config.timeout_ms))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { config, client }
    }
}

#[derive(Serialize)]
struct WireMessage<'a> {
    role: &'a str,
    content: &'a str,
}

impl<'a> From<&'a ChatMessage> for WireMessage<'a> {
    fn from(message: &'a ChatMessage) -> Self {
        WireMessage {
            role: message.role.as_wire_str(),
            content: &message.content,
        }
    }
}

#[derive(Serialize)]
struct ChatCompletionRequest<'a> {
    model: &'a str,
    messages: Vec<WireMessage<'a>>,
    max_tokens: u32,
}

#[derive(Deserialize)]
struct ChatCompletionResponse {
    #[serde(default)]
    choices: Vec<ChoiceWire>,
    #[serde(default)]
    usage: Option<UsageWire>,
}

#[derive(Deserialize)]
struct ChoiceWire {
    message: MessageWire,
}

#[derive(Deserialize)]
struct MessageWire {
    content: String,
}

#[derive(Deserialize)]
#[allow(clippy::struct_field_names)]
struct UsageWire {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
}

impl LlmClient for HttpLlmClient {
    async fn chat(&self, messages: &[ChatMessage]) -> Result<ChatReply, AssistError> {
        let url = format!(
            "{}/v1/chat/completions",
            self.config.base_url.trim_end_matches('/')
        );
        let wire_messages: Vec<WireMessage<'_>> = messages.iter().map(WireMessage::from).collect();
        let body = ChatCompletionRequest {
            model: &self.config.model,
            messages: wire_messages,
            max_tokens: self.config.max_tokens,
        };

        let mut request = self.client.post(&url).json(&body);
        if let Some(key) = self.config.resolve_api_key() {
            request = request.bearer_auth(key);
        }

        let response = request.send().await.map_err(|source| {
            if source.is_timeout() {
                AssistError::Timeout
            } else {
                AssistError::EndpointUnreachable(source.to_string())
            }
        })?;
        let text = response
            .text()
            .await
            .map_err(|source| AssistError::EndpointUnreachable(source.to_string()))?;
        let parsed: ChatCompletionResponse =
            serde_json::from_str(&text).map_err(|_| AssistError::NonJsonResponse(text.clone()))?;
        let choice = parsed
            .choices
            .into_iter()
            .next()
            .ok_or(AssistError::EmptyResponse)?;

        Ok(ChatReply {
            content: choice.message.content,
            usage: parsed.usage.map(|usage| TokenUsage {
                prompt_tokens: usage.prompt_tokens,
                completion_tokens: usage.completion_tokens,
                total_tokens: usage.total_tokens,
            }),
        })
    }
}

/// One canned request/reply pair for `dash9 demo --assist` and
/// `tests/fixtures_replay.rs`. See `docs/specs/assist.md` Section K.
#[derive(Debug, Clone, Deserialize)]
pub struct Fixture {
    pub request: String,
    pub reply: String,
}

/// A zero-network stand-in for `HttpLlmClient`: matches the most
/// recent `user` message against a fixed set of canned fixtures by
/// exact text and returns the corresponding reply verbatim. Real
/// keystrokes and the real contract/validate/execute path still run
/// — only the network call is replaced.
pub struct FixtureLlmClient {
    fixtures: Vec<Fixture>,
}

impl FixtureLlmClient {
    pub fn new(fixtures: Vec<Fixture>) -> Self {
        Self { fixtures }
    }

    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        let fixtures: Vec<Fixture> = serde_json::from_str(json)?;
        Ok(Self::new(fixtures))
    }
}

impl LlmClient for FixtureLlmClient {
    async fn chat(&self, messages: &[ChatMessage]) -> Result<ChatReply, AssistError> {
        let last_user = messages
            .iter()
            .rev()
            .find(|message| message.role == ChatRole::User);
        let Some(last_user) = last_user else {
            return Err(AssistError::EndpointUnreachable(
                "no fixture for this input: no user message in history".to_string(),
            ));
        };
        self.fixtures
            .iter()
            .find(|fixture| fixture.request == last_user.content)
            .map(|fixture| ChatReply {
                content: fixture.reply.clone(),
                usage: None,
            })
            .ok_or_else(|| {
                AssistError::EndpointUnreachable(format!(
                    "no fixture for this input: {:?}",
                    last_user.content
                ))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_usage_add_assign_sums_fields() {
        let mut total = TokenUsage {
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
        };
        total += TokenUsage {
            prompt_tokens: 2,
            completion_tokens: 3,
            total_tokens: 5,
        };
        assert_eq!(total.prompt_tokens, 12);
        assert_eq!(total.completion_tokens, 8);
        assert_eq!(total.total_tokens, 20);
    }

    #[tokio::test]
    async fn fixture_client_matches_last_user_message() {
        let client = FixtureLlmClient::new(vec![Fixture {
            request: "show cpu".to_string(),
            reply: "```\nq up\n```".to_string(),
        }]);
        let reply = client.chat(&[ChatMessage::user("show cpu")]).await.unwrap();
        assert_eq!(reply.content, "```\nq up\n```");
        assert!(reply.usage.is_none());
    }

    #[tokio::test]
    async fn fixture_client_reports_a_clear_error_for_unknown_input() {
        let client = FixtureLlmClient::new(vec![]);
        let err = client
            .chat(&[ChatMessage::user("unknown request")])
            .await
            .unwrap_err();
        assert!(matches!(err, AssistError::EndpointUnreachable(msg) if msg.contains("no fixture")));
    }
}

#[cfg(test)]
mod live_tests {
    //! Same throwaway-local-TCP-listener technique as
    //! `dash9-prom`'s `live_tests`: no real network access, no
    //! mocking dependency, fully offline and deterministic. Proves
    //! `HttpLlmClient`'s request-building/response-reading wrapper
    //! actually works, not just the JSON shapes in isolation.
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn respond_once(body: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut socket, _)) = listener.accept().await {
                let mut buf = [0u8; 8192];
                let _ = socket.read(&mut buf).await;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            }
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn chat_hits_the_http_api_and_parses_content_and_usage() {
        let body = r#"{"choices":[{"message":{"role":"assistant","content":"```\nq up\n```"}}],"usage":{"prompt_tokens":50,"completion_tokens":10,"total_tokens":60}}"#;
        let base_url = respond_once(body).await;
        let config = AssistConfig::from_toml_str(&format!(
            "base_url = \"{base_url}\"\nmodel = \"demo-model\"\n"
        ))
        .unwrap();
        let client = HttpLlmClient::new(config);
        let reply = client.chat(&[ChatMessage::user("show cpu")]).await.unwrap();
        assert_eq!(reply.content, "```\nq up\n```");
        assert_eq!(
            reply.usage,
            Some(TokenUsage {
                prompt_tokens: 50,
                completion_tokens: 10,
                total_tokens: 60,
            })
        );
    }

    #[tokio::test]
    async fn chat_reports_empty_response_when_choices_are_missing() {
        let body = r#"{"choices":[]}"#;
        let base_url = respond_once(body).await;
        let config = AssistConfig::from_toml_str(&format!(
            "base_url = \"{base_url}\"\nmodel = \"demo-model\"\n"
        ))
        .unwrap();
        let client = HttpLlmClient::new(config);
        let err = client
            .chat(&[ChatMessage::user("show cpu")])
            .await
            .unwrap_err();
        assert!(matches!(err, AssistError::EmptyResponse));
    }

    #[tokio::test]
    async fn chat_reports_non_json_response() {
        let base_url = respond_once("not json").await;
        let config = AssistConfig::from_toml_str(&format!(
            "base_url = \"{base_url}\"\nmodel = \"demo-model\"\n"
        ))
        .unwrap();
        let client = HttpLlmClient::new(config);
        let err = client
            .chat(&[ChatMessage::user("show cpu")])
            .await
            .unwrap_err();
        assert!(matches!(err, AssistError::NonJsonResponse(_)));
    }
}
