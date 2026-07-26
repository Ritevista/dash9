//! `dash9 open`: the live, interactive multi-panel viewer with a
//! command bar, status bar, and (with `--assist`) natural-language
//! input. Loads a dashboard TOML, lays out its panels on the
//! 12-column grid (SPEC.md C.1), polls each panel's datasource live,
//! renders all four panel types, and runs the full command grammar
//! (SPEC.md Section B) against a live, mutable session.
//!
//! The event loop and key-handling are `dash9_tui::shell`'s
//! `ShellState`/`CommandHandler` — a pure state machine (no terminal/
//! filesystem/network I/O, fully unit-tested there) calling into a
//! `CommandHandler` implementation that does the real work. This
//! module supplies two implementations: [`GrammarOnlyHandler`] (no
//! assist awareness at all, used by `run_plain`) and
//! `assist_bridge::AssistHandler` (`#[cfg(feature = "assist")]`, used
//! by `run_with_assist`). Both drive the same [`shell_loop`], so the
//! render loop itself is written once — `run_plain`/`run_with_assist`
//! are now thin constructors, not two parallel copies of the loop.
//!
//! `CommandHandler` (defined in `dash9-tui`) has no way to expose
//! `LiveSession`-typed data — `dash9-tui` doesn't and can't depend on
//! this binary crate's types — so panel-grid drawing goes through a
//! second, local, non-generic-crate-boundary trait, [`HasSession`],
//! implemented by both handlers alongside `CommandHandler`.

use std::io::stdout;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration as StdDuration;

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, MouseButton, MouseEventKind,
};
use crossterm::execute;
use dash9_core::{load_path, validate, Command};
use dash9_tui::chart::{ChartModel, ChartViewState};
use dash9_tui::shell::{CommandHandler, CommandResponse, Region, ShellInput, ShellState, Zoom};
use dash9_tui::{
    detail_height, draw_chart, draw_command_bar, draw_gauge, draw_output, draw_panel_outline,
    draw_stat, draw_status_bar, draw_table, draw_zoom_bar, ensure_visible, grid_layout_fit,
    grid_layout_scrolled, help_text, max_grid_scroll, output_height, panel_content_range,
    series_as_table, StatusBarModel, ZoomBarModel,
};
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use tokio::sync::mpsc;

use crate::live_session::{execute_command, LivePanel, LiveSession, SessionUpdate};
use crate::log_recorder::LogRecorder;
use crate::selection::{self, Selection};

const TICK: StdDuration = StdDuration::from_millis(250);
const CHANNEL_CAPACITY: usize = 64;
const STATUS_BAR_HEIGHT: u16 = 1;
const ZOOM_BAR_HEIGHT: u16 = 1;

pub fn run(path: &Path, assist: bool) -> anyhow::Result<()> {
    if assist {
        return run_with_assist(path);
    }
    run_plain(path)
}

fn run_plain(path: &Path) -> anyhow::Result<()> {
    let dashboard = load_path(path)
        .and_then(|file| validate(&file))
        .map_err(|err| anyhow::anyhow!("dashboard invalid: {err}"))?;
    let workspace_root = std::env::current_dir()?;

    let (tx, rx) = mpsc::channel::<SessionUpdate>(CHANNEL_CAPACITY);
    let session = LiveSession::new(&dashboard, workspace_root.clone(), tx);
    let recorder = Arc::new(Mutex::new(LogRecorder::new(workspace_root)));
    let handler = GrammarOnlyHandler {
        session,
        update_rx: rx,
        recorder: Arc::clone(&recorder),
    };
    shell_loop(handler, ShellState::default(), &recorder)
}

#[cfg(not(feature = "assist"))]
fn run_with_assist(_path: &Path) -> anyhow::Result<()> {
    anyhow::bail!(
        "dash9 was built without the `assist` feature; rebuild with `cargo build --features assist`"
    )
}

#[cfg(feature = "assist")]
fn run_with_assist(path: &Path) -> anyhow::Result<()> {
    let dashboard = load_path(path)
        .and_then(|file| validate(&file))
        .map_err(|err| anyhow::anyhow!("dashboard invalid: {err}"))?;
    let workspace_root = std::env::current_dir()?;

    let (tx, rx) = mpsc::channel::<SessionUpdate>(CHANNEL_CAPACITY);
    let session = LiveSession::new(&dashboard, workspace_root.clone(), tx);
    let recorder = Arc::new(Mutex::new(LogRecorder::new(workspace_root.clone())));

    let mut state = ShellState::default();
    let (handler, startup_message) = crate::assist_bridge::AssistHandler::new(
        session,
        rx,
        workspace_root,
        Arc::clone(&recorder),
    );
    if let Some(message) = startup_message {
        state.log.push(dash9_core::LogLine::Result(message));
    }
    shell_loop(handler, state, &recorder)
}

