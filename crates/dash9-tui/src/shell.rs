//! A reusable command-bar shell state machine, in the same spirit as
//! `LoreMesh`'s `loremesh-tui` shell (`docs/adr/0007-workbench-shell.md`
//! in that repo): [`ShellState::handle_key`] is a pure transition —
//! no terminal, filesystem, or network I/O, fully unit-testable —
//! that calls into a [`CommandHandler`] trait for anything that
//! actually needs to do work. The trait lives here (generic, no
//! `dash9-assist`/`tokio`/`reqwest` dependency, same rule
//! `check-architecture.sh` already enforces for this crate);
//! implementations with real I/O live in the `dash9` binary
//! (`GrammarOnlyHandler`, `AssistHandler`).
//!
//! Adopts three things dash9's command bar was missing relative to
//! `LoreMesh`'s: **Escape never quits** (only `q` outside input, or the
//! `quit` grammar verb — an accidental `Esc` no longer kills the
//! session), **command history** (`Up`/`Down` while typing cycles
//! previous submissions), and a pure/testable core instead of
//! key-handling inlined into the render loop.

use std::collections::VecDeque;
use std::time::{SystemTime, UNIX_EPOCH};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use dash9_core::{Command, CommandSource, LogLine, SessionLogEntry};

use crate::export::ExportFormat;
use crate::status_bar::StatusBarModel;

const MAX_LOG_LINES: usize = 500;
const MAX_HISTORY: usize = 50;

/// What one submitted command-bar line resolved to, before execution.
#[derive(Debug, Clone, PartialEq)]
pub enum ShellInput {
    Grammar(Command),
    Help,
    ModelStatus,
    ModelSwitch(String),
    Export {
        format: ExportFormat,
        path: Option<String>,
    },
    /// Carries the original parse error alongside the raw text: a
    /// handler without natural-language support (`GrammarOnlyHandler`)
    /// shows `parse_error` verbatim — the exact same message this
    /// input would have produced before any of this existed — while
    /// `AssistHandler` uses `text` to ask instead.
    NaturalLanguage {
        text: String,
        parse_error: dash9_core::CommandError,
    },
    ToggleAssist,
}

/// Parses one submitted line into a [`ShellInput`]. Recognizes the
/// shell-level meta-commands (`help`, `model`, `model <name>`,
/// `save <format> [path]`) before falling back to
/// `dash9_core::parse` — these are shell/UI concerns, not part of
/// SPEC.md's append-only command grammar, so they're intercepted
/// here rather than added to `dash9_core::Command`.
pub fn parse_shell_input(text: &str) -> ShellInput {
    let trimmed = text.trim();
    match trimmed {
        "help" | "?" => return ShellInput::Help,
        "model" => return ShellInput::ModelStatus,
        _ => {}
    }
    if let Some(rest) = trimmed.strip_prefix("model ") {
        let name = rest.trim();
        if !name.is_empty() {
            return ShellInput::ModelSwitch(name.to_string());
        }
    }
    if let Some(rest) = trimmed.strip_prefix("save ") {
        let mut parts = rest.trim().splitn(2, char::is_whitespace);
        let format_str = parts.next().unwrap_or("");
        if let Some(format) = ExportFormat::parse(format_str) {
            let path = parts
                .next()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from);
            return ShellInput::Export { format, path };
        }
        // Unrecognized format falls through to grammar parsing below,
        // which reports the same "unknown verb" shape any other
        // unrecognized input gets — no bespoke error path needed.
    }
    match dash9_core::parse(trimmed) {
        Ok(cmd) => ShellInput::Grammar(cmd),
        Err(parse_error) => ShellInput::NaturalLanguage {
            text: trimmed.to_string(),
            parse_error,
        },
    }
}

/// Outcome of one `CommandHandler::execute`/`poll` call.
#[derive(Debug, Default)]
pub struct CommandResponse {
    pub log_entries: Vec<LogLine>,
    pub new_proposals: Vec<Command>,
    pub should_quit: bool,
}

