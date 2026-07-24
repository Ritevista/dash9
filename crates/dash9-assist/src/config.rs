//! Assistant configuration. Lives in its own file (default
//! `~/.config/dash9/assist.toml`, overridable), never the dashboard
//! TOML — dashboard files are meant to be shared/committed, and an
//! LLM endpoint config doesn't belong in a file whose whole purpose
//! is to be checked into a team's repo. See `docs/specs/assist.md`
//! Section D.

use serde::Deserialize;

fn default_timeout_ms() -> u64 {
    30_000
}

fn default_max_tokens() -> u32 {
    1024
}

#[derive(Debug, Clone, Deserialize)]
pub struct AssistConfig {
    pub base_url: String,
    pub model: String,
    /// Name of an environment variable holding the API key, e.g.
    /// `"OPENAI_API_KEY"`. The literal key value is never read from
    /// or written to this file — only the variable *name* is.
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    /// Reserved for v2. A config with `stream = true` fails to load
    /// (`ConfigError::StreamingNotSupported`) rather than silently
    /// falling back to non-streaming.
    #[serde(default)]
    pub stream: bool,
    /// Optional, purely a display/discoverability hint: model names
    /// the user knows their `base_url` endpoint serves, so a caller
    /// (e.g. `dash9 open --assist`'s `:model` command) can list real
    /// choices without querying a `/v1/models`-style endpoint — v1
    /// has no such client method, and that endpoint's shape wasn't
    /// reliable enough across the gateways tried during development
    /// to build on. Never read or written by anything in this crate
    /// beyond carrying it through; empty by default, fully backward
    /// compatible with configs written before this field existed.
    #[serde(default)]
    pub known_models: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read {path}: {source}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid assist config TOML: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("streaming is not supported in v1; set stream = false")]
    StreamingNotSupported,
}

impl AssistConfig {
    pub fn load(path: &std::path::Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Self::from_toml_str(&text)
    }

    pub fn from_toml_str(text: &str) -> Result<Self, ConfigError> {
        let config: AssistConfig = toml::from_str(text)?;
        if config.stream {
            return Err(ConfigError::StreamingNotSupported);
        }
        Ok(config)
    }

    /// Resolves the API key from the environment at call time — never
    /// cached, never logged, never part of any serialized form.
    pub fn resolve_api_key(&self) -> Option<String> {
        self.api_key_env
            .as_ref()
            .and_then(|var| std::env::var(var).ok())
    }

    /// Host only (e.g. `"localhost:11434"`), for a short status line —
    /// never the full URL, and there is never a key in it (the key is
    /// an env-var reference, never part of `base_url`).
    pub fn endpoint_host(&self) -> String {
        let after_scheme = self.base_url.split("://").nth(1).unwrap_or(&self.base_url);
        after_scheme
            .split('/')
            .next()
            .unwrap_or(after_scheme)
            .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_config_loads_with_defaults() {
        let config = AssistConfig::from_toml_str(
            r#"
base_url = "http://localhost:11434"
model = "llama3"
"#,
        )
        .unwrap();
        assert_eq!(config.timeout_ms, 30_000);
        assert_eq!(config.max_tokens, 1024);
        assert!(!config.stream);
        assert!(config.api_key_env.is_none());
        assert!(
            config.known_models.is_empty(),
            "a config written before this field existed must still load"
        );
    }

    #[test]
    fn known_models_round_trips_when_present() {
        let config = AssistConfig::from_toml_str(
            r#"
base_url = "http://localhost:4000"
model = "cerebras-gpt-oss"
known_models = ["cerebras-gpt-oss", "gemini-flash"]
"#,
        )
        .unwrap();
        assert_eq!(
            config.known_models,
            vec!["cerebras-gpt-oss".to_string(), "gemini-flash".to_string()]
        );
    }

    #[test]
    fn stream_true_is_a_load_error() {
        let err = AssistConfig::from_toml_str(
            r#"
base_url = "http://localhost:11434"
model = "llama3"
stream = true
"#,
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::StreamingNotSupported));
    }

    #[test]
    fn api_key_is_not_resolved_when_the_env_var_is_unset() {
        // Setting an env var requires `std::env::set_var`, which is
        // `unsafe` and this workspace forbids unsafe code entirely
        // (`unsafe_code = "forbid"`), so this only exercises the
        // "unset" branch. `resolve_api_key`'s "set" branch is a
        // one-line `std::env::var(..).ok()` call with no branching
        // logic of its own left to cover.
        let config = AssistConfig::from_toml_str(
            r#"
base_url = "http://localhost:11434"
model = "llama3"
api_key_env = "DASH9_TEST_ASSIST_KEY_UNSET_7f3a"
"#,
        )
        .unwrap();
        assert!(config.resolve_api_key().is_none());
    }

    #[test]
    fn api_key_is_none_when_not_configured() {
        let config = AssistConfig::from_toml_str(
            r#"
base_url = "http://localhost:11434"
model = "llama3"
"#,
        )
        .unwrap();
        assert!(config.resolve_api_key().is_none());
    }

    #[test]
    fn endpoint_host_strips_scheme_and_path() {
        let config = AssistConfig::from_toml_str(
            r#"
base_url = "http://localhost:11434/some/path"
model = "llama3"
"#,
        )
        .unwrap();
        assert_eq!(config.endpoint_host(), "localhost:11434");
    }

    #[test]
    fn missing_file_is_reported() {
        let err =
            AssistConfig::load(std::path::Path::new("/nonexistent/dash9-assist.toml")).unwrap_err();
        assert!(matches!(err, ConfigError::Io { .. }));
    }
}