/// Exposes the `LiveSession` a `CommandHandler` implementation wraps,
/// purely so the (already-existing, unchanged) panel-grid drawing
/// functions below can read panel data. Separate from
/// `CommandHandler` itself because `dash9-tui` cannot know about
/// `LiveSession` — see module docs.
pub(crate) trait HasSession {
    fn session(&self) -> &LiveSession;
}

/// The shared render loop: identical for `run_plain` and
/// `run_with_assist` now, generic over whichever `CommandHandler`
/// implementation is driving it. `recorder` is the same handle the
/// handler holds (see [`GrammarOnlyHandler`]/`AssistHandler`) — the
/// handler owns turning `/record on|off` into an open/closed file,
/// this loop owns noticing every new `state.log` line and offering it
/// to the recorder, since that's the one place that sees every line
/// (including ones `ShellState` itself adds, like the echoed command
/// text, which never passes through a handler at all).
fn shell_loop<H: CommandHandler + HasSession>(
    mut handler: H,
    mut state: ShellState,
    recorder: &Arc<Mutex<LogRecorder>>,
) -> anyhow::Result<()> {
    // The last-rendered Grid viewport height, used to nudge `grid_scroll`
    // into keeping a newly-focused panel visible (`docs/specs/session-
    // layout.md` Section B) right when focus actually changes, not
    // recomputed every frame — recomputing it on every render would fight
    // a manual `PageDown`/`PageUp` and snap the view back the very next
    // tick. One frame stale on a terminal resize, self-correcting via the
    // `.min(max_grid_scroll(..))` clamp `draw_session` always applies.
    let mut grid_viewport_height: u16 = 0;

    // In-app drag-to-select + OSC 52 clipboard copy (`docs/specs/open.md`
    // Section L), screen-coordinate state — lives here, not in
    // `ShellState`, for the same reason `grid_viewport_height` does (it's
    // meaningless without a real terminal). `last_buffer` is the most
    // recently rendered frame's content, captured right after every
    // `terminal.draw` — that's what a mouse-up's copy reads from, since
    // it reflects exactly what the user was looking at when they released
    // the button (`selection` extraction never re-renders anything).
    let mut selection: Option<Selection> = None;
    let mut last_buffer: Option<Buffer> = None;

    // Manual init/restore (not `ratatui::run`) so mouse capture can be
    // layered on: `ratatui::init()` still gives raw mode, the alternate
    // screen, and a panic hook that restores both. Mouse capture is
    // enabled separately, and the panic hook is re-wrapped to also
    // disable it — the terminal must never come back to the user with
    // mouse reporting stuck on, panic or not.
    let mut terminal = ratatui::init();
    let _ = execute!(stdout(), EnableMouseCapture);
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(stdout(), DisableMouseCapture);
        previous_hook(info);
    }));

    let result = (|| -> anyhow::Result<()> {
        loop {
            let focused_before = state.focused_panel;
            let before = state.log.len();
            state.apply_poll(&mut handler);
            record_new_lines(&state, recorder, before);
            sync_grid_scroll_to_focus(
                &mut state,
                handler.session(),
                grid_viewport_height,
                focused_before,
            );

            let completed = terminal.draw(|f| {
                let status = handler.status_bar();
                grid_viewport_height =
                    draw_session(f, f.area(), &state, handler.session(), &status);
                if let Some(active) = &selection {
                    f.render_widget(active, f.area());
                }
            })?;
            last_buffer = Some(completed.buffer.clone());

            if !event::poll(TICK)? {
                continue;
            }
            match event::read()? {
                Event::Mouse(mouse) => {
                    handle_mouse(mouse, &mut selection, last_buffer.as_ref());
                }
                Event::Key(key) => {
                    // Any keypress dismisses a lingering post-copy
                    // selection highlight — typing means you've moved on
                    // from whatever you just selected.
                    selection = None;
                    let focused_before = state.focused_panel;
                    let before = state.log.len();
                    let should_quit = state.handle_key(key, &mut handler);
                    record_new_lines(&state, recorder, before);
                    sync_grid_scroll_to_focus(
                        &mut state,
                        handler.session(),
                        grid_viewport_height,
                        focused_before,
                    );
                    if should_quit {
                        return Ok(());
                    }
                }
                _ => {}
            }
        }
    })();

    let _ = execute!(stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}

/// One mouse event's effect on the in-progress/just-finished selection
/// (`docs/specs/open.md` Section L). Left-button drag only — `Down`
/// starts a fresh selection (replacing any previous one, same as a new
/// click in a real terminal), `Drag` extends it, `Up` finalizes it:
/// copies to the clipboard if it covers more than one cell, otherwise
/// (a plain click) clears it. Every other mouse event (scroll, right/
/// middle click, plain move) is a deliberate no-op for v1 — nothing
/// else has a binding yet.
fn handle_mouse(
    mouse: event::MouseEvent,
    selection: &mut Option<Selection>,
    last_buffer: Option<&Buffer>,
) {
    let at = (mouse.column, mouse.row);
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            *selection = Some(Selection::new(at));
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if let Some(active) = selection.as_mut() {
                active.cursor = at;
            }
        }
        MouseEventKind::Up(MouseButton::Left) => {
            let Some(active) = selection else { return };
            if active.is_empty() {
                *selection = None;
                return;
            }
            if let Some(text) = last_buffer.and_then(|buffer| active.extract_text(buffer)) {
                let _ = selection::copy_to_clipboard(&text);
            }
        }
        _ => {}
    }
}