impl CommandResponse {
    pub fn result(text: impl Into<String>) -> Self {
        Self {
            log_entries: vec![LogLine::Result(text.into())],
            ..Self::default()
        }
    }

    pub fn none() -> Self {
        Self::default()
    }
}

/// The application boundary: `dash9-tui` defines the contract,
/// `dash9`'s binary crate implements it with real I/O (`LiveSession`
/// polling, `dash9-assist` calls). `execute` is synchronous — every
/// grammar verb dash9 has today except ad-hoc `q` and natural
/// language is already synchronous, and even those two just kick off
/// a background task and return immediately; `poll` is where results
/// from those background tasks (panel pollers, ad-hoc queries,
/// `ask()` calls) surface, checked once per render tick.
pub trait CommandHandler {
    fn execute(&mut self, input: ShellInput, focused_panel: usize) -> CommandResponse;
    fn poll(&mut self, focused_panel: usize) -> Option<CommandResponse>;
    fn panel_count(&self) -> usize;
    fn status_bar(&self) -> StatusBarModel;
}

/// Pure shell state: the command-bar buffer, the session log, pending
/// AI proposals awaiting `y`/`n`, and which panel has focus. No
/// terminal, filesystem, or network access anywhere in this type.
#[derive(Debug, Default)]
pub struct ShellState {
    pub input: Option<String>,
    pub log: Vec<LogLine>,
    pub pending_proposals: VecDeque<Command>,
    pub focused_panel: usize,
    history: VecDeque<String>,
    history_cursor: Option<usize>,
}

impl ShellState {
    /// Applies one keyboard event. Returns `true` only when the
    /// session should end — `Esc` never does, matching `LoreMesh`'s
    /// explicit invariant ("Escape never terminates... even when
    /// pressed repeatedly"); only `q` outside input, typing `quit`, or
    /// `Ctrl+C` (checked first, unconditionally, regardless of input
    /// state — raw mode intercepts it as a normal keypress rather than
    /// sending a real `SIGINT`, so without this a `Ctrl+C` habit either
    /// types a literal `c` while editing or does nothing at all, which
    /// reads as "the program won't quit") ends the session.
    pub fn handle_key<H: CommandHandler>(&mut self, key: KeyEvent, handler: &mut H) -> bool {
        if key.kind != KeyEventKind::Press {
            return false;
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return true;
        }

        if let Some(buffer) = self.input.as_mut() {
            match key.code {
                KeyCode::Esc => {
                    self.input = None;
                    self.history_cursor = None;
                }
                KeyCode::Enter => {
                    let text = buffer.clone();
                    self.input = None;
                    self.history_cursor = None;
                    if !text.trim().is_empty() {
                        return self.submit(&text, handler);
                    }
                }
                KeyCode::Backspace => {
                    buffer.pop();
                }
                KeyCode::Up => self.history_previous(),
                KeyCode::Down => self.history_next(),
                KeyCode::Char(c) => buffer.push(c),
                _ => {}
            }
            return false;
        }

        match key.code {
            KeyCode::Char('y') if !self.pending_proposals.is_empty() => {
                let cmd = self
                    .pending_proposals
                    .pop_front()
                    .expect("checked by !is_empty() above");
                let response = handler.execute(ShellInput::Grammar(cmd), self.focused_panel);
                return self.apply_response(response);
            }
            KeyCode::Char('n') if !self.pending_proposals.is_empty() => {
                self.pending_proposals.pop_front();
                self.push_log(LogLine::Result("proposal dismissed".to_string()));
            }
            KeyCode::Char('a') => {
                let response = handler.execute(ShellInput::ToggleAssist, self.focused_panel);
                return self.apply_response(response);
            }
            KeyCode::Char('q') => return true,
            KeyCode::Char(':') => self.input = Some(String::new()),
            KeyCode::Tab if handler.panel_count() > 0 => {
                self.focused_panel = (self.focused_panel + 1) % handler.panel_count();
            }
            KeyCode::BackTab if handler.panel_count() > 0 => {
                let len = handler.panel_count();
                self.focused_panel = (self.focused_panel + len - 1) % len;
            }
            _ => {}
        }
        false
    }

