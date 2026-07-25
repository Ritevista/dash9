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
//! Adopts things dash9's command bar was missing relative to
//! `LoreMesh`: **Escape never quits** (only `q` outside input, `/quit`,
//! or `Ctrl+C`), **command history** (`Up`/`Down` while typing cycles
//! previous submissions), and a pure/testable core instead of
//! key-handling inlined into the render loop.
//!
//! Command routing is explicit-prefix, not fallback: a submitted line
//! starting with `/` is always a structured command (a shell
//! meta-verb, or SPEC.md grammar via `dash9_core::parse`) —
//! unrecognized text after `/` is a hard error, never silently
//! retried as natural language. A line with **no** leading `/` is
//! always natural language, even if it happens to look like valid
//! grammar (`dash9_core::parse` is never called on it). `Tab`/
//! `Shift+Tab` cycle through every panel and then into the command
//! box itself (a `panel_count() + 1`-stop ring), so the command box
//! is reachable without needing to already know about `:` — but once
//! inside and actively composing text, `Tab`/`Shift+Tab` are inert;
//! they must never silently discard what's typed by navigating away,
//! so only `Esc` (cancel) or `Enter` (submit) leave the box.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::time::{SystemTime, UNIX_EPOCH};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use dash9_core::{Command, CommandError, CommandSource, LogLine, SessionLogEntry, VerbSpec};

use crate::export::ExportFormat;
use crate::status_bar::StatusBarModel;

const MAX_LOG_LINES: usize = 500;
const MAX_HISTORY: usize = 50;

/// What one submitted command-bar line resolved to, before execution.
#[derive(Debug, Clone, PartialEq)]
pub enum ShellInput {
    Grammar(Command),
    /// `/help` or `/?` (`None`), or `/help <topic>` (`Some`).
    Help(Option<String>),
    ModelStatus,
    ModelSwitch(String),
    /// Bare `/ai`: combined enabled/disabled + model status.
    AssistStatus,
    /// `/ai on` / `/ai off` — explicit, idempotent (unlike the bare
    /// `a` key, which toggles).
    SetAssist(bool),
    ToggleAssist,
    Export {
        format: ExportFormat,
        path: Option<String>,
    },
    /// Bare `/record`: whether continuous log recording is on, and
    /// where.
    RecordingStatus,
    /// `/record on [path]` / `/record off` — continuous, append-as-
    /// you-go recording of every log line to a file, distinct from
    /// `Export`'s one-shot panel snapshot.
    SetRecording {
        on: bool,
        path: Option<String>,
    },
    /// A line with no leading `/` — always sent as-is, never
    /// grammar-parsed first.
    NaturalLanguage(String),
    /// A line that started with `/` but didn't match any shell
    /// meta-verb or `dash9_core` grammar verb.
    CommandError(CommandError),
}