/// After a `Tab`/`Shift+Tab` (or a `dash open` reload changing panel
/// count, via `ShellState::apply_poll`'s own focus-reclamp) moves
/// `focused_panel`, nudge `grid_scroll` the minimum amount to bring it
/// into view (`layout::ensure_visible`) — a real, persisted mutation of
/// `state.grid_scroll` made by the composition root (which knows the real
/// terminal size) right at the moment focus changes, not a per-frame
/// display-time computation (`ShellState` itself stays terminal-size-
/// agnostic; see its `grid_scroll` field docs — this is the binary crate
/// adjusting a public field after the fact, the same category as it
/// already does with `state.log` for startup messages).
fn sync_grid_scroll_to_focus(
    state: &mut ShellState,
    session: &LiveSession,
    viewport_height: u16,
    focused_before: usize,
) {
    if state.focused_panel == focused_before {
        return;
    }
    let grids: Vec<_> = session.panels.iter().map(|p| p.grid).collect();
    let Some(range) = panel_content_range(&grids, state.focused_panel) else {
        return;
    };
    let max_scroll = max_grid_scroll(&grids, viewport_height);
    state.grid_scroll = ensure_visible(state.grid_scroll, range, viewport_height).min(max_scroll);
}

/// Offers every log line added since `before` to the recorder — a
/// no-op on its end unless `/record on` is active. `before`/`after`
/// bracket a single `apply_poll`/`handle_key` call, so this is immune
/// to `ShellState`'s own `MAX_LOG_LINES` trimming shifting indices
/// across ticks (nothing persists between calls); it would only miss
/// lines if one single call added enough entries to itself trigger a
/// trim, which needs hundreds of lines from one keypress or poll
/// drain — never happens in practice.
fn record_new_lines(state: &ShellState, recorder: &Arc<Mutex<LogRecorder>>, before: usize) {
    let after = state.log.len();
    if after <= before {
        return;
    }
    lock_recorder(recorder).record(&state.log[before..after]);
}

/// A poisoned lock here would mean an earlier panic happened while
/// holding it — recovering the guard anyway rather than propagating
/// the poison keeps one bad recorder write from taking down the
/// entire render loop over what's fundamentally a best-effort side
/// channel, not session-critical state.
fn lock_recorder(recorder: &Mutex<LogRecorder>) -> std::sync::MutexGuard<'_, LogRecorder> {
    recorder
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The `run_plain` handler: no `dash9-assist` awareness at all. Every
/// AI-only `ShellInput` variant reports the same "requires --assist"
/// message; `NaturalLanguage` (any line with no leading `/`) reports
/// that natural language itself needs `--assist`.
struct GrammarOnlyHandler {
    session: LiveSession,
    update_rx: mpsc::Receiver<SessionUpdate>,
    recorder: Arc<Mutex<LogRecorder>>,
}

impl HasSession for GrammarOnlyHandler {
    fn session(&self) -> &LiveSession {
        &self.session
    }
}

impl CommandHandler for GrammarOnlyHandler {
    fn execute(&mut self, input: ShellInput, focused_panel: usize) -> CommandResponse {
        match input {
            ShellInput::Grammar(Command::Quit) => CommandResponse {
                should_quit: true,
                ..CommandResponse::default()
            },
            ShellInput::Grammar(cmd) => {
                CommandResponse::result(execute_command(&mut self.session, focused_panel, cmd))
            }
            ShellInput::Help(topic) => CommandResponse::result(help_text(topic.as_deref())),
            ShellInput::CommandError(err) => CommandResponse::result(err.to_string()),
            ShellInput::Export { format, path } => CommandResponse::result(
                self.session
                    .export_panel(focused_panel, format, path.as_deref()),
            ),
            ShellInput::RecordingStatus => {
                CommandResponse::result(lock_recorder(&self.recorder).status())
            }
            ShellInput::SetRecording { on, path } => {
                CommandResponse::result(lock_recorder(&self.recorder).set(on, path))
            }
            ShellInput::Shell(command) => {
                CommandResponse::result(self.session.spawn_shell_command(&command))
            }
            ShellInput::NaturalLanguage(_) => CommandResponse::result(
                "natural language requires --assist (or prefix a command with /, see /help)"
                    .to_string(),
            ),
            ShellInput::ModelStatus
            | ShellInput::ModelSwitch(_)
            | ShellInput::ToggleAssist
            | ShellInput::AssistStatus
            | ShellInput::SetAssist(_) => {
                CommandResponse::result("AI features require --assist".to_string())
            }
        }
    }