    /// Drains every background result currently ready, applying each
    /// one, then re-clamps `focused_panel` in case a `dash open`
    /// changed the panel count. Call once per render tick.
    pub fn apply_poll<H: CommandHandler>(&mut self, handler: &mut H) {
        while let Some(response) = handler.poll(self.focused_panel) {
            // A background result never ends the session on its own.
            self.apply_response(response);
        }
        if self.focused_panel >= handler.panel_count() {
            self.focused_panel = 0;
        }
    }

    fn submit<H: CommandHandler>(&mut self, text: &str, handler: &mut H) -> bool {
        self.push_history(text.to_string());
        self.push_log(LogLine::Command(SessionLogEntry {
            source: CommandSource::User,
            command_text: text.to_string(),
            timestamp_ms: epoch_ms_now(),
        }));
        let input = parse_shell_input(text);
        let response = handler.execute(input, self.focused_panel);
        self.apply_response(response)
    }

    fn apply_response(&mut self, response: CommandResponse) -> bool {
        self.log.extend(response.log_entries);
        self.pending_proposals.extend(response.new_proposals);
        self.trim_log();
        response.should_quit
    }

    fn push_log(&mut self, line: LogLine) {
        self.log.push(line);
        self.trim_log();
    }

    fn trim_log(&mut self) {
        if self.log.len() > MAX_LOG_LINES {
            let excess = self.log.len() - MAX_LOG_LINES;
            self.log.drain(0..excess);
        }
    }

    fn push_history(&mut self, text: String) {
        self.history.push_front(text);
        if self.history.len() > MAX_HISTORY {
            self.history.pop_back();
        }
    }

    fn history_previous(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let next = match self.history_cursor {
            None => 0,
            Some(i) => (i + 1).min(self.history.len() - 1),
        };
        self.history_cursor = Some(next);
        self.input = Some(self.history[next].clone());
    }

    fn history_next(&mut self) {
        match self.history_cursor {
            None => {}
            Some(0) => {
                self.history_cursor = None;
                self.input = Some(String::new());
            }
            Some(i) => {
                self.history_cursor = Some(i - 1);
                self.input = Some(self.history[i - 1].clone());
            }
        }
    }
}

