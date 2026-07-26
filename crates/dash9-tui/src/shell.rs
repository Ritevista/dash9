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
//! retried as natural language. A line starting with `!` always runs
//! through the user's shell (`docs/specs/open.md` Section K), unrelated
//! to SPEC.md's grammar. A line with **no** leading `/` or `!` is
//! always natural language, even if it happens to look like valid
//! grammar (`dash9_core::parse` is never called on it).
//!
//! Focus is one of four **regions** ([`Region`] — `Main` the panel
//! grid/layout/focus area, `Output`, `Log` — plus editing the command
//! box, `input.is_some()`, layered on top rather than a fifth `Region`
//! variant since it's orthogonal, checked first, everywhere). `Tab`/
//! `Shift+Tab` cycle forward/backward through all four, a fixed 4-stop
//! ring regardless of panel count — Main is *one* stop, not one per
//! panel, so a large dashboard doesn't need N `Tab` presses just to
//! reach Output/Log/the command box. `1`-`9` (panel selection) only do
//! anything when `region == Region::Main` — elsewhere they're inert,
//! since "which panel" is meaningless outside the grid. The command
//! box, once reached, doesn't lose focus on its own: `Enter` submits
//! and immediately reopens an empty buffer (seamless back-to-back
//! commands) — only `Tab`/`Shift+Tab` (deliberate navigation) or `Esc`
//! (deliberate cancel) actually leave it. While editing, `Tab`/
//! `Shift+Tab` still cycle which panel is focused underneath without
//! leaving the box or touching the buffer, so you can glance at
//! another panel's chart mid-command without losing what you've typed
//! — this one is unaffected by the region ring above, it's a separate,
//! narrower `panel_count()`-stop cycle over panels only.

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
    /// A line starting with `!` — everything after it is run through
    /// the user's shell (`docs/specs/open.md` Section K), unrelated to
    /// SPEC.md's command grammar. Checked before the `/` discriminator
    /// (`parse_shell_input`), so `!`/`/`/plain-text are three mutually
    /// exclusive leading-character cases, not a fallback chain.
    Shell(String),
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
         (needs --assist and AI on). Prefix with ! to run a shell command \
         (e.g. !ls) — output appears in the output pane.\n",
    );
    out.push_str(
        "Keys: Tab/Shift+Tab cycle Main (the panel grid), Output, Log, \
         and the command box, one Tab per region regardless of panel \
         count · 1-9 select a panel, only while Main has focus · : jump \
         to command box (Enter submits and stays put — Esc is what \
         leaves it) · +/- zoom Layout/Grid/Focus (an enlarged single \
         chart) · Space toggle the focused panel's detail pane below \
         the main area · Esc closes detail, then goes home to Grid · \
         PageUp/PageDown page panels (Main + Grid zoom), scroll Output \
         or Log (whichever has focus), or scroll the log (editing) · \
         y/n confirm proposal · a toggle AI · q or Ctrl+C quit\n",
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

/// The active zoom level's own one-line key hint
/// (`docs/specs/session-layout.md` Section D — "per-pane shortcut hints,"
/// the complement to `/help`'s full reference, not a replacement for it).
/// Pure text, no I/O, same category as [`help_text`]. Doesn't include the
/// Grid "panels X-Y of Z" paging indicator — that needs real viewport/rect
/// data the render layer has and this module doesn't.
/// Region-level navigation only — `i` (open/close the focused panel's
/// detail pane) is deliberately not repeated here: every panel already
/// shows it on its own border when focused
/// (`dash9_tui::draw::PANEL_HINT`, `docs/specs/open.md` Section G.2),
/// and it means the same thing regardless of zoom, so restating it in
/// every arm here would just be the same text twice on screen at once.
pub fn zoom_hint(zoom: Zoom) -> &'static str {
    match zoom {
        Zoom::Grid => "PageUp/PageDown page panels · 1-9 select · +/- zoom",
        Zoom::Layout => "1-9 select · + back to grid",
        Zoom::Focus => "1-9 select · Esc/- back to grid",
    }
}

/// Parses one submitted line. Three mutually exclusive cases, decided
/// by the first character alone, checked in this order: a leading `!`
/// is always [`ShellInput::Shell`] (`docs/specs/open.md` Section K —
/// the rest of the line runs through the user's shell, never
/// `dash9_core::parse`); a leading `/` is a structured command —
/// everything after it must match a shell meta-verb or
/// `dash9_core::parse`, or the result is [`ShellInput::CommandError`];
/// anything else is always [`ShellInput::NaturalLanguage`],
/// unconditionally — the text is never handed to `dash9_core::parse`
/// at all.
pub fn parse_shell_input(text: &str) -> ShellInput {
    let trimmed = text.trim();
    if let Some(command) = trimmed.strip_prefix('!') {
        return ShellInput::Shell(command.trim().to_string());
    }
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

/// Same step size for paging the Grid viewport (`docs/specs/session-layout.md`
/// Section A.2/C) — in the same content row-units `layout.rs` already uses
/// for `ROW_UNIT_HEIGHT`-scaled panel positions. `layout.rs`'s scroll-clamp
/// helpers self-clamp the resulting offset against whatever viewport height
/// the render layer actually has, same relationship `LOG_SCROLL_STEP` has
/// with `visible_window`.
const GRID_SCROLL_STEP: u16 = 6;

/// The three zoom levels (`docs/specs/session-layout.md` Section A): one
/// line, Layout ↔ Grid ↔ Focus. `Grid` is the fixed "home" level — today's
/// only level, kept as the default so a fresh session behaves exactly as
/// it did before this type existed. `Focus` is a single panel's chart,
/// enlarged, full-pane — nothing more; the config+data detail overlay is
/// a separate concern (`ShellState::detail_open`, below), not a zoom
/// level, since replacing the whole main area to show it made it
/// impossible to see any chart while inspecting one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Zoom {
    Layout,
    #[default]
    Grid,
    Focus,
}