    fn poll(&mut self, _focused_panel: usize) -> Option<CommandResponse> {
        match self.update_rx.try_recv() {
            Ok(update) => {
                let mut log_entries = Vec::new();
                self.session.apply_update(update, &mut log_entries);
                Some(CommandResponse {
                    log_entries,
                    ..CommandResponse::default()
                })
            }
            Err(_) => None,
        }
    }

    fn panel_count(&self) -> usize {
        self.session.panels.len()
    }

    fn status_bar(&self) -> StatusBarModel {
        status_bar_for(&self.session, None)
    }
}

/// Shared by both handlers so the "count panels, summarize
/// datasources, compute health" logic isn't duplicated between
/// `GrammarOnlyHandler` and `AssistHandler`.
pub(crate) fn status_bar_for(
    session: &LiveSession,
    assist: Option<dash9_tui::AssistStatusLine>,
) -> StatusBarModel {
    StatusBarModel {
        title: session.title.clone(),
        panel_count: session.panels.len(),
        datasource_summary: session.datasource_summary(),
        health: session.datasource_health(),
        assist,
    }
}

/// `(grid, detail, output, bar)` heights for `draw_session`'s outer
/// `Layout::vertical`, in that order — split out from `draw_session`
/// itself purely to stay under `clippy::too_many_lines`, not because
/// this logic is reusable elsewhere.
///
/// `Output`/`Log` maximize (`+`, `docs/specs/open.md` Section F) takes
/// over the space `Main` normally gets — the direct parallel to
/// `Zoom::Focus` doing the same for a single panel: grid and detail both
/// go to `0` (`Main` is fully hidden while another region is maximized),
/// and the maximized pane gets everything left after the *other* of
/// `Output`/`Log` keeps its own normal small size (so it's still visible,
/// just not dominant). `Main` itself has no maximize case here — it
/// already owns `zoom` for the same purpose. Every other case (including
/// editing, which doesn't change `region`) falls through to the shared
/// default sizing: output first, then the command bar, then detail, then
/// whatever's left to the grid — same order `draw_session` always used,
/// just using `dash9_tui::command_bar_height` (precise) instead of the
/// old `MIN_BAR_HEIGHT` floor.
/// `focused_panel_thresholds` is just `session.panels.get(state.focused_panel)
/// .map_or(0, |p| p.thresholds.len())` — passed in rather than a full
/// `&LiveSession`/`&Session` so this stays a pure function of plain data,
/// directly unit-testable (`tests::pane_heights_*` below) without needing
/// a real `LiveSession` fixture (async pollers, datasources, ...) just to
/// exercise sizing math that never touches any of that.
fn pane_heights(
    state: &ShellState,
    focused_panel_thresholds: usize,
    grids: &[dash9_core::GridSpec],
    available: u16,
) -> (u16, u16, u16, u16) {
    match (state.region, state.pane_maximized) {
        (Region::Output, true) => {
            let bar = dash9_tui::command_bar_height(&state.log, available);
            let output = available.saturating_sub(bar);
            (0, 0, output, bar)
        }
        (Region::Log, true) => {
            let output = output_height(&state.log, available);
            let bar = available.saturating_sub(output);
            (0, 0, output, bar)
        }
        _ => {
            let output = output_height(&state.log, available);
            let available_after_output = available.saturating_sub(output);

            let bar = dash9_tui::command_bar_height(&state.log, available_after_output);
            let available_after_bar = available_after_output.saturating_sub(bar);

            // The detail pane (`Space`) is independent of zoom
            // (`ShellState::detail_open` docs) — it never replaces the
            // grid/layout/focus area above it, only takes space below
            // it, so the chart(s) stay visible the whole time you're
            // inspecting one.
            let detail = if state.detail_open {
                detail_height(focused_panel_thresholds, available_after_bar)
            } else {
                0
            };
            let available_for_grid = available_after_bar.saturating_sub(detail);

            let grid = match state.zoom {
                Zoom::Focus => available_for_grid,
                Zoom::Grid | Zoom::Layout => {
                    dash9_tui::content_height(grids).min(available_for_grid)
                }
            };
            (grid, detail, output, bar)
        }
    }
}