fn epoch_ms_now() -> i64 {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;
    use dash9_core::{PanelType, RefreshInterval};

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn char_key(c: char) -> KeyEvent {
        press(KeyCode::Char(c))
    }

    /// Records every `execute`/`poll` call it receives so tests can
    /// assert on exactly what the shell asked it to do, decoupled
    /// from any real `LiveSession`/`dash9-assist` wiring.
    struct MockHandler {
        panel_count: usize,
        calls: Vec<ShellInput>,
        next_response: Option<CommandResponse>,
    }

    impl MockHandler {
        fn new(panel_count: usize) -> Self {
            Self {
                panel_count,
                calls: Vec::new(),
                next_response: None,
            }
        }
    }

    impl CommandHandler for MockHandler {
        fn execute(&mut self, input: ShellInput, _focused_panel: usize) -> CommandResponse {
            self.calls.push(input);
            self.next_response.take().unwrap_or_default()
        }
        fn poll(&mut self, _focused_panel: usize) -> Option<CommandResponse> {
            None
        }
        fn panel_count(&self) -> usize {
            self.panel_count
        }
        fn status_bar(&self) -> StatusBarModel {
            unreachable!("not exercised by these tests")
        }
    }

    fn type_and_submit(state: &mut ShellState, handler: &mut MockHandler, text: &str) -> bool {
        state.handle_key(char_key(':'), handler);
        for c in text.chars() {
            state.handle_key(char_key(c), handler);
        }
        state.handle_key(press(KeyCode::Enter), handler)
    }

    #[test]
    fn q_outside_input_quits() {
        let mut state = ShellState::default();
        let mut handler = MockHandler::new(1);
        assert!(state.handle_key(char_key('q'), &mut handler));
    }

    #[test]
    fn ctrl_c_always_quits_in_or_out_of_input() {
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);

        let mut state = ShellState::default();
        let mut handler = MockHandler::new(1);
        assert!(state.handle_key(ctrl_c, &mut handler), "outside input");

        let mut state = ShellState::default();
        let mut handler = MockHandler::new(1);
        state.handle_key(char_key(':'), &mut handler);
        state.handle_key(char_key('x'), &mut handler);
        assert!(state.handle_key(ctrl_c, &mut handler), "while typing");
    }

    #[test]
    fn plain_c_without_control_is_a_literal_character_while_typing() {
        let mut state = ShellState::default();
        let mut handler = MockHandler::new(1);
        state.handle_key(char_key(':'), &mut handler);
        let quit = state.handle_key(char_key('c'), &mut handler);
        assert!(!quit);
        assert_eq!(state.input.as_deref(), Some("c"));
    }

    #[test]
    fn esc_never_quits_in_or_out_of_input() {
        let mut state = ShellState::default();
        let mut handler = MockHandler::new(1);
        assert!(!state.handle_key(press(KeyCode::Esc), &mut handler));
        assert!(state.input.is_none());

        state.handle_key(char_key(':'), &mut handler);
        state.handle_key(char_key('x'), &mut handler);
        assert!(!state.handle_key(press(KeyCode::Esc), &mut handler));
        assert!(state.input.is_none(), "Esc cancels input, doesn't quit");
    }

    #[test]
    fn q_while_typing_is_a_literal_character_not_quit() {
        let mut state = ShellState::default();
        let mut handler = MockHandler::new(1);
        state.handle_key(char_key(':'), &mut handler);
        let quit = state.handle_key(char_key('q'), &mut handler);
        assert!(!quit);
        assert_eq!(state.input.as_deref(), Some("q"));
    }

    #[test]
    fn tab_and_backtab_cycle_panel_focus() {
        let mut state = ShellState::default();
        let mut handler = MockHandler::new(3);
        state.handle_key(press(KeyCode::Tab), &mut handler);
        assert_eq!(state.focused_panel, 1);
        state.handle_key(press(KeyCode::Tab), &mut handler);
        state.handle_key(press(KeyCode::Tab), &mut handler);
        assert_eq!(state.focused_panel, 0, "wraps around");
        state.handle_key(press(KeyCode::BackTab), &mut handler);
        assert_eq!(state.focused_panel, 2, "wraps backward");
    }

    #[test]
    fn submitting_grammar_text_logs_command_then_result_and_calls_handler() {
        let mut state = ShellState::default();
        let mut handler = MockHandler::new(1);
        handler.next_response = Some(CommandResponse::result("range set to 5m"));

        let quit = type_and_submit(&mut state, &mut handler, "range 5m");
        assert!(!quit);
        assert_eq!(handler.calls.len(), 1);
        assert!(matches!(
            handler.calls[0],
            ShellInput::Grammar(Command::Range { .. })
        ));
        assert_eq!(state.log.len(), 2, "one Command line, one Result line");
    }

    #[test]
    fn unparseable_text_reaches_handler_as_natural_language_with_the_parse_error_attached() {
        let mut state = ShellState::default();
        let mut handler = MockHandler::new(1);
        type_and_submit(&mut state, &mut handler, "please rename this");
        match &handler.calls[0] {
            ShellInput::NaturalLanguage { text, parse_error } => {
                assert_eq!(text, "please rename this");
                assert_eq!(parse_error.code, dash9_core::ErrorCode::E002);
            }
            other => panic!("expected NaturalLanguage, got {other:?}"),
        }
    }

    #[test]
    fn help_and_model_meta_commands_are_recognized_before_grammar_parsing() {
        assert_eq!(parse_shell_input("help"), ShellInput::Help);
        assert_eq!(parse_shell_input("?"), ShellInput::Help);
        assert_eq!(parse_shell_input("model"), ShellInput::ModelStatus);
        assert_eq!(
            parse_shell_input("model gemini-flash"),
            ShellInput::ModelSwitch("gemini-flash".to_string())
        );
    }

    #[test]
    fn save_meta_command_parses_format_and_optional_path() {
        assert_eq!(
            parse_shell_input("save csv"),
            ShellInput::Export {
                format: ExportFormat::Csv,
                path: None,
            }
        );
        assert_eq!(
            parse_shell_input("save md exports/out.md"),
            ShellInput::Export {
                format: ExportFormat::Markdown,
                path: Some("exports/out.md".to_string()),
            }
        );
    }

    #[test]
    fn save_with_unknown_format_falls_back_to_grammar_parse_error() {
        match parse_shell_input("save pdf out.pdf") {
            ShellInput::NaturalLanguage { .. } => {}
            other => panic!("expected a fallback parse failure, got {other:?}"),
        }
    }

    #[test]
    fn y_and_n_only_act_when_a_proposal_is_pending() {
        let mut state = ShellState::default();
        let mut handler = MockHandler::new(1);
        // No proposal pending: 'y'/'n' are inert, no handler call.
        state.handle_key(char_key('y'), &mut handler);
        state.handle_key(char_key('n'), &mut handler);
        assert!(handler.calls.is_empty());

        state.pending_proposals.push_back(Command::Refresh {
            interval: RefreshInterval::Off,
        });
        handler.next_response = Some(CommandResponse::result("refresh set to off"));
        state.handle_key(char_key('y'), &mut handler);
        assert_eq!(handler.calls.len(), 1);
        assert!(state.pending_proposals.is_empty());
    }

    #[test]
    fn n_dismisses_without_calling_the_handler() {
        let mut state = ShellState::default();
        let mut handler = MockHandler::new(1);
        state.pending_proposals.push_back(Command::PanelType {
            panel_type: PanelType::Gauge,
        });
        state.handle_key(char_key('n'), &mut handler);
        assert!(
            handler.calls.is_empty(),
            "dismissal never reaches the handler"
        );
        assert!(state.pending_proposals.is_empty());
        assert!(
            matches!(state.log.last(), Some(LogLine::Result(text)) if text == "proposal dismissed")
        );
    }

    #[test]
    fn history_recall_cycles_most_recent_first_and_back_to_empty() {
        let mut state = ShellState::default();
        let mut handler = MockHandler::new(1);
        type_and_submit(&mut state, &mut handler, "range 1h");
        type_and_submit(&mut state, &mut handler, "range 2h");

        state.handle_key(char_key(':'), &mut handler);
        state.handle_key(press(KeyCode::Up), &mut handler);
        assert_eq!(
            state.input.as_deref(),
            Some("range 2h"),
            "most recent first"
        );
        state.handle_key(press(KeyCode::Up), &mut handler);
        assert_eq!(state.input.as_deref(), Some("range 1h"));
        state.handle_key(press(KeyCode::Up), &mut handler);
        assert_eq!(state.input.as_deref(), Some("range 1h"), "stops at oldest");
        state.handle_key(press(KeyCode::Down), &mut handler);
        assert_eq!(state.input.as_deref(), Some("range 2h"));
        state.handle_key(press(KeyCode::Down), &mut handler);
        assert_eq!(
            state.input.as_deref(),
            Some(""),
            "back past newest clears input"
        );
    }

    #[test]
    fn empty_submission_does_not_log_or_call_the_handler() {
        let mut state = ShellState::default();
        let mut handler = MockHandler::new(1);
        state.handle_key(char_key(':'), &mut handler);
        state.handle_key(press(KeyCode::Enter), &mut handler);
        assert!(state.log.is_empty());
        assert!(handler.calls.is_empty());
    }

    #[test]
    fn non_press_events_are_ignored() {
        let mut state = ShellState::default();
        let mut handler = MockHandler::new(1);
        let mut release = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        assert!(!state.handle_key(release, &mut handler));
    }
}
