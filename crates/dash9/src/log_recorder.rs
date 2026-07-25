//! `/record on|off [path]`: continuous, append-as-you-go recording of
//! the session log to a JSONL file — one JSON object per `LogLine`,
//! written the instant it's added rather than exported as a single
//! point-in-time snapshot (`:save` already covers that for panels).
//! The motivating use case is building new commands/skills from a
//! session's history, which wants a machine-parseable command
//! sequence, not a one-shot dump you have to remember to trigger
//! before the thing you wanted has already scrolled past.
//!
//! Handler-agnostic on purpose — `GrammarOnlyHandler` and
//! `AssistHandler` both hold the same `Arc<Mutex<LogRecorder>>` and
//! call identical methods on it; nothing here is assist-specific.

use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::PathBuf;

use dash9_core::{validate_workspace_relative_path, CommandSource, LogLine};

use crate::datasources::epoch_ms_now;

pub struct LogRecorder {
    workspace_root: PathBuf,
    file: Option<std::fs::File>,
    path: Option<String>,
    lines_written: u64,
}

impl LogRecorder {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self {
            workspace_root,
            file: None,
            path: None,
            lines_written: 0,
        }
    }

    /// `/record on [path]` / `/record off`. Opens in append mode, not
    /// truncate — stopping and restarting a recording against the
    /// same path (in this session or a later run) accumulates one
    /// continuous history instead of losing what came before.
    pub fn set(&mut self, on: bool, path: Option<String>) -> String {
        if !on {
            return self.stop();
        }
        let target = path.unwrap_or_else(default_recording_path);
        let destination = match validate_workspace_relative_path(&self.workspace_root, &target) {
            Ok(p) => p,
            Err(err) => return err.to_string(),
        };
        if let Some(parent) = destination.parent() {
            if let Err(err) = std::fs::create_dir_all(parent) {
                return format!("could not create {}: {err}", parent.display());
            }
        }
        match OpenOptions::new()
            .create(true)
            .append(true)
            .open(&destination)
        {
            Ok(file) => {
                self.file = Some(file);
                self.path = Some(target.clone());
                self.lines_written = 0;
                format!("recording to {target} (appending)")
            }
            Err(err) => format!("could not open {target}: {err}"),
        }
    }

    fn stop(&mut self) -> String {
        let Some(path) = self.path.take() else {
            return "recording already off".to_string();
        };
        self.file = None;
        let written = self.lines_written;
        self.lines_written = 0;
        format!("recording stopped ({written} lines written to {path})")
    }

    pub fn status(&self) -> String {
        match &self.path {
            Some(path) => format!(
                "recording: on ({path}, {} lines this session)",
                self.lines_written
            ),
            None => "recording: off".to_string(),
        }
    }

    /// Appends every given line as one JSONL record. A no-op when
    /// recording is off — callers never need to check `is_on()`
    /// first, same "off means harmless no-op" shape as every other
    /// toggle in this codebase (`/ai off`, a disabled assist session).
    pub fn record(&mut self, lines: &[LogLine]) {
        let Some(file) = self.file.as_mut() else {
            return;
        };
        for line in lines {
            let value = match line {
                LogLine::Command(entry) => serde_json::json!({
                    "type": "command",
                    "source": match entry.source {
                        CommandSource::User => "user",
                        CommandSource::Assistant => "assistant",
                    },
                    "text": entry.command_text,
                    "timestamp_ms": entry.timestamp_ms,
                }),
                // `LogLine::Result` carries no timestamp of its own
                // (see `dash9_core::session`) — it's recorded the
                // instant it's produced, so "now" is an accurate
                // stand-in, not an approximation of something else.
                LogLine::Result(text) => serde_json::json!({
                    "type": "result",
                    "text": text,
                    "timestamp_ms": epoch_ms_now(),
                }),
            };
            if writeln!(file, "{value}").is_ok() {
                self.lines_written += 1;
            }
        }
    }
}

fn default_recording_path() -> String {
    format!("exports/session-{}.jsonl", epoch_ms_now())
}

#[cfg(test)]
mod tests {
    use super::*;
    use dash9_core::SessionLogEntry;
    use std::io::BufRead as _;

    fn read_lines(path: &std::path::Path) -> Vec<serde_json::Value> {
        let file = std::fs::File::open(path).unwrap();
        std::io::BufReader::new(file)
            .lines()
            .map(|line| serde_json::from_str(&line.unwrap()).unwrap())
            .collect()
    }

    #[test]
    fn off_by_default_and_record_is_a_no_op() {
        let workspace = tempfile::tempdir().unwrap();
        let mut recorder = LogRecorder::new(workspace.path().to_path_buf());
        recorder.record(&[LogLine::Result("hello".to_string())]);
        assert!(recorder.status().contains("off"));
    }

    #[test]
    fn on_then_off_reports_lines_written_and_stops_recording() {
        let workspace = tempfile::tempdir().unwrap();
        let mut recorder = LogRecorder::new(workspace.path().to_path_buf());

        let on_msg = recorder.set(true, Some("session.jsonl".to_string()));
        assert!(on_msg.contains("recording to session.jsonl"), "{on_msg}");
        assert!(recorder.status().contains("on"));

        recorder.record(&[
            LogLine::Command(SessionLogEntry {
                source: CommandSource::User,
                command_text: "/range 5m".to_string(),
                timestamp_ms: 1,
            }),
            LogLine::Result("range set to 5m".to_string()),
        ]);

        let off_msg = recorder.set(false, None);
        assert!(off_msg.contains("2 lines written"), "{off_msg}");
        assert!(recorder.status().contains("off"));

        let lines = read_lines(&workspace.path().join("session.jsonl"));
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["type"], "command");
        assert_eq!(lines[0]["source"], "user");
        assert_eq!(lines[0]["text"], "/range 5m");
        assert_eq!(lines[1]["type"], "result");
        assert_eq!(lines[1]["text"], "range set to 5m");
    }

    #[test]
    fn off_when_already_off_says_so() {
        let workspace = tempfile::tempdir().unwrap();
        let mut recorder = LogRecorder::new(workspace.path().to_path_buf());
        assert!(recorder.set(false, None).contains("already off"));
    }

    #[test]
    fn restarting_a_recording_appends_instead_of_truncating() {
        let workspace = tempfile::tempdir().unwrap();
        let mut recorder = LogRecorder::new(workspace.path().to_path_buf());

        recorder.set(true, Some("session.jsonl".to_string()));
        recorder.record(&[LogLine::Result("first".to_string())]);
        recorder.set(false, None);

        recorder.set(true, Some("session.jsonl".to_string()));
        recorder.record(&[LogLine::Result("second".to_string())]);
        recorder.set(false, None);

        let lines = read_lines(&workspace.path().join("session.jsonl"));
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["text"], "first");
        assert_eq!(lines[1]["text"], "second");
    }

    #[test]
    fn path_escaping_the_workspace_is_rejected() {
        let workspace = tempfile::tempdir().unwrap();
        let mut recorder = LogRecorder::new(workspace.path().to_path_buf());
        let msg = recorder.set(true, Some("../escape.jsonl".to_string()));
        assert!(msg.contains("E107"), "{msg}");
    }

    #[test]
    fn default_path_is_used_when_none_given() {
        let workspace = tempfile::tempdir().unwrap();
        let mut recorder = LogRecorder::new(workspace.path().to_path_buf());
        let msg = recorder.set(true, None);
        assert!(msg.contains("exports/session-"), "{msg}");
    }
}