fn draw_session(
    frame: &mut Frame,
    area: Rect,
    state: &ShellState,
    session: &LiveSession,
    status: &StatusBarModel,
) -> u16 {
    let grids: Vec<_> = session.panels.iter().map(|p| p.grid).collect();

    // `Focus` gets the whole remaining main area — "one panel, full-pane"
    // (`docs/specs/session-layout.md` Section A.3) — while `Grid`/`Layout`
    // shrink-wrap to their content like before zoom levels existed
    // (`content_height`'s own doc comment). The detail pane, output pane,
    // and command bar are carved out first, top to bottom, so the grid
    // never pushes any of them to nothing on a large or scrolled-open
    // dashboard — detail and output are reserved *before* the grid gets
    // whatever's left, not the grid stretching over them.
    let reserved_fixed = STATUS_BAR_HEIGHT + ZOOM_BAR_HEIGHT;
    let available_after_fixed = area.height.saturating_sub(reserved_fixed);

    let focused_panel_thresholds = session
        .panels
        .get(state.focused_panel)
        .map_or(0, |p| p.thresholds.len());
    let (grid_height, detail_area_height, output_area_height, bar_area_height) = pane_heights(
        state,
        focused_panel_thresholds,
        &grids,
        available_after_fixed,
    );

    // Every constraint below is an exact `Length`, not a soaking `Min(0)`
    // — deliberate, see `dash9_tui::MAX_LOG_HEIGHT`'s docs for the bug
    // that shape used to cause (the log silently absorbing every leftover
    // row on a short dashboard or tall terminal). Genuinely leftover
    // space — everything already capped/sized and still not filling the
    // terminal — becomes unlabeled blank space in `_spacer`, at the very
    // bottom, rather than being attributed to any one pane.
    let [status_area, zoom_area, grid_area, detail_area, output_area, bar_area, _spacer] =
        Layout::vertical([
            Constraint::Length(STATUS_BAR_HEIGHT),
            Constraint::Length(ZOOM_BAR_HEIGHT),
            Constraint::Length(grid_height),
            Constraint::Length(detail_area_height),
            Constraint::Length(output_area_height),
            Constraint::Length(bar_area_height),
            Constraint::Min(0),
        ])
        .areas(area);

    draw_status_bar(frame, status_area, status);

    // `state.grid_scroll` is already the right value going into this
    // frame — `PageUp`/`PageDown` set it directly (`shell.rs`), and
    // `sync_grid_scroll_to_focus` already nudged it for any focus change
    // since the last frame. This is just the final defensive clamp
    // against the content/viewport actually on screen right now.
    let grid_scroll = state
        .grid_scroll
        .min(max_grid_scroll(&grids, grid_area.height));
    draw_zoom_bar(
        frame,
        zoom_area,
        &zoom_bar_model(state, &grids, grid_area, grid_scroll),
    );

    // While the command box is capturing keystrokes, Space types a literal
    // character instead of toggling detail (`shell.rs::handle_key`), so
    // no panel's hint should claim otherwise — border/focus color stays
    // as-is (`Tab` still moves it while editing), only the hint text is
    // gated on this.
    let editing = state.input.is_some();

    // A panel's border only lights up as "focused" while `Main` actually
    // has `Tab`-focus (`docs/specs/open.md` Section E) — only one thing
    // on screen should ever look focused at a time, so Output/Log
    // stealing focus dims whichever panel was highlighted, the same way
    // it dims once you Tab away today. `usize::MAX` never matches a real
    // panel index, so every panel draws unfocused without needing a
    // second code path through `draw_dashboard`/`draw_layout`.
    let main_focused = state.region == Region::Main;
    let focused_panel = if main_focused {
        state.focused_panel
    } else {
        usize::MAX
    };

    match state.zoom {
        Zoom::Grid => draw_dashboard(
            frame,
            grid_area,
            &grids,
            session,
            focused_panel,
            grid_scroll,
            editing,
        ),
        Zoom::Layout => draw_layout(frame, grid_area, &grids, session, focused_panel, editing),
        Zoom::Focus => {
            if let Some(panel) = session.panels.get(state.focused_panel) {
                draw_panel(
                    frame,
                    grid_area,
                    panel,
                    main_focused,
                    main_focused && !editing,
                );
            }
        }
    }

    if state.detail_open {
        let detail = panel_detail(session, state.focused_panel);
        dash9_tui::draw_panel_detail(frame, detail_area, detail.as_ref());
    }

    // Same defensive clamp `grid_scroll` gets above — `state.output_scroll`
    // is the user's last explicit paging request (`ShellState` has no
    // notion of terminal size), this is the final clamp against the
    // output pane's actual rendered height this frame.
    let output_scroll = state.output_scroll.min(dash9_tui::max_output_scroll(
        &state.log,
        output_area.height.saturating_sub(2),
    ));
    let output_focused = state.region == Region::Output;
    draw_output(
        frame,
        output_area,
        &state.log,
        output_scroll,
        output_focused,
        output_focused && !editing,
    );

    let log_focused = state.region == Region::Log;
    draw_command_bar(
        frame,
        bar_area,
        &state.log,
        state.input.as_deref(),
        &command_bar_hint(state, status),
        state.log_scroll,
        dash9_tui::LogFocus {
            focused: log_focused,
            show_hint: log_focused && !editing,
        },
    );

    grid_area.height
}