impl Zoom {
    /// `+`/`=`: one step toward Focus. No-op at Focus (already innermost).
    fn zoom_in(self) -> Self {
        match self {
            Self::Layout => Self::Grid,
            Self::Grid => Self::Focus,
            Self::Focus => self,
        }
    }

    /// `-`/`_`: one step toward Layout. No-op at Layout (already outermost).
    fn zoom_out(self) -> Self {
        match self {
            Self::Focus => Self::Grid,
            Self::Grid => Self::Layout,
            Self::Layout => self,
        }
    }

    /// `Esc` (not editing, and only once nothing else has claimed it —
    /// see `handle_key`): "go home" — one hop straight to Grid from
    /// anywhere, unlike `zoom_out`'s one-step walk. No-op at Grid.
    fn zoom_home(self) -> Self {
        match self {
            Self::Focus | Self::Layout => Self::Grid,
            Self::Grid => self,
        }
    }
}

/// Which of the three non-editing regions currently has `Tab`-focus
/// (`docs/specs/open.md` Section E) — `Main` is the panel grid/layout/
/// focus area as *one* stop, not one per panel, so a large dashboard
/// doesn't cost N `Tab` presses to get past. Editing the command box is
/// deliberately not a fourth variant here — it's orthogonal state
/// (`ShellState.input.is_some()`), checked first everywhere a region
/// would otherwise matter (`handle_key`'s `PageUp`/`PageDown` dispatch,
/// digit-key gating). Entering it via `:` never touches `region` — a
/// quick interrupt returns to wherever you were. Entering it via `Tab`
/// (`advance_focus`) *does* set `region` to `Main` on the way in — not
/// left alone the way `:` leaves it, on purpose: an earlier version left
/// it untouched here too, which meant `Esc` bounced straight back to
/// whichever region you `Tab`-ed in from (typically `Log`, the stop
/// right before the command box), and the very next `Tab` just
/// re-entered the box — an inescapable two-region loop, confirmed live,
/// never reaching `Main`/`Output`. Setting `region` to `Main` (the
/// stop right *after* the command box) when `Tab` lands there means
/// `Esc` resumes the ring's forward progress instead of dead-ending.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Region {
    #[default]
    Main,
    Output,
    Log,
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
    /// Lines scrolled up from the newest log line (0 = pinned to the
    /// tail). Reset to 0 on every new submission — actively typing a
    /// new command means "I want to see what happens," not "keep me
    /// where I was"; background results (pollers, assistant replies)
    /// never reset it, so reading old output isn't interrupted.
    pub log_scroll: usize,
    /// Which of the three zoom levels (`docs/specs/session-layout.md`
    /// Section A) the main area is currently showing. Always renders
    /// `focused_panel`'s live state in `Focus`, so Tab-ing to a different
    /// panel while zoomed in just follows — no separate "which panel"
    /// tracking needed.
    pub zoom: Zoom,
    /// Space-toggled config+data overlay (`PanelDetail`/`draw_panel_detail`)
    /// for whichever panel is currently focused — rendered in its own pane
    /// **below** the main area (grid/layout/focus), never in place of it,
    /// so the chart(s) stay visible the whole time you're inspecting one.
    /// Independent of `zoom`: open in any of the three levels. Always
    /// renders `focused_panel`'s live state, so Tab-ing or a `1`-`9` jump
    /// while it's open just follows the newly focused panel.
    pub detail_open: bool,
    /// Grid viewport scroll, in the same content row-units `layout.rs`
    /// positions panels with (0 = top). Only ever changed by `PageUp`/
    /// `PageDown` while `zoom == Zoom::Grid` and not editing (Section C) —
    /// `ShellState` has no notion of terminal size, so this is the user's
    /// last explicit paging request, not a final render offset; the render
    /// layer additionally nudges it to keep the focused panel visible on
    /// `Tab` (Section B) without writing that nudge back here.
    pub grid_scroll: u16,
    /// Which region has `Tab`-focus when not editing (`Region`'s own
    /// docs). Independent of `zoom` the same way `detail_open` is: none
    /// of `Main`/`Output`/`Log` ever changes which zoom level is active.
    pub region: Region,
    /// Lines scrolled down from the top of the output pane's current
    /// text (0 = top) — the opposite anchor from `log_scroll` (which is
    /// tail-anchored) since the output pane shows one long block of text
    /// meant to be read top-to-bottom (a `/help` listing, a long `!`
    /// command's output), not a running history read newest-first. Reset
    /// to 0 whenever a new `Result` line arrives (`apply_response`) —
    /// same reasoning `log_scroll`'s reset-on-submit has: a new result
    /// should be read from its start, not wherever a stale scroll
    /// position from the *previous* result happened to leave off.
    pub output_scroll: usize,
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
        // `PageUp`/`PageDown` are contextual to whichever region is active
        // (`docs/specs/session-layout.md` Section C), checked before the
        // editing/non-editing split below since "editing" is itself one of
        // the regions in that table (it always scrolls the log, unchanged
        // from before zoom levels existed — reading old output while
        // composing a new command is a normal thing to want to do).
        match key.code {
            KeyCode::PageUp | KeyCode::PageDown if self.input.is_some() => {
                self.scroll_log(key.code);
                return false;
            }
            // `Output`/`Log` region focus (`advance_focus`'s ring, `Region`'s
            // docs) outranks zoom-level paging the same way editing does
            // above — each is its own region, independent of which zoom
            // level the main area happens to be showing. `Log`'s scroll
            // reuses `scroll_log` — same tail-anchored pane, same behavior
            // whether you got there by editing or by `Tab`-focusing it.
            KeyCode::PageUp | KeyCode::PageDown if self.region == Region::Output => {
                self.scroll_output(key.code);
                return false;
            }
            KeyCode::PageUp | KeyCode::PageDown if self.region == Region::Log => {
                self.scroll_log(key.code);
                return false;
            }
            KeyCode::PageUp | KeyCode::PageDown
                if self.region == Region::Main && self.zoom == Zoom::Grid =>
            {
                self.scroll_grid(key.code);
                return false;
            }
            // Layout: nothing to page, every panel is already visible.
            // Focus: reserved for v1 (scrolling one panel's own long
            // content) — not built now, deliberately a no-op rather than
            // falling back to scrolling the log out from under Focus.
            KeyCode::PageUp | KeyCode::PageDown => return false,
            _ => {}
        }

        if self.input.is_some() {
            match key.code {
                KeyCode::Esc => {
                    self.input = None;
                    self.history_cursor = None;
                }
                // Deliberately never leaves the command box — only `Esc`
                // (cancel) or `Tab`/`Shift+Tab` (navigate away) do (module
                // docs). A non-empty submission reopens a fresh empty
                // buffer right after, so a second command can be typed
                // immediately without re-pressing `:` or `Tab`-ing back;
                // an empty/whitespace-only line is already an implicit
                // no-op (nothing submitted) and just gets the same fresh
                // buffer.
                KeyCode::Enter => {
                    let text = self.input.take().unwrap_or_default();
                    self.history_cursor = None;
                    if !text.trim().is_empty() && self.submit(&text, handler) {
                        return true;
                    }
                    self.input = Some(String::new());
                }
                KeyCode::Backspace => {
                    if let Some(buffer) = self.input.as_mut() {
                        buffer.pop();
                    }
                }
                KeyCode::Up => self.history_previous(),
                KeyCode::Down => self.history_next(),
                // Cycles which panel is focused without leaving the
                // command box or touching the buffer — you can look at a
                // different panel's chart mid-command (e.g. to check a
                // value before finishing `/panel threshold ...`) without
                // losing what you've typed. Deliberately *not*
                // `advance_focus` (the non-editing version below): that
                // one also reaches the command box itself as a ring stop,
                // which makes no sense when we're already in it — this is
                // a plain `panel_count()`-stop ring over panels only, and
                // never touches `self.input`, so it can never discard it.
                KeyCode::Tab => self.cycle_focused_panel(handler, true),
                KeyCode::BackTab => self.cycle_focused_panel(handler, false),
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
            // Entering the command box never touches `region` — whichever
            // of Main/Output/Log was current before `:` is still current
            // once you leave it (`Region`'s docs), so nothing needs
            // resetting here.
            KeyCode::Char(':') => self.input = Some(String::new()),
            KeyCode::Char(' ') => self.detail_open = !self.detail_open,
            KeyCode::Char('+' | '=') => self.zoom = self.zoom.zoom_in(),
            KeyCode::Char('-' | '_') => self.zoom = self.zoom.zoom_out(),
            // Layered, same shape `Esc` already had before zoom levels
            // existed (cancel input, *then* close detail): close the
            // detail pane first if it's open; only once it's closed (or
            // was never open) does Esc fall through to "go home" for
            // zoom. One press always does at most one thing.
            KeyCode::Esc if self.detail_open => self.detail_open = false,
            KeyCode::Esc => self.zoom = self.zoom.zoom_home(),
            KeyCode::Tab => self.advance_focus(true),
            KeyCode::BackTab => self.advance_focus(false),
            // Only meaningful within the Main region — "which panel" has
            // no meaning while Output or Log has focus, so a digit is
            // inert there instead of silently doing something to a panel
            // you can't currently see highlighted anywhere.
            KeyCode::Char(c @ '1'..='9') if self.region == Region::Main => {
                self.focus_panel_by_number(c, handler);
            }
            _ => {}
        }
        false
    }

    /// `1`-`9`: jump focus straight to that panel (1-indexed, matching how
    /// panels are announced/counted elsewhere — e.g. the zoom bar's
    /// "panels X-Y of Z"), instead of stepping through `Tab`'s cycle one
    /// panel at a time. A digit past `panel_count()` (including on an
    /// empty dashboard) is a no-op rather than clamping to the last panel
    /// — silently landing on the wrong panel would be more surprising
    /// than nothing happening. Works in every zoom level, same as `Tab`
    /// (`advance_focus` itself is never zoom-gated). Only reachable when
    /// `region == Region::Main` already (the `handle_key` guard), so
    /// there's nothing to reset here — `region` is already right.
    fn focus_panel_by_number<H: CommandHandler>(&mut self, digit: char, handler: &H) {
        let index = digit
            .to_digit(10)
            .expect("checked by the '1'..='9' pattern") as usize
            - 1;
        if index < handler.panel_count() {
            self.focused_panel = index;
        }
    }

    /// Moves focus one step around the fixed 4-stop ring: `Main`,
    /// `Output`, `Log`, then the command box, wrapping — regardless of
    /// panel count, since `Main` is one stop, not one per panel (see
    /// `Region`'s docs for why). Only called from the non-editing branch
    /// of `handle_key` — while editing, `cycle_focused_panel` (below) is
    /// the one Tab/BackTab reach instead — so entering the command box
    /// here always starts a fresh empty buffer; there's never an
    /// in-progress one to discard.
    fn advance_focus(&mut self, forward: bool) {
        const RING: [Region; 3] = [Region::Main, Region::Output, Region::Log];
        const COMMAND_BOX_STOP: usize = RING.len();
        const TOTAL: usize = RING.len() + 1;

        let current = RING
            .iter()
            .position(|r| *r == self.region)
            .expect("region is always one of RING's variants");
        let next = if forward {
            (current + 1) % TOTAL
        } else {
            (current + TOTAL - 1) % TOTAL
        };
        self.input = None;
        if next == COMMAND_BOX_STOP {
            self.input = Some(String::new());
            // Landing on the command box via the ring — unlike `:` (which
            // leaves `region` untouched, a quick interrupt that returns to
            // wherever you were), Tab is continuing the cycle, so `region`
            // advances too, straight to `Main` (`RING[0]`, the stop right
            // after Command going forward). Without this, `region` stayed
            // wherever it was before this Tab (e.g. `Log`), and since Tab
            // never leaves the box while editing (only Esc/Enter do —
            // `cycle_focused_panel` below), `Esc` would land right back on
            // that same stale region — one more Tab would just re-enter
            // the box. That's an inescapable Log<->Command loop, not a
            // ring: confirmed live, this was a real dead end, not a
            // hypothetical one.
            self.region = Region::Main;
        } else {
            self.region = RING[next];
        }
        self.history_cursor = None;
    }

    /// The editing-time counterpart to `advance_focus`: a plain
    /// `panel_count()`-stop ring over panels only (no command-box stop —
    /// we're already there), and never touches `self.input`/
    /// `self.history_cursor`. No-op on an empty dashboard.
    fn cycle_focused_panel<H: CommandHandler>(&mut self, handler: &H, forward: bool) {
        let panel_count = handler.panel_count();
        if panel_count == 0 {
            return;
        }
        let current = self.focused_panel.min(panel_count - 1);
        self.focused_panel = if forward {
            (current + 1) % panel_count
        } else {
            (current + panel_count - 1) % panel_count
        };
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

    fn scroll_log(&mut self, code: KeyCode) {
        match code {
            KeyCode::PageUp => self.log_scroll = self.log_scroll.saturating_add(LOG_SCROLL_STEP),
            KeyCode::PageDown => {
                self.log_scroll = self.log_scroll.saturating_sub(LOG_SCROLL_STEP);
            }
            _ => unreachable!("only called for PageUp/PageDown"),
        }
    }

    /// `PageDown` moves further into the content (later panel rows,
    /// growing `grid_scroll`) matching the "`PageDown` for more" affordance
    /// `docs/specs/session-layout.md` Section A.2 describes; `PageUp` walks
    /// back toward the top. This is the opposite direction from the log's
    /// own convention (`scroll_log`, where `PageUp` grows the offset) —
    /// the log is tail-anchored (0 = newest, at the bottom), the grid is
    /// top-anchored (0 = first panels, at the top), so "`PageDown` reveals
    /// more" means growing the offset in the log's case and the grid's
    /// case alike, which is a shrink for one and a grow for the other.
    fn scroll_grid(&mut self, code: KeyCode) {
        match code {
            KeyCode::PageDown => {
                self.grid_scroll = self.grid_scroll.saturating_add(GRID_SCROLL_STEP);
            }
            KeyCode::PageUp => {
                self.grid_scroll = self.grid_scroll.saturating_sub(GRID_SCROLL_STEP);
            }
            _ => unreachable!("only called for PageUp/PageDown"),
        }
    }

    /// Same top-anchored direction convention as `scroll_grid` (`PageDown`
    /// grows the offset, revealing more of the content below) rather than
    /// the log's tail-anchored one — the output pane shows one block of
    /// text meant to be read top-to-bottom, not a history read
    /// newest-first. Render-layer clamping (mirroring `grid_scroll`'s
    /// `.min(max_grid_scroll(..))` pattern) keeps this from scrolling past
    /// the actual content once a real terminal size is known.
    fn scroll_output(&mut self, code: KeyCode) {
        match code {
            KeyCode::PageDown => {
                self.output_scroll = self.output_scroll.saturating_add(LOG_SCROLL_STEP);
            }
            KeyCode::PageUp => {
                self.output_scroll = self.output_scroll.saturating_sub(LOG_SCROLL_STEP);
            }
            _ => unreachable!("only called for PageUp/PageDown"),
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
        if response
            .log_entries
            .iter()
            .any(|line| matches!(line, LogLine::Result(_)))
        {
            self.output_scroll = 0;
        }
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

    /// Only presses `:` if the box isn't already open — the command box
    /// stays open after a submit now (`docs/specs/open.md` Section E), so
    /// blindly pressing `:` on a second call would type a literal `:`
    /// into the still-open buffer instead of doing nothing new.
    fn type_and_submit(state: &mut ShellState, handler: &mut MockHandler, text: &str) -> bool {
        if state.input.is_none() {
            state.handle_key(char_key(':'), handler);
        }
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
    fn tab_cycles_main_output_log_then_reaches_the_command_box_regardless_of_panel_count() {
        // A dashboard with many panels must still take exactly one Tab to
        // leave Main — the whole point of the fixed 4-stop ring (Region's
        // docs) is that panel count never affects how many Tabs it takes
        // to reach Output/Log/the command box.
        let mut state = ShellState::default();
        let mut handler = MockHandler::new(50);

        assert_eq!(state.region, Region::Main);

        state.handle_key(press(KeyCode::Tab), &mut handler);
        assert_eq!(state.region, Region::Output, "one Tab leaves Main");
        assert!(state.input.is_none());

        state.handle_key(press(KeyCode::Tab), &mut handler);
        assert_eq!(state.region, Region::Log);

        state.handle_key(press(KeyCode::Tab), &mut handler);
        assert!(
            state.input.is_some(),
            "the 4th Tab (past Main, Output, Log) reaches the command box"
        );

        // Further Tabs, now that editing has started, must not leave
        // the command box — only Esc does that (or Enter submitting,
        // which reopens it immediately — see
        // `enter_never_leaves_the_command_box_even_after_a_real_submit`).
        // They still cycle which panel is focused underneath, though
        // (`tab_and_backtab_while_editing_cycle_panel_focus_without_losing_the_buffer`).
        state.handle_key(press(KeyCode::Tab), &mut handler);
        assert!(state.input.is_some(), "Tab does not leave the command box");
    }

    /// Regression test: reaching the command box via `Tab` and then
    /// cancelling used to return to whichever region was current *before*
    /// that `Tab` (e.g. `Log`) — but `Tab` never leaves the box while
    /// editing (only `Esc`/`Enter` do), so the very next `Tab` after that
    /// `Esc` just re-entered the box again. `Tab` → `Esc` → `Tab` → `Esc`
    /// looped between `Log` and the command box forever, never reaching
    /// `Main`/`Output` — confirmed live, a genuine dead end, not
    /// hypothetical. Fixed: landing on the command box *via the ring*
    /// advances `region` to `Main` (the next stop, continuing the cycle),
    /// so `Esc` from there resumes forward progress instead of bouncing
    /// back to a stop that only leads right back to the box.
    #[test]
    fn cancelling_out_of_a_tab_reached_command_box_continues_the_ring_to_main() {
        let mut state = ShellState::default();
        let mut handler = MockHandler::new(3);
        state.handle_key(press(KeyCode::Tab), &mut handler); // Main -> Output
        state.handle_key(press(KeyCode::Tab), &mut handler); // Output -> Log
        state.handle_key(press(KeyCode::Tab), &mut handler); // Log -> command box
        assert!(state.input.is_some());

        state.handle_key(press(KeyCode::Esc), &mut handler);
        assert!(state.input.is_none());
        assert_eq!(
            state.region,
            Region::Main,
            "Esc must continue the ring to Main, not bounce back to Log"
        );

        // And the ring keeps working from here — no dead end.
        state.handle_key(press(KeyCode::Tab), &mut handler);
        assert_eq!(state.region, Region::Output);
    }

    /// The other entry path (`:`, bypassing Tab entirely) is unaffected —
    /// it's a quick interrupt, not ring navigation, so cancelling out of
    /// it must still return to exactly where you were.
    #[test]
    fn cancelling_out_of_a_colon_reached_command_box_returns_to_the_same_region() {
        let mut state = ShellState::default();
        let mut handler = MockHandler::new(3);
        state.handle_key(press(KeyCode::Tab), &mut handler); // Main -> Output
        state.handle_key(press(KeyCode::Tab), &mut handler); // Output -> Log
        assert_eq!(state.region, Region::Log);

        state.handle_key(char_key(':'), &mut handler);
        assert!(state.input.is_some());
        state.handle_key(press(KeyCode::Esc), &mut handler);
        assert_eq!(
            state.region,
            Region::Log,
            "`:` doesn't advance the ring, so Esc returns to the same region"
        );
    }

    /// Deterministic end-to-end guard for the same bug: driving `Tab`
    /// (cancelling out with `Esc` whenever it lands on the command box)
    /// repeatedly must trace out full `Main -> Output -> Log -> Command`
    /// cycles indefinitely, never repeating the same two regions forever.
    #[test]
    fn repeated_tab_and_cancel_completes_full_cycles_without_getting_stuck() {
        let mut state = ShellState::default();
        let mut handler = MockHandler::new(1);
        let mut regions = Vec::new();
        for _ in 0..8 {
            state.handle_key(press(KeyCode::Tab), &mut handler);
            if state.input.is_some() {
                state.handle_key(press(KeyCode::Esc), &mut handler);
            }
            regions.push(state.region);
        }
        assert_eq!(
            regions,
            vec![
                Region::Output,
                Region::Log,
                Region::Main,
                Region::Output,
                Region::Log,
                Region::Main,
                Region::Output,
                Region::Log,
            ],
            "two full cycles, always making forward progress: {regions:?}"
        );
    }

    #[test]
    fn number_keys_jump_focus_straight_to_that_panel() {
        let mut state = ShellState::default();
        let mut handler = MockHandler::new(5);

        state.handle_key(char_key('3'), &mut handler);
        assert_eq!(
            state.focused_panel, 2,
            "'3' jumps to the 3rd (index 2) panel"
        );

        state.handle_key(char_key('1'), &mut handler);
        assert_eq!(state.focused_panel, 0);
    }

    #[test]
    fn number_key_past_panel_count_is_a_no_op() {
        let mut state = ShellState::default();
        let mut handler = MockHandler::new(3);
        state.focused_panel = 1;

        state.handle_key(char_key('9'), &mut handler);
        assert_eq!(state.focused_panel, 1, "no 9th panel — focus stays put");
    }

    #[test]
    fn number_key_on_an_empty_dashboard_is_a_no_op() {
        let mut state = ShellState::default();
        let mut handler = MockHandler::new(0);
        state.handle_key(char_key('1'), &mut handler);
        assert_eq!(state.focused_panel, 0);
    }

    #[test]
    fn number_keys_work_in_every_zoom_level() {
        for zoom in [Zoom::Layout, Zoom::Grid, Zoom::Focus] {
            let mut state = ShellState {
                zoom,
                ..ShellState::default()
            };
            let mut handler = MockHandler::new(4);
            state.handle_key(char_key('4'), &mut handler);
            assert_eq!(
                state.focused_panel, 3,
                "{zoom:?} should still honor 1-9 selection"
            );
        }
    }

    #[test]
    fn number_key_while_editing_is_a_literal_character_not_a_jump() {
        let mut state = ShellState::default();
        let mut handler = MockHandler::new(5);
        state.handle_key(char_key(':'), &mut handler);
        state.handle_key(char_key('3'), &mut handler);
        assert_eq!(
            state.focused_panel, 0,
            "digits while editing must not move focus"
        );
        assert_eq!(state.input.as_deref(), Some("3"));
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
    fn tab_and_backtab_while_editing_cycle_panel_focus_without_losing_the_buffer() {
        let mut state = ShellState::default();
        let mut handler = MockHandler::new(3);
        state.handle_key(char_key(':'), &mut handler);
        state.handle_key(char_key('x'), &mut handler);
        assert_eq!(state.input.as_deref(), Some("x"));
        assert_eq!(state.focused_panel, 0);

        state.handle_key(press(KeyCode::Tab), &mut handler);
        assert_eq!(
            state.input.as_deref(),
            Some("x"),
            "Tab must not discard what was typed"
        );
        assert_eq!(state.focused_panel, 1, "but it does move panel focus");

        state.handle_key(press(KeyCode::Tab), &mut handler);
        assert_eq!(state.focused_panel, 2);
        state.handle_key(press(KeyCode::Tab), &mut handler);
        assert_eq!(
            state.focused_panel, 0,
            "wraps — no command-box stop while already in it"
        );

        state.handle_key(press(KeyCode::BackTab), &mut handler);
        assert_eq!(state.focused_panel, 2, "BackTab wraps the other way");
        assert_eq!(state.input.as_deref(), Some("x"));
    }

    #[test]
    fn tab_while_editing_on_an_empty_dashboard_is_a_no_op() {
        let mut state = ShellState::default();
        let mut handler = MockHandler::new(0);
        state.handle_key(char_key(':'), &mut handler);
        state.handle_key(press(KeyCode::Tab), &mut handler);
        assert_eq!(state.focused_panel, 0);
    }

    #[test]
    fn tab_on_an_empty_dashboard_still_reaches_output_and_log_before_the_command_box() {
        // Main being empty of panels doesn't change the ring at all — it's
        // still exactly one Tab per region, panel count never enters into
        // it (`Region`'s docs).
        let mut state = ShellState::default();
        let mut handler = MockHandler::new(0);

        state.handle_key(press(KeyCode::Tab), &mut handler);
        assert_eq!(state.region, Region::Output);
        assert!(state.input.is_none());

        state.handle_key(press(KeyCode::Tab), &mut handler);
        assert_eq!(state.region, Region::Log);

        state.handle_key(press(KeyCode::Tab), &mut handler);
        assert!(state.input.is_some());
    }

    #[test]
    fn digit_keys_are_inert_outside_main_and_never_change_region() {
        // "Which panel" has no meaning while Output or Log has focus — a
        // digit press there must do nothing at all, not fall back to
        // jumping a panel and silently switching back to Main.
        let mut state = ShellState::default();
        let mut handler = MockHandler::new(5);

        state.handle_key(press(KeyCode::Tab), &mut handler);
        assert_eq!(state.region, Region::Output);

        state.handle_key(char_key('3'), &mut handler);
        assert_eq!(
            state.region,
            Region::Output,
            "digit press must not implicitly return focus to Main"
        );
        assert_eq!(
            state.focused_panel, 0,
            "and must not change which panel is remembered as focused"
        );

        state.handle_key(press(KeyCode::Tab), &mut handler);
        assert_eq!(state.region, Region::Log);
        state.handle_key(char_key('2'), &mut handler);
        assert_eq!(state.region, Region::Log, "same for Log");
        assert_eq!(state.focused_panel, 0);
    }

    #[test]
    fn digit_key_works_normally_once_back_on_main() {
        let mut state = ShellState::default();
        let mut handler = MockHandler::new(5);
        state.handle_key(press(KeyCode::Tab), &mut handler); // Main -> Output
        state.handle_key(press(KeyCode::Tab), &mut handler); // Output -> Log
        state.handle_key(press(KeyCode::Tab), &mut handler); // Log -> command box
        state.handle_key(press(KeyCode::Esc), &mut handler); // cancel — ring continues to Main

        assert_eq!(state.region, Region::Main);
        state.handle_key(char_key('3'), &mut handler);
        assert_eq!(state.focused_panel, 2, "digits work again once on Main");
    }

    #[test]
    fn output_region_is_reachable_via_tab() {
        let mut state = ShellState::default();
        let mut handler = MockHandler::new(1);
        state.handle_key(press(KeyCode::Tab), &mut handler);
        assert_eq!(state.region, Region::Output);
    }

    #[test]
    fn log_region_is_reachable_via_tab_and_scrolls_with_page_up_down() {
        let mut state = ShellState::default();
        let mut handler = MockHandler::new(1);
        state.handle_key(press(KeyCode::Tab), &mut handler); // Main -> Output
        state.handle_key(press(KeyCode::Tab), &mut handler); // Output -> Log
        assert_eq!(state.region, Region::Log);

        assert!(!state.handle_key(press(KeyCode::PageUp), &mut handler));
        assert_eq!(state.log_scroll, LOG_SCROLL_STEP);
        assert_eq!(
            state.grid_scroll, 0,
            "Log focus must not also page the grid"
        );
        assert_eq!(state.output_scroll, 0, "or scroll the output pane");
    }

    #[test]
    fn page_up_and_page_down_scroll_output_when_output_region_is_focused() {
        let mut state = ShellState {
            region: Region::Output,
            ..ShellState::default()
        };
        let mut handler = MockHandler::new(1);

        assert!(!state.handle_key(press(KeyCode::PageDown), &mut handler));
        assert_eq!(state.output_scroll, LOG_SCROLL_STEP);
        assert_eq!(
            state.grid_scroll, 0,
            "output focus must not also page the grid"
        );

        state.handle_key(press(KeyCode::PageUp), &mut handler);
        assert_eq!(state.output_scroll, 0);
    }

    #[test]
    fn output_scroll_saturates_instead_of_underflowing() {
        let mut state = ShellState {
            region: Region::Output,
            ..ShellState::default()
        };
        let mut handler = MockHandler::new(1);
        state.handle_key(press(KeyCode::PageUp), &mut handler);
        assert_eq!(state.output_scroll, 0);
    }

    #[test]
    fn a_new_result_resets_output_scroll_to_the_top() {
        let mut state = ShellState {
            output_scroll: 40,
            ..ShellState::default()
        };
        let mut handler = MockHandler::new(1);
        handler.next_response = Some(CommandResponse::result("fresh result"));
        type_and_submit(&mut state, &mut handler, "/range 5m");
        assert_eq!(state.output_scroll, 0);
    }

    #[test]
    fn editing_still_scrolls_the_log_even_if_output_region_was_focused_before_entering_it() {
        // `region` is untouched by entering the command box (`Region`'s
        // docs), so this guards the dispatch order directly: editing
        // (`input.is_some()`) must outrank `region` in the PageUp/PageDown
        // match, never misrouted to `scroll_output` just because `region`
        // still says `Output` underneath.
        let mut state = ShellState {
            region: Region::Output,
            ..ShellState::default()
        };
        let mut handler = MockHandler::new(1);
        state.handle_key(char_key(':'), &mut handler);
        state.handle_key(press(KeyCode::PageUp), &mut handler);
        assert_eq!(state.log_scroll, LOG_SCROLL_STEP);
        assert_eq!(state.output_scroll, 0);
    }

    #[test]
    fn page_up_and_page_down_while_editing_adjust_log_scroll_and_never_quit() {
        let mut state = ShellState::default();
        let mut handler = MockHandler::new(1);
        state.handle_key(char_key(':'), &mut handler);
        assert_eq!(state.log_scroll, 0);

        assert!(!state.handle_key(press(KeyCode::PageUp), &mut handler));
        assert_eq!(state.log_scroll, LOG_SCROLL_STEP);
        state.handle_key(press(KeyCode::PageUp), &mut handler);
        assert_eq!(state.log_scroll, LOG_SCROLL_STEP * 2);

        assert!(!state.handle_key(press(KeyCode::PageDown), &mut handler));
        assert_eq!(state.log_scroll, LOG_SCROLL_STEP);
    }

    #[test]
    fn page_up_and_page_down_in_grid_and_not_editing_adjust_grid_scroll_instead_of_log() {
        let mut state = ShellState::default();
        let mut handler = MockHandler::new(1);
        assert_eq!(state.zoom, Zoom::Grid, "default zoom is Grid");

        assert!(!state.handle_key(press(KeyCode::PageDown), &mut handler));
        assert_eq!(state.grid_scroll, GRID_SCROLL_STEP);
        assert_eq!(
            state.log_scroll, 0,
            "log is untouched outside the command box"
        );

        state.handle_key(press(KeyCode::PageUp), &mut handler);
        assert_eq!(state.grid_scroll, 0);
    }

    #[test]
    fn grid_scroll_saturates_instead_of_underflowing() {
        let mut state = ShellState::default();
        let mut handler = MockHandler::new(1);
        state.handle_key(press(KeyCode::PageUp), &mut handler);
        assert_eq!(state.grid_scroll, 0);
    }

    #[test]
    fn page_up_and_page_down_are_a_no_op_in_layout_and_focus() {
        for zoom in [Zoom::Layout, Zoom::Focus] {
            let mut state = ShellState {
                zoom,
                ..ShellState::default()
            };
            let mut handler = MockHandler::new(1);
            state.handle_key(press(KeyCode::PageDown), &mut handler);
            assert_eq!(state.grid_scroll, 0, "{zoom:?} must not page the grid");
            assert_eq!(
                state.log_scroll, 0,
                "{zoom:?} must not scroll the log either"
            );
        }
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
        let mut state = ShellState {
            log_scroll: LOG_SCROLL_STEP,
            ..ShellState::default()
        };
        let mut handler = MockHandler::new(1);

        type_and_submit(&mut state, &mut handler, "hello");
        assert_eq!(state.log_scroll, 0);
    }

    #[test]
    fn space_toggles_detail_open_and_types_a_literal_space_while_editing() {
        let mut state = ShellState::default();
        let mut handler = MockHandler::new(1);
        assert!(!state.detail_open);
        assert_eq!(
            state.zoom,
            Zoom::Grid,
            "opening detail must not change zoom — it renders below the main area, not in place of it"
        );

        state.handle_key(char_key(' '), &mut handler);
        assert!(state.detail_open);
        assert_eq!(state.zoom, Zoom::Grid, "zoom is untouched by Space");

        state.handle_key(char_key(':'), &mut handler);
        state.handle_key(char_key(' '), &mut handler);
        assert!(
            state.detail_open,
            "typing a space in the buffer must not toggle it"
        );
        assert_eq!(state.input.as_deref(), Some(" "));
    }

    /// Regression guard: `i` was the detail-toggle key before it was
    /// switched to Space (`docs/specs/open.md` Section E) — it must now
    /// be inert outside the command box, not silently still toggle
    /// detail alongside Space.
    #[test]
    fn i_no_longer_toggles_detail() {
        let mut state = ShellState::default();
        let mut handler = MockHandler::new(1);
        state.handle_key(char_key('i'), &mut handler);
        assert!(
            !state.detail_open,
            "i must be inert outside the command box now that Space is the detail-toggle key"
        );
    }

    #[test]
    fn detail_can_be_open_in_any_zoom_level() {
        for zoom in [Zoom::Layout, Zoom::Grid, Zoom::Focus] {
            let mut state = ShellState {
                zoom,
                ..ShellState::default()
            };
            let mut handler = MockHandler::new(1);
            state.handle_key(char_key(' '), &mut handler);
            assert!(
                state.detail_open,
                "{zoom:?} should still let Space open detail"
            );
            assert_eq!(state.zoom, zoom, "{zoom:?} zoom must not change from Space");
        }
    }

    #[test]
    fn plus_and_minus_step_one_level_along_layout_grid_focus() {
        let mut state = ShellState::default();
        let mut handler = MockHandler::new(1);
        assert_eq!(state.zoom, Zoom::Grid);

        state.handle_key(char_key('-'), &mut handler);
        assert_eq!(state.zoom, Zoom::Layout);
        state.handle_key(char_key('-'), &mut handler);
        assert_eq!(state.zoom, Zoom::Layout, "no-op at the outermost level");

        state.handle_key(char_key('+'), &mut handler);
        assert_eq!(state.zoom, Zoom::Grid, "+ from Layout lands on Grid");
        state.handle_key(char_key('+'), &mut handler);
        assert_eq!(state.zoom, Zoom::Focus, "+ from Grid enters Focus");
        state.handle_key(char_key('+'), &mut handler);
        assert_eq!(state.zoom, Zoom::Focus, "no-op at the innermost level");

        state.handle_key(char_key('-'), &mut handler);
        assert_eq!(state.zoom, Zoom::Grid, "- from Focus lands on Grid");
    }

    #[test]
    fn equals_and_underscore_are_aliases_for_plus_and_minus() {
        let mut state = ShellState::default();
        let mut handler = MockHandler::new(1);
        state.handle_key(char_key('='), &mut handler);
        assert_eq!(state.zoom, Zoom::Focus);
        state.handle_key(char_key('_'), &mut handler);
        assert_eq!(state.zoom, Zoom::Grid);
    }

    #[test]
    fn esc_goes_straight_home_to_grid_from_anywhere_and_never_quits() {
        for zoom in [Zoom::Layout, Zoom::Focus] {
            let mut state = ShellState {
                zoom,
                ..ShellState::default()
            };
            let mut handler = MockHandler::new(1);
            assert!(!state.handle_key(press(KeyCode::Esc), &mut handler));
            assert_eq!(
                state.zoom,
                Zoom::Grid,
                "Esc from {zoom:?} goes home to Grid"
            );
        }
    }

    #[test]
    fn esc_at_grid_is_a_no_op() {
        let mut state = ShellState::default();
        let mut handler = MockHandler::new(1);
        assert!(!state.handle_key(press(KeyCode::Esc), &mut handler));
        assert_eq!(state.zoom, Zoom::Grid);
    }

    #[test]
    fn esc_closes_detail_before_going_home_to_grid() {
        let mut state = ShellState {
            zoom: Zoom::Focus,
            detail_open: true,
            ..ShellState::default()
        };
        let mut handler = MockHandler::new(1);

        state.handle_key(press(KeyCode::Esc), &mut handler);
        assert!(!state.detail_open, "first Esc closes detail");
        assert_eq!(
            state.zoom,
            Zoom::Focus,
            "zoom stays put — a second Esc goes home"
        );

        state.handle_key(press(KeyCode::Esc), &mut handler);
        assert_eq!(state.zoom, Zoom::Grid, "second Esc goes home to Grid");
    }

    #[test]
    fn esc_while_editing_cancels_input_first_even_with_detail_open() {
        let mut state = ShellState {
            detail_open: true,
            ..ShellState::default()
        };
        let mut handler = MockHandler::new(1);
        state.handle_key(char_key(':'), &mut handler);
        state.handle_key(char_key('x'), &mut handler);

        state.handle_key(press(KeyCode::Esc), &mut handler);
        assert!(state.input.is_none(), "input cancelled first");
        assert!(
            state.detail_open,
            "detail stays open — a second Esc closes it"
        );
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

    /// Regression test: the command box used to lose focus on every
    /// submit — after `Enter`, `input` became `None`, so a second command
    /// needed either `:` again or Tab-cycling all the way back around the
    /// ring (`docs/specs/open.md` Section E). Fixed so `Enter` reopens an
    /// empty buffer immediately instead.
    #[test]
    fn enter_never_leaves_the_command_box_even_after_a_real_submit() {
        let mut state = ShellState::default();
        let mut handler = MockHandler::new(1);
        handler.next_response = Some(CommandResponse::result("range set to 5m"));

        type_and_submit(&mut state, &mut handler, "/range 5m");
        assert_eq!(
            state.input.as_deref(),
            Some(""),
            "Enter must reopen an empty buffer, not leave the command box"
        );
    }

    /// Same regression, exercised the way a user actually hits it: two
    /// commands typed back to back with only one `:` press, no Tab or
    /// re-pressing `:` in between.
    #[test]
    fn back_to_back_commands_work_without_re_pressing_colon_or_tab() {
        let mut state = ShellState::default();
        let mut handler = MockHandler::new(1);
        handler.next_response = Some(CommandResponse::result("range set to 5m"));

        state.handle_key(char_key(':'), &mut handler);
        for c in "/range 5m".chars() {
            state.handle_key(char_key(c), &mut handler);
        }
        state.handle_key(press(KeyCode::Enter), &mut handler);
        assert_eq!(handler.calls.len(), 1);

        handler.next_response = Some(CommandResponse::result("refresh set to 10s"));
        for c in "/refresh 10s".chars() {
            state.handle_key(char_key(c), &mut handler);
        }
        state.handle_key(press(KeyCode::Enter), &mut handler);
        assert_eq!(
            handler.calls.len(),
            2,
            "second command must reach the handler without any Tab/: in between"
        );
        assert!(matches!(
            handler.calls[1],
            ShellInput::Grammar(Command::Refresh { .. })
        ));
    }

    #[test]
    fn empty_enter_also_keeps_the_command_box_open() {
        let mut state = ShellState::default();
        let mut handler = MockHandler::new(1);
        state.handle_key(char_key(':'), &mut handler);
        state.handle_key(press(KeyCode::Enter), &mut handler);
        assert_eq!(state.input.as_deref(), Some(""));
    }

    #[test]
    fn esc_is_still_the_way_to_actually_leave_the_command_box() {
        let mut state = ShellState::default();
        let mut handler = MockHandler::new(1);
        state.handle_key(char_key(':'), &mut handler);
        state.handle_key(char_key('x'), &mut handler);
        state.handle_key(press(KeyCode::Esc), &mut handler);
        assert!(state.input.is_none(), "Esc, unlike Enter, leaves the box");
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
    fn bang_prefix_is_always_shell_checked_before_slash_and_natural_language() {
        assert_eq!(
            parse_shell_input("!ls -la"),
            ShellInput::Shell("ls -la".to_string())
        );
        // Leading/trailing whitespace around the command is trimmed,
        // same as every other `parse_shell_input` case.
        assert_eq!(
            parse_shell_input("  !  echo hi  "),
            ShellInput::Shell("echo hi".to_string())
        );
        // A bare `!` (no command text) is still `Shell`, just with an
        // empty command — the handler decides what an empty shell
        // command means, not the parser.
        assert_eq!(parse_shell_input("!"), ShellInput::Shell(String::new()));
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

        // The box is already open (sticky post-submit) — no `:` needed.
        assert!(state.input.is_some());
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