/// Shell-level meta-commands (`help`, `model`, `ai`, `save`, `quit`)
/// in the same `VerbSpec` shape `dash9_core::VERB_REFERENCE` uses —
/// these are deliberately *not* part of SPEC.md's append-only grammar
/// (they're shell/UI concerns, not something an automated caller
/// would ever propose), but sharing the shape lets `/help` group and
/// render both sources identically.
const SHELL_META_REFERENCE: &[VerbSpec] = &[
    VerbSpec {
        verb: "help",
        args: &["topic?"],
        example: "/help ds",
        description: "List commands, or show detail for one topic.",
    },
    VerbSpec {
        verb: "model",
        args: &[],
        example: "/model",
        description: "Show the current AI model and any configured known models.",
    },
    VerbSpec {
        verb: "model",
        args: &["name"],
        example: "/model gemini-flash",
        description: "Switch the AI model (resets conversation history).",
    },
    VerbSpec {
        verb: "ai",
        args: &[],
        example: "/ai",
        description: "Show whether the assistant is on/off and the current model.",
    },
    VerbSpec {
        verb: "ai on",
        args: &[],
        example: "/ai on",
        description: "Turn the assistant on.",
    },
    VerbSpec {
        verb: "ai off",
        args: &[],
        example: "/ai off",
        description: "Turn the assistant off.",
    },
    VerbSpec {
        verb: "ai model",
        args: &["name"],
        example: "/ai model gemini-flash",
        description: "Switch the AI model (alias of \"/model <name>\").",
    },
    VerbSpec {
        verb: "save",
        args: &["format", "path?"],
        example: "/save csv exports/out.csv",
        description: "Export the focused panel's data (csv, md, or png).",
    },
    VerbSpec {
        verb: "record",
        args: &[],
        example: "/record",
        description: "Show whether continuous log recording is on, and where.",
    },
    VerbSpec {
        verb: "record on",
        args: &["path?"],
        example: "/record on exports/session.jsonl",
        description:
            "Start recording every log line (JSONL, one record per line, appended) to a file.",
    },
    VerbSpec {
        verb: "record off",
        args: &[],
        example: "/record off",
        description: "Stop recording.",
    },
    VerbSpec {
        verb: "quit",
        args: &[],
        example: "/quit",
        description: "End the session.",
    },
];

/// One-line blurbs for `/help`'s top-level listing — written
/// separately from `VERB_REFERENCE`/`SHELL_META_REFERENCE`'s
/// per-verb descriptions since a group summary ("manage datasources")
/// reads better at the top level than any single member verb's own
/// description would.
const GROUP_BLURBS: &[(&str, &str)] = &[
    ("ds", "manage datasources (add, list)"),
    ("q", "run an instant query against the focused datasource"),
    (
        "panel",
        "configure the focused panel (type, threshold, title)",
    ),
    ("range", "set the view's time range"),
    ("refresh", "set the auto-refresh interval"),
    ("dash", "save or open a dashboard file"),
    ("save", "export the focused panel's data (csv, md, png)"),
    ("record", "continuously record the log to a file (JSONL)"),
    ("model", "show or switch the AI model"),
    ("ai", "AI on/off/model (--assist only)"),
    ("help", "show this list, or /help <name> for detail"),
    ("quit", "end the session"),
];

fn all_specs() -> impl Iterator<Item = &'static VerbSpec> {
    dash9_core::VERB_REFERENCE
        .iter()
        .chain(SHELL_META_REFERENCE.iter())
}

/// A verb's group is its first whitespace-separated token — `"ds
/// add"` groups under `"ds"`, `"ai model"` groups under `"ai"`, a
/// single-token verb like `"range"` is its own one-member group.
fn group_of(verb: &str) -> &str {
    verb.split_whitespace().next().unwrap_or(verb)
}

/// The full command reference: `/help` (bare) lists top-level groups,
/// `/help <topic>` drills into one. Pure, no I/O — shared by every
/// `CommandHandler` implementation instead of being duplicated per
/// handler.
pub fn help_text(topic: Option<&str>) -> String {
    match topic {
        None => help_overview(),
        Some(t) => help_topic(t),
    }
}

fn help_overview() -> String {
    let mut out = String::from(
        "Commands — prefix with / (e.g. /range 5m). Type \"/help <name>\" for detail:\n",
    );
    for (name, blurb) in GROUP_BLURBS {
        let _ = writeln!(out, "  {name:<7} {blurb}");
    }
    out.push_str(
        "\nNo leading / — the whole line goes to the AI as natural language \
         (needs --assist and AI on).\n",
    );
    out.push_str(
        "Keys: Tab/Shift+Tab cycle panels + command box · : jump to command box \
         (Esc cancels) · i toggle focused panel's detail view (Esc closes) · \
         PageUp/PageDown scroll log · y/n confirm proposal · \
         a toggle AI · q or Ctrl+C quit\n",
    );
    out
}