/// Same one-line region hint the zoom bar always showed, generalized
/// past just Main's zoom levels now that `Tab`-focus can land on Output
/// or Log too (`docs/specs/open.md` Section E) — those two get their own
/// short hint here since `zoom_hint` (`dash9-tui::shell`) only knows
/// about `Zoom`, not the region model layered above it. `detail_open`'s
/// `"+ detail"` suffix stays region-independent, same as before: which
/// panel's detail is open is `state.focused_panel`, tracked separately
/// from whichever region currently has `Tab`-focus.
const OUTPUT_OR_LOG_HINT: &str = "PageUp/PageDown scroll";

fn zoom_bar_model(
    state: &ShellState,
    grids: &[dash9_core::GridSpec],
    grid_area: Rect,
    grid_scroll: u16,
) -> ZoomBarModel {
    let (region_name, hint) = match state.region {
        Region::Main => {
            let zoom_name = match state.zoom {
                Zoom::Layout => "Layout",
                Zoom::Grid => "Grid",
                Zoom::Focus => "Focus",
            };
            let mut hint = dash9_tui::zoom_hint(state.zoom).to_string();
            if state.zoom == Zoom::Grid {
                if let Some(suffix) = grid_paging_suffix(grids, grid_area, grid_scroll) {
                    hint.push_str(&suffix);
                }
            }
            (zoom_name.to_string(), hint)
        }
        Region::Output | Region::Log => {
            let name = if state.region == Region::Output {
                "Output"
            } else {
                "Log"
            };
            let hint = if state.pane_maximized {
                format!("{OUTPUT_OR_LOG_HINT} · - restore")
            } else {
                format!("{OUTPUT_OR_LOG_HINT} · + maximize")
            };
            let name = if state.pane_maximized {
                format!("{name} (maximized)")
            } else {
                name.to_string()
            };
            (name, hint)
        }
    };
    let zoom_label = if state.detail_open {
        format!("{region_name} + detail")
    } else {
        region_name
    };
    ZoomBarModel { zoom_label, hint }
}

/// `"panels 5-8 of 12 — PageDown for more"`-style suffix
/// (`docs/specs/session-layout.md` Section A.2's paging affordance) —
/// `None` when every panel already fits (nothing to page), matching how
/// `command_bar.rs`'s log title only changes "(scrolled...)" once there's
/// actually somewhere else to scroll to.
fn grid_paging_suffix(
    grids: &[dash9_core::GridSpec],
    grid_area: Rect,
    scroll: u16,
) -> Option<String> {
    let total = grids.len();
    let max_scroll = max_grid_scroll(grids, grid_area.height);
    if total == 0 || max_scroll == 0 {
        return None;
    }
    let rects = grid_layout_scrolled(grid_area, grids, scroll);
    let visible_indices: Vec<usize> = rects
        .iter()
        .enumerate()
        .filter(|(_, r)| r.width > 0 && r.height > 0)
        .map(|(i, _)| i)
        .collect();
    let (Some(&first), Some(&last)) = (visible_indices.first(), visible_indices.last()) else {
        return Some(format!(" · 0 of {total} panels visible"));
    };
    let more = if scroll < max_scroll {
        "PageDown for more"
    } else {
        "PageUp for more"
    };
    Some(format!(
        " · panels {}-{} of {total} — {more}",
        first + 1,
        last + 1
    ))
}

/// Extracts the plain fields `dash9_tui::PanelDetail` needs from the
/// focused panel. Lives here, not in `dash9-tui`, because it needs
/// both `LivePanel` *and* `LiveDatasource` (for the datasource's type
/// and URL) — both binary-crate types `dash9-tui` can't depend on.
fn panel_detail(session: &LiveSession, focused_panel: usize) -> Option<dash9_tui::PanelDetail<'_>> {
    let panel = session.panels.get(focused_panel)?;
    let datasource_line = match session.datasources.get(&panel.datasource) {
        Some(ds) => format!("{}: {} {}", panel.datasource, ds.datasource_type, ds.url),
        None => format!("{} (not configured)", panel.datasource),
    };
    Some(dash9_tui::PanelDetail {
        title: &panel.title,
        panel_type: panel.panel_type,
        datasource_line,
        query: &panel.query,
        allow_empty: panel.allow_empty,
        latency_budget: panel.latency_budget,
        grid: panel.grid,
        thresholds: &panel.thresholds,
        last_result: panel.last_result.as_ref(),
    })
}

/// State-dependent footer text for the command-bar input line when
/// not actively typing: surfaces `y`/`n` only while a proposal is
/// genuinely pending, and `a` only when there's an assist handler to
/// toggle at all — never shown as options that would currently do
/// nothing. Zoom/region-specific keys (Layout/Grid/Focus paging, `i`,
/// `Esc`) live in the zoom bar (`zoom_bar_model`) instead — this hint
/// stays about the command box itself, the one region that's the same
/// regardless of zoom level.
fn command_bar_hint(state: &ShellState, status: &StatusBarModel) -> String {
    let mut hints = vec![
        "/command · text = AI",
        "Tab reaches this box",
        "+/- zoom",
        "/help",
    ];
    if !state.pending_proposals.is_empty() {
        hints.push("y/n confirm proposal");
    }
    if status.assist.is_some() {
        hints.push("a toggle AI");
    }
    hints.push("q quit");
    hints.join(" · ")
}

fn draw_dashboard(
    frame: &mut Frame,
    area: Rect,
    grids: &[dash9_core::GridSpec],
    session: &LiveSession,
    focused_panel: usize,
    scroll: u16,
    editing: bool,
) {
    let rects = grid_layout_scrolled(area, grids, scroll);

    for (index, panel) in session.panels.iter().enumerate() {
        let rect = rects[index];
        if rect.width == 0 || rect.height == 0 {
            continue; // Scrolled or positioned entirely out of view.
        }
        let focused = index == focused_panel;
        draw_panel(frame, rect, panel, focused, focused && !editing);
    }
}

/// The Layout zoom level (`docs/specs/session-layout.md` Section A.1):
/// every panel, all at once, title-and-border only — `grid_layout_fit`
/// scales panels down to fit `area` instead of paging/clipping, so unlike
/// `draw_dashboard` there is no "positioned off-screen" case to skip; a
/// zero-size rect only happens in the genuinely degenerate case of an
/// area shorter than the dashboard's total grid-row count even at the
/// minimum 1-row-unit height (`grid_layout_fit`'s own doc comment).
fn draw_layout(
    frame: &mut Frame,
    area: Rect,
    grids: &[dash9_core::GridSpec],
    session: &LiveSession,
    focused_panel: usize,
    editing: bool,
) {
    let rects = grid_layout_fit(area, grids);

    for (index, panel) in session.panels.iter().enumerate() {
        let rect = rects[index];
        if rect.width == 0 || rect.height == 0 {
            continue;
        }
        let focused = index == focused_panel;
        draw_panel_outline(frame, rect, &panel.title, focused, focused && !editing);
    }
}

fn draw_panel(frame: &mut Frame, area: Rect, panel: &LivePanel, focused: bool, show_hint: bool) {
    let Some(result) = panel.last_result.as_ref() else {
        draw_placeholder(frame, area, &panel.title, "(loading…)", focused, show_hint);
        return;
    };
    let core_frame = match result {
        Err(err) => {
            draw_placeholder(
                frame,
                area,
                &panel.title,
                &err.to_string(),
                focused,
                show_hint,
            );
            return;
        }
        Ok(core_frame) => core_frame,
    };
    if core_frame.is_empty() {
        draw_placeholder(frame, area, &panel.title, "(no data)", focused, show_hint);
        return;
    }

    match panel.panel_type {
        dash9_core::PanelType::Timeseries
        | dash9_core::PanelType::Gauge
        | dash9_core::PanelType::Stat => {
            match ChartModel::project(
                &panel.title,
                core_frame,
                &panel.thresholds,
                &ChartViewState::default(),
                usize::from(area.width),
            ) {
                Ok(model) => match panel.panel_type {
                    dash9_core::PanelType::Timeseries => {
                        draw_chart(frame, area, &model, focused, show_hint);
                    }
                    dash9_core::PanelType::Gauge => {
                        draw_gauge(frame, area, &model, focused, show_hint);
                    }
                    dash9_core::PanelType::Stat => {
                        draw_stat(frame, area, &model, focused, show_hint);
                    }
                    dash9_core::PanelType::Table => unreachable!("handled in the outer match arm"),
                },
                Err(err) => {
                    draw_placeholder(
                        frame,
                        area,
                        &panel.title,
                        &err.to_string(),
                        focused,
                        show_hint,
                    );
                }
            }
        }
        dash9_core::PanelType::Table => {
            // Prometheus never produces a native `FrameKind::Table`
            // (see `series_as_table`'s docs), so the common case here
            // is synthesizing one from the instant vector it actually
            // returned; `core_frame.table` is only used when some
            // future datasource does return one natively.
            match core_frame
                .table
                .clone()
                .or_else(|| series_as_table(core_frame))
            {
                Some(table) => draw_table(frame, area, &table, &panel.title, focused, show_hint),
                None => {
                    draw_placeholder(
                        frame,
                        area,
                        &panel.title,
                        "(no table data)",
                        focused,
                        show_hint,
                    );
                }
            }
        }
    }
}