/// `topic` containing a space (e.g. `"ds add"`) is matched exactly
/// against one verb; a single-word topic (e.g. `"ds"`, `"model"`)
/// matches every verb in that group, which also naturally covers
/// single-verb groups like `"range"` (a group of one).
fn help_topic(topic: &str) -> String {
    let topic = topic.trim();
    let matches: Vec<&VerbSpec> = if topic.contains(' ') {
        all_specs().filter(|spec| spec.verb == topic).collect()
    } else {
        all_specs()
            .filter(|spec| group_of(spec.verb) == topic)
            .collect()
    };
    if matches.is_empty() {
        return format!("unknown help topic \"{topic}\" — try /help");
    }
    let mut out = String::new();
    for spec in matches {
        let args = spec.args.join(" ");
        let _ = writeln!(out, "/{} {args}", spec.verb);
        let _ = writeln!(out, "  {}", spec.description);
        let _ = writeln!(out, "  e.g. {}\n", spec.example);
    }
    out.trim_end().to_string()
}

/// Parses one submitted line. A leading `/` is the only thing that
/// makes a line a structured command — everything after it must
/// match a shell meta-verb or `dash9_core::parse`, or the result is
/// [`ShellInput::CommandError`]. No leading `/` is always
/// [`ShellInput::NaturalLanguage`], unconditionally — the text is
/// never handed to `dash9_core::parse` at all.
pub fn parse_shell_input(text: &str) -> ShellInput {
    let trimmed = text.trim();
    let Some(body) = trimmed.strip_prefix('/') else {
        return ShellInput::NaturalLanguage(trimmed.to_string());
    };
    let body = body.trim();

    if body == "help" || body == "?" {
        return ShellInput::Help(None);
    }
    if let Some(topic) = body.strip_prefix("help ") {
        let topic = topic.trim();
        return ShellInput::Help((!topic.is_empty()).then(|| topic.to_string()));
    }
    if body == "model" {
        return ShellInput::ModelStatus;
    }
    if let Some(name) = body.strip_prefix("model ") {
        let name = name.trim();
        if !name.is_empty() {
            return ShellInput::ModelSwitch(name.to_string());
        }
    }
    if body == "ai" {
        return ShellInput::AssistStatus;
    }
    if body == "ai on" {
        return ShellInput::SetAssist(true);
    }
    if body == "ai off" {
        return ShellInput::SetAssist(false);
    }
    if let Some(name) = body.strip_prefix("ai model ") {
        let name = name.trim();
        if !name.is_empty() {
            return ShellInput::ModelSwitch(name.to_string());
        }
    }
    if let Some(rest) = body.strip_prefix("save ") {
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
        // unrecognized `/`-command gets.
    }
    if body == "record" {
        return ShellInput::RecordingStatus;
    }
    if body == "record on" {
        return ShellInput::SetRecording {
            on: true,
            path: None,
        };
    }
    if let Some(path) = body.strip_prefix("record on ") {
        let path = path.trim();
        return ShellInput::SetRecording {
            on: true,
            path: (!path.is_empty()).then(|| path.to_string()),
        };
    }
    if body == "record off" {
        return ShellInput::SetRecording {
            on: false,
            path: None,
        };
    }

    match dash9_core::parse(body) {
        Ok(cmd) => ShellInput::Grammar(cmd),
        Err(err) => ShellInput::CommandError(err),
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

/// Lines scrolled per `PageUp`/`PageDown` press. `ShellState` has no
/// notion of terminal size (deliberately — it stays testable without
/// a real terminal), so this is a fixed step rather than "one
/// screen's worth"; `command_bar::draw_log`'s `visible_window` clamps
/// the resulting offset against whatever area it's actually given.
const LOG_SCROLL_STEP: usize = 8;

/// Pure shell state: the command-bar buffer, the session log, pending
/// AI proposals awaiting `y`/`n`, and which panel has focus. No
/// terminal, filesystem, or network access anywhere in this type.
#[derive(Debug, Default)]
pub struct ShellState {
    pub input: Option<String>,
    pub log: Vec<LogLine>,
    pub pending_proposals: VecDeque<Command>,
    pub focused_panel: usize,
    /// Lines scrolled up from the newest log line (0 = pinned to the
    /// tail). Reset to 0 on every new submission — actively typing a
    /// new command means "I want to see what happens," not "keep me
    /// where I was"; background results (pollers, assistant replies)
    /// never reset it, so reading old output isn't interrupted.
    pub log_scroll: usize,
    /// `i`-toggled full-screen detail overlay for whichever panel is
    /// currently focused. Always renders `focused_panel`'s live
    /// state, so Tab-ing to a different panel while it's open just
    /// follows — no separate "which panel" tracking needed.
    pub detail_view: bool,
    history: VecDeque<String>,
    history_cursor: Option<usize>,
}

impl ShellState {
    /// Applies one keyboard event. Returns `true` only when the
    /// session should end — `Esc` never does, matching `LoreMesh`'s
    /// explicit invariant ("Escape never terminates... even when
    /// pressed repeatedly"); only `q` outside input, `/quit`, or
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
        // Scrolling works the same whether or not the command box is
        // being edited — reading old output while composing a new
        // command is a normal thing to want to do.
        match key.code {
            KeyCode::PageUp => {
                self.log_scroll = self.log_scroll.saturating_add(LOG_SCROLL_STEP);
                return false;
            }
            KeyCode::PageDown => {
                self.log_scroll = self.log_scroll.saturating_sub(LOG_SCROLL_STEP);
                return false;
            }
            _ => {}
        }

        if self.input.is_some() {
            match key.code {
                KeyCode::Esc => {
                    self.input = None;
                    self.history_cursor = None;
                }
                KeyCode::Enter => {
                    let text = self.input.take().unwrap_or_default();
                    self.history_cursor = None;
                    if !text.trim().is_empty() {
                        return self.submit(&text, handler);
                    }
                }
                KeyCode::Backspace => {
                    if let Some(buffer) = self.input.as_mut() {
                        buffer.pop();
                    }
                }
                KeyCode::Up => self.history_previous(),
                KeyCode::Down => self.history_next(),
                // Deliberately not `advance_focus` here — Tab reaches
                // the command box from *outside* it (see the
                // non-editing branch below), but once you're actively
                // composing text, Tab must not navigate away and
                // discard it out from under you. Only Esc (cancel) or
                // Enter (submit) leave the box while editing.
                KeyCode::Char(c) => {
                    if let Some(buffer) = self.input.as_mut() {
                        buffer.push(c);
                    }
                }
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
            KeyCode::Char('i') => self.detail_view = !self.detail_view,
            KeyCode::Esc if self.detail_view => self.detail_view = false,
            KeyCode::Tab => self.advance_focus(handler, true),
            KeyCode::BackTab => self.advance_focus(handler, false),
            _ => {}
        }
        false
    }

    /// Moves focus one step around the `panel_count() + 1`-stop ring
    /// (every panel, then the command box, wrapping). Only reachable
    /// while *not* editing (see `handle_key` — Tab/BackTab are a
    /// no-op while `input.is_some()`), so entering the command box
    /// here always starts a fresh empty buffer; there's never an
    /// in-progress one to discard.
    fn advance_focus<H: CommandHandler>(&mut self, handler: &H, forward: bool) {
        let panel_count = handler.panel_count();
        let total = panel_count + 1;
        let current = self.focused_panel.min(panel_count.saturating_sub(1));
        let next = if forward {
            (current + 1) % total
        } else {
            (current + total - 1) % total
        };
        if next == panel_count {
            self.input = Some(String::new());
        } else {
            self.input = None;
            self.focused_panel = next;
        }
        self.history_cursor = None;
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
        self.log_scroll = 0;
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
    use dash9_core::{ErrorCode, PanelType, RefreshInterval};

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
    fn tab_cycles_every_panel_then_reaches_the_command_box_and_stays_put() {
        let mut state = ShellState::default();
        let mut handler = MockHandler::new(3);

        state.handle_key(press(KeyCode::Tab), &mut handler);
        assert_eq!(state.focused_panel, 1);
        assert!(state.input.is_none());

        state.handle_key(press(KeyCode::Tab), &mut handler);
        assert_eq!(state.focused_panel, 2);

        state.handle_key(press(KeyCode::Tab), &mut handler);
        assert!(
            state.input.is_some(),
            "the 4th Tab (past the last of 3 panels) reaches the command box"
        );

        // Further Tabs, now that editing has started, must not
        // navigate away — only Esc/Enter leave the box once inside.
        state.handle_key(press(KeyCode::Tab), &mut handler);
        assert!(state.input.is_some(), "Tab stays put while editing");
    }

    #[test]
    fn backtab_from_the_first_panel_reaches_the_command_box_backward() {
        let mut state = ShellState::default();
        let mut handler = MockHandler::new(3);
        assert_eq!(state.focused_panel, 0);

        state.handle_key(press(KeyCode::BackTab), &mut handler);
        assert!(state.input.is_some());
    }

    #[test]
    fn tab_and_backtab_while_editing_are_a_no_op_and_never_lose_the_buffer() {
        let mut state = ShellState::default();
        let mut handler = MockHandler::new(3);
        state.handle_key(char_key(':'), &mut handler);
        state.handle_key(char_key('x'), &mut handler);
        assert_eq!(state.input.as_deref(), Some("x"));

        state.handle_key(press(KeyCode::Tab), &mut handler);
        assert_eq!(
            state.input.as_deref(),
            Some("x"),
            "Tab must not navigate away and drop what was typed"
        );

        state.handle_key(press(KeyCode::BackTab), &mut handler);
        assert_eq!(state.input.as_deref(), Some("x"));
    }

    #[test]
    fn page_up_and_page_down_adjust_log_scroll_and_never_quit() {
        let mut state = ShellState::default();
        let mut handler = MockHandler::new(1);
        assert_eq!(state.log_scroll, 0);

        assert!(!state.handle_key(press(KeyCode::PageUp), &mut handler));
        assert_eq!(state.log_scroll, LOG_SCROLL_STEP);
        state.handle_key(press(KeyCode::PageUp), &mut handler);
        assert_eq!(state.log_scroll, LOG_SCROLL_STEP * 2);

        assert!(!state.handle_key(press(KeyCode::PageDown), &mut handler));
        assert_eq!(state.log_scroll, LOG_SCROLL_STEP);
    }

    #[test]
    fn i_toggles_the_detail_view_outside_input_but_types_a_literal_i_while_editing() {
        let mut state = ShellState::default();
        let mut handler = MockHandler::new(1);
        assert!(!state.detail_view);

        state.handle_key(char_key('i'), &mut handler);
        assert!(state.detail_view);
        state.handle_key(char_key('i'), &mut handler);
        assert!(!state.detail_view);

        state.handle_key(char_key(':'), &mut handler);
        state.handle_key(char_key('i'), &mut handler);
        assert!(
            !state.detail_view,
            "typing 'i' in the buffer must not toggle it"
        );
        assert_eq!(state.input.as_deref(), Some("i"));
    }

    #[test]
    fn esc_closes_the_detail_view_when_open_and_not_editing() {
        let mut state = ShellState::default();
        let mut handler = MockHandler::new(1);
        state.detail_view = true;

        assert!(!state.handle_key(press(KeyCode::Esc), &mut handler));
        assert!(
            !state.detail_view,
            "Esc closes the detail view, doesn't quit"
        );
    }

    #[test]
    fn esc_while_editing_cancels_input_first_even_if_detail_view_is_open() {
        let mut state = ShellState::default();
        let mut handler = MockHandler::new(1);
        state.detail_view = true;
        state.handle_key(char_key(':'), &mut handler);
        state.handle_key(char_key('x'), &mut handler);

        state.handle_key(press(KeyCode::Esc), &mut handler);
        assert!(state.input.is_none(), "input cancelled first");
        assert!(
            state.detail_view,
            "detail view stays open — a second Esc closes it"
        );
    }

    #[test]
    fn page_down_past_zero_saturates_instead_of_underflowing() {
        let mut state = ShellState::default();
        let mut handler = MockHandler::new(1);
        state.handle_key(press(KeyCode::PageDown), &mut handler);
        assert_eq!(state.log_scroll, 0);
    }

    #[test]
    fn scrolling_works_while_editing_without_touching_the_buffer() {
        let mut state = ShellState::default();
        let mut handler = MockHandler::new(1);
        state.handle_key(char_key(':'), &mut handler);
        state.handle_key(char_key('x'), &mut handler);

        state.handle_key(press(KeyCode::PageUp), &mut handler);
        assert_eq!(state.log_scroll, LOG_SCROLL_STEP);
        assert_eq!(
            state.input.as_deref(),
            Some("x"),
            "scrolling must not disturb the in-progress buffer"
        );
    }

    #[test]
    fn submitting_a_command_resets_log_scroll_to_the_tail() {
        let mut state = ShellState::default();
        let mut handler = MockHandler::new(1);
        state.handle_key(press(KeyCode::PageUp), &mut handler);
        assert_eq!(state.log_scroll, LOG_SCROLL_STEP);

        type_and_submit(&mut state, &mut handler, "hello");
        assert_eq!(state.log_scroll, 0);
    }

    #[test]
    fn submitting_grammar_text_logs_command_then_result_and_calls_handler() {
        let mut state = ShellState::default();
        let mut handler = MockHandler::new(1);
        handler.next_response = Some(CommandResponse::result("range set to 5m"));

        let quit = type_and_submit(&mut state, &mut handler, "/range 5m");
        assert!(!quit);
        assert_eq!(handler.calls.len(), 1);
        assert!(matches!(
            handler.calls[0],
            ShellInput::Grammar(Command::Range { .. })
        ));
        assert_eq!(state.log.len(), 2, "one Command line, one Result line");
    }

    #[test]
    fn text_without_a_leading_slash_is_always_natural_language() {
        let mut state = ShellState::default();
        let mut handler = MockHandler::new(1);
        type_and_submit(&mut state, &mut handler, "please rename this");
        assert_eq!(
            handler.calls[0],
            ShellInput::NaturalLanguage("please rename this".to_string())
        );
    }

    #[test]
    fn bare_text_that_looks_like_grammar_is_still_natural_language() {
        // The discriminator is the leading `/`, not parseability —
        // "range 5m" is valid grammar, but with no `/` it must still
        // be routed as natural language, never silently executed.
        let mut state = ShellState::default();
        let mut handler = MockHandler::new(1);
        type_and_submit(&mut state, &mut handler, "range 5m");
        assert_eq!(
            handler.calls[0],
            ShellInput::NaturalLanguage("range 5m".to_string())
        );
    }

    #[test]
    fn slash_prefixed_unknown_command_is_a_command_error() {
        let mut state = ShellState::default();
        let mut handler = MockHandler::new(1);
        type_and_submit(&mut state, &mut handler, "/bogus");
        match &handler.calls[0] {
            ShellInput::CommandError(err) => assert_eq!(err.code, ErrorCode::E002),
            other => panic!("expected CommandError, got {other:?}"),
        }
    }

    #[test]
    fn help_and_ai_meta_commands_are_recognized_before_grammar_parsing() {
        assert_eq!(parse_shell_input("/help"), ShellInput::Help(None));
        assert_eq!(parse_shell_input("/?"), ShellInput::Help(None));
        assert_eq!(
            parse_shell_input("/help ds"),
            ShellInput::Help(Some("ds".to_string()))
        );
        assert_eq!(parse_shell_input("/model"), ShellInput::ModelStatus);
        assert_eq!(
            parse_shell_input("/model gemini-flash"),
            ShellInput::ModelSwitch("gemini-flash".to_string())
        );
        assert_eq!(parse_shell_input("/ai"), ShellInput::AssistStatus);
        assert_eq!(parse_shell_input("/ai on"), ShellInput::SetAssist(true));
        assert_eq!(parse_shell_input("/ai off"), ShellInput::SetAssist(false));
        assert_eq!(
            parse_shell_input("/ai model gemini-flash"),
            ShellInput::ModelSwitch("gemini-flash".to_string())
        );
    }

    #[test]
    fn record_meta_commands_are_recognized_before_grammar_parsing() {
        assert_eq!(parse_shell_input("/record"), ShellInput::RecordingStatus);
        assert_eq!(
            parse_shell_input("/record on"),
            ShellInput::SetRecording {
                on: true,
                path: None
            }
        );
        assert_eq!(
            parse_shell_input("/record on exports/session.jsonl"),
            ShellInput::SetRecording {
                on: true,
                path: Some("exports/session.jsonl".to_string())
            }
        );
        assert_eq!(
            parse_shell_input("/record off"),
            ShellInput::SetRecording {
                on: false,
                path: None
            }
        );
    }

    #[test]
    fn save_meta_command_parses_format_and_optional_path() {
        assert_eq!(
            parse_shell_input("/save csv"),
            ShellInput::Export {
                format: ExportFormat::Csv,
                path: None,
            }
        );
        assert_eq!(
            parse_shell_input("/save md exports/out.md"),
            ShellInput::Export {
                format: ExportFormat::Markdown,
                path: Some("exports/out.md".to_string()),
            }
        );
    }

    #[test]
    fn save_with_unknown_format_falls_through_to_a_command_error() {
        match parse_shell_input("/save pdf out.pdf") {
            ShellInput::CommandError(_) => {}
            other => panic!("expected a CommandError, got {other:?}"),
        }
    }

    #[test]
    fn help_overview_lists_every_top_level_group() {
        let text = help_text(None);
        for (name, _) in GROUP_BLURBS {
            assert!(text.contains(name), "missing group {name:?} in {text}");
        }
    }

    #[test]
    fn help_topic_group_lists_every_member_verb() {
        let text = help_text(Some("ds"));
        assert!(text.contains("/ds add"));
        assert!(text.contains("/ds list"));
    }

    #[test]
    fn help_topic_exact_multiword_verb_shows_one_entry() {
        let text = help_text(Some("ds add"));
        assert!(text.contains("/ds add"));
        assert!(!text.contains("/ds list"));
    }

    #[test]
    fn help_topic_unknown_reports_unknown() {
        let text = help_text(Some("bogus"));
        assert!(text.contains("unknown help topic"));
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
        type_and_submit(&mut state, &mut handler, "/range 1h");
        type_and_submit(&mut state, &mut handler, "/range 2h");

        state.handle_key(char_key(':'), &mut handler);
        state.handle_key(press(KeyCode::Up), &mut handler);
        assert_eq!(
            state.input.as_deref(),
            Some("/range 2h"),
            "most recent first"
        );
        state.handle_key(press(KeyCode::Up), &mut handler);
        assert_eq!(state.input.as_deref(), Some("/range 1h"));
        state.handle_key(press(KeyCode::Up), &mut handler);
        assert_eq!(state.input.as_deref(), Some("/range 1h"), "stops at oldest");
        state.handle_key(press(KeyCode::Down), &mut handler);
        assert_eq!(state.input.as_deref(), Some("/range 2h"));
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