fn draw_placeholder(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    message: &str,
    focused: bool,
    show_hint: bool,
) {
    let block = dash9_tui::pane_block(
        title,
        focused,
        None,
        show_hint.then_some(dash9_tui::PANEL_HINT),
    );
    frame.render_widget(Paragraph::new(message.to_string()).block(block), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use dash9_core::GridSpec;

    /// Regression test for the real bug this session's changes fixed: an
    /// earlier version gave the whole command bar an uncapped
    /// `Constraint::Min(0)`, so it silently absorbed every leftover
    /// terminal row — confirmed live, 17 blank rows on a 50-row terminal.
    /// `bar`/`output` must stay at their small default sizes no matter
    /// how much `available` space there is, with the difference going
    /// nowhere in particular (an unlabeled spacer in `draw_session`, not
    /// exercised here) rather than into the log.
    #[test]
    fn pane_heights_caps_output_and_bar_even_with_lots_of_available_space() {
        let state = ShellState::default();
        let (grid, detail, output, bar) = pane_heights(&state, 0, &[], 200);
        assert_eq!(grid, 0, "empty dashboard: no panels to size");
        assert_eq!(detail, 0, "detail closed by default");
        assert_eq!(
            output,
            dash9_tui::MIN_OUTPUT_HEIGHT,
            "empty output pane stays at its minimum, not 200"
        );
        assert_eq!(
            bar,
            dash9_tui::MIN_LOG_HEIGHT + 3,
            "empty log stays at its minimum (+ the input line), not 200"
        );
    }

    #[test]
    fn pane_heights_output_maximized_takes_over_mains_space_log_stays_small() {
        let mut state = ShellState::default();
        state.region = Region::Output;
        state.pane_maximized = true;
        let (grid, detail, output, bar) = pane_heights(&state, 0, &[], 100);
        assert_eq!(grid, 0, "Main is fully hidden while Output is maximized");
        assert_eq!(detail, 0);
        let expected_bar = dash9_tui::command_bar_height(&state.log, 100);
        assert_eq!(bar, expected_bar, "log/input keep their normal small size");
        assert_eq!(
            output,
            100 - expected_bar,
            "output takes everything Main and the bar aren't using"
        );
    }

    #[test]
    fn pane_heights_log_maximized_takes_over_mains_space_output_stays_small() {
        let mut state = ShellState::default();
        state.region = Region::Log;
        state.pane_maximized = true;
        let (grid, detail, output, bar) = pane_heights(&state, 0, &[], 100);
        assert_eq!(grid, 0, "Main is fully hidden while Log is maximized");
        assert_eq!(detail, 0);
        let expected_output = output_height(&state.log, 100);
        assert_eq!(
            output, expected_output,
            "output keeps its normal small size"
        );
        assert_eq!(
            bar,
            100 - expected_output,
            "the log takes everything Main and output aren't using"
        );
    }

    #[test]
    fn pane_heights_maximize_is_ignored_while_region_is_main() {
        // `pane_maximized` is meaningless for `Main` (it has `zoom`
        // instead) — a state where it's somehow `true` alongside
        // `Region::Main` must still fall through to the default sizing,
        // not be treated as some fourth maximize case.
        let default_state = ShellState::default();
        let mut quirky_state = ShellState::default();
        quirky_state.pane_maximized = true;
        assert_eq!(
            pane_heights(&default_state, 0, &[], 80),
            pane_heights(&quirky_state, 0, &[], 80),
        );
    }

    #[test]
    fn pane_heights_focus_zoom_gives_the_grid_the_whole_remaining_area() {
        let mut state = ShellState::default();
        state.zoom = Zoom::Focus;
        let (grid, _detail, output, bar) = pane_heights(&state, 0, &[], 50);
        // Unlike Grid/Layout (capped to `content_height`), Focus claims
        // everything left after output/bar, regardless of panel content.
        assert_eq!(grid, 50 - output - bar);
    }

    #[test]
    fn pane_heights_detail_open_reserves_space_computed_by_the_real_detail_height_fn() {
        let mut state = ShellState::default();
        state.detail_open = true;
        // Empty `grids` (`content_height([]) == 0`) so `grid` isolates to
        // exactly 0 regardless of leftover space, same as the dedicated
        // Grid-zoom-capping test — this test's own focus is `detail`.
        let (grid, detail, output, bar) = pane_heights(&state, 2, &[], 60);
        let available_for_detail = 60 - output - bar;
        assert_eq!(
            detail,
            detail_height(2, available_for_detail),
            "must match the real detail_height computation, not a hardcoded number"
        );
        assert!(detail > 0, "2 thresholds must need some real space");
        assert_eq!(grid, 0, "empty dashboard: content_height([]) is 0");
    }

    #[test]
    fn pane_heights_grid_zoom_is_capped_to_content_height_not_all_available_space() {
        let grids = [GridSpec {
            row: 0,
            col: 0,
            w: 6,
            h: 4,
        }];
        let state = ShellState::default(); // Zoom::Grid by default
        let (grid, _detail, output, bar) = pane_heights(&state, 0, &grids, 200);
        let expected = dash9_tui::content_height(&grids);
        assert_eq!(
            grid, expected,
            "Grid zoom must not stretch past its content height, unlike Focus"
        );
        assert!(
            grid < 200 - output - bar,
            "there must be real leftover space this doesn't claim"
        );
    }
}
