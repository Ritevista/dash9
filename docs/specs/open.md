# `dash9 open` — the interactive session — docs/specs/open.md

`dash9 open <path>` is the live, interactive multi-panel viewer: a
panel grid, a scrollable session log, and a command bar, all driven by
the one command grammar `SPEC.md` Section B defines. This document is
the single source of truth for everything `open` adds on top of that
grammar — the shell meta-commands, keybindings, the zoom levels, the
status bar, and continuous log recording — none of which is part of
SPEC.md's append-only grammar (Section B.1) because it is shell/UI
behavior, not something a dashboard TOML file or a scripted `dash9
test` run ever needs to express.

Status: **Accepted** — every behavior below is implemented and has
unit or integration coverage
(`crates/dash9-tui/src/{shell,layout,pane,output}.rs`,
`crates/dash9-tui/src/{detail_view,status_bar,command_bar,draw}.rs`,
`crates/dash9/src/{open,live_session,log_recorder,selection}.rs`). Prerequisites:
`SPEC.md` (grammar, error codes, dashboard schema), `docs/architecture/rendering.md`.
This spec does not cover the assistant's own AI behavior (context
assembly, the contract loop, the LLM client) — that is
[`docs/specs/assist.md`](assist.md); it does cover the parts of the
interactive session every `dash9 open` invocation has regardless of
whether assist wiring is available (Section H below is the one
exception, since the `/ai` verbs and the assist status-bar segment
only do anything when it is). Section E (keybindings) and
Section G (zoom levels) reflect `docs/specs/session-layout.md`
Sections A-D, which are implemented; that spec's Section E (`/save
png`) is not — Section I below still accurately says so.

## Contents

- [A. Invocation](#a-invocation)
- [B. Session model](#b-session-model)
- [C. Input routing](#c-input-routing)
- [D. Shell meta-commands](#d-shell-meta-commands)
- [E. Keybindings](#e-keybindings)
- [F. The command log and the output pane](#f-the-command-log-and-the-output-pane)
- [G. Zoom levels: Layout, Grid, Focus](#g-zoom-levels-layout-grid-focus)
- [H. Status bar](#h-status-bar)
- [I. Panel export (`/save`)](#i-panel-export-save)
- [J. Continuous log recording (`/record`)](#j-continuous-log-recording-record)
- [K. Shell command execution (`!`)](#k-shell-command-execution)
- [L. Mouse selection and clipboard copy](#l-mouse-selection-and-clipboard-copy)
- [M. Non-goals](#m-non-goals)

---

## A. Invocation

```
dash9 open <path> [--prometheus-url <url>]
```

`<path>` is a dashboard TOML file (`SPEC.md` Section C) or Grafana
dashboard JSON (`docs/specs/grafana-dashboards.md`), detected from the
file itself — `.json`/`.toml` extension, content-sniffed if ambiguous
— never a separate flag. Either way it is loaded and validated exactly
as `dash9 test` loads one (`SPEC.md` C.3 step 1) — an invalid file is a
startup error, not something the session recovers from interactively.
`--prometheus-url` (default `http://localhost:9090`) is where every
Prometheus-typed panel in a Grafana JSON import resolves its
datasource to — a Grafana export carries only an internal datasource
`uid`, never a queryable URL; ignored for a TOML dashboard, which
declares its own `[[datasources]] url`.

**No `--assist` flag.** An earlier version required passing `--assist`
at the CLI, on top of the `assist` Cargo feature (on by default), to
get natural-language input alongside the command grammar — two
separate on/off questions for the same capability, one at build time
and one at process-start time. The CLI flag is gone: a build with the
`assist` feature now always attempts to load
`~/.config/dash9/assist.toml` (`docs/specs/assist.md` Section D) and
wire up the assistant when `dash9 open` starts, exactly as `--assist`
used to trigger — a missing or broken config degrades gracefully to
"assist unavailable: \<reason\>" (logged once at startup) rather than
failing the whole session, same as before. The one remaining on/off
question is answered entirely at runtime, by `/ai on`/`/ai off`
(Section D) or the `a` key (Section E) — there is no longer a separate
"was it even requested" gate above that. Building with
`--no-default-features` (dropping the `assist` Cargo feature) is still
how you get a build with no AI code linked in at all; `/ai`/`/model`
and natural-language input report they need that feature rather than
doing anything, the same shape "assist unavailable" already had.

## B. Session model

`dash9 open` builds one `LiveSession` from the loaded dashboard and
spawns one polling task per panel, each fetching on its own
`refresh`/`range`-driven cadence against its configured datasource.
Grammar verbs that mutate session state (`ds add`, `panel type` /
`panel threshold` / `panel title`, `range`, `refresh`) take effect on
already-running pollers without restarting them; `dash save`/`dash
open` (Section B.3) rewrite or replace the whole session, and `dash
open` on an existing session tears down and respawns every poller
against the newly loaded dashboard. Ad-hoc `q` queries and `ds
metrics` run once against the focused panel's datasource and post
their result to the log (Section F) rather than becoming a panel.

Every panel result — pass or fail — is retained as the panel's `last_result`,
which both the panel grid (small chart box) and the detail pane
(Section G.1, full data table) render from; there is exactly one source
of truth for "what did this panel's query last return," not a
separate copy for each view.

## C. Input routing

The command bar has exactly one discriminator for what a submitted
line means, decided before anything else: its leading character —
`!`, `/`, or neither.

- A line starting with `!` always runs through the user's shell
  (Section K) — unrelated to `SPEC.md`'s grammar, and checked first.
- A line starting with `/` is always a **structured command**: a shell
  meta-command (Section D) or `SPEC.md` grammar via `dash9_core::parse`.
  Text after `/` that matches neither is a hard error (`E002`/`E003`,
  `SPEC.md` B.5) — it is never silently retried as natural language.
- A line with **no** leading `!` or `/` is always **natural language**,
  unconditionally — even if it happens to parse as valid grammar
  (`range 5m` with no `/` is sent as natural language, not executed).
  It is handed to the assistant only when the session was built with
  assist wiring available and the assistant is currently on
  (Section D); otherwise it is reported as unavailable.

This rule has no exceptions and no fallback direction — a line's
routing is decided by its first character alone, never by whether
parsing succeeds.

## D. Shell meta-commands

These share `dash9_core::VerbSpec`'s shape with `SPEC.md` Section B.3's
grammar table (so `/help`, Section D below, can list and render both
uniformly) but are **not** part of the append-only grammar guarantee —
they are shell/UI concerns no dashboard TOML file or scripted `dash9
test` run ever emits. Changing one only requires updating this
document, not treating it as a breaking grammar change.

| Verb | Args | Example | Description |
|---|---|---|---|
| `help` | `[topic]` | `/help ds` | List every command group, or show detail for one topic/verb. |
| `model` | — | `/model` | Show the current AI model and any configured known models. |
| `model` | `<name>` | `/model gemini-flash` | Switch the AI model (resets conversation history). |
| `ai` | — | `/ai` | Show whether the assistant is on/off and the current model. |
| `ai on` | — | `/ai on` | Turn the assistant on (explicit, idempotent — unlike the `a` key, which toggles). |
| `ai off` | — | `/ai off` | Turn the assistant off. |
| `ai model` | `<name>` | `/ai model gemini-flash` | Alias of `/model <name>`. |
| `ai context` | — | `/ai context` | Show what the assistant currently knows: configured datasources, whether a dashboard TOML is available to send, the active time range, and how many messages are in the running conversation. |
| `ai clear` | — | `/ai clear` | Clear the running conversation history without switching models. |
| `save` | `<format> [path]` | `/save csv exports/out.csv` | Export the focused panel's data (`csv`, `md`, or `png`; see Section I). |
| `record` | — | `/record` | Show whether continuous log recording is on, and where. |
| `record on` | `[path]` | `/record on exports/session.jsonl` | Start recording every log line to a JSONL file (Section J). |
| `record off` | — | `/record off` | Stop recording. |
| `quit` | — | `/quit` | End the session (same effect as the `q` key or `Ctrl+C`). |

`/help` (bare) lists every top-level group — every `SPEC.md` B.3 verb
group plus every group above — with a one-line blurb; `/help <topic>`
matches a single-word topic against every verb in that group (`/help
ds` lists `ds add`, `ds list`, `ds metrics`) or an exact multi-word
topic against one verb (`/help "ds add"` shows only that verb); an
unmatched topic reports "unknown help topic" rather than showing
nothing. `/?` is an alias for bare `/help`.

`/ai`, `/model`, and their variants are meaningful only when the
session was built with assist wiring available (the `assist` Cargo
feature, on by default — Section A) and its config loaded
successfully; every build still recognizes them but reports the
assistant as unavailable rather than treating them as unknown
commands, whether that's because the feature wasn't compiled in or
because `~/.config/dash9/assist.toml` is missing/broken. Full
assistant behavior (context assembly, the contract loop, proposal
classification) is `docs/specs/assist.md`.

## E. Focus regions, and keybindings

Focus is one of four **regions** (`dash9_tui::shell::Region` — `Main`
the panel grid/layout/focus area, `Output`, `Log` — plus editing the
command box, layered on top rather than a fourth `Region` variant since
it's orthogonal and always checked first). `Tab`/`Shift+Tab` cycle
forward/backward through all four, a **fixed 4-stop ring regardless of
panel count** — `Main` is *one* stop, not one per panel. This is a
deliberate simplification over an earlier design where `Tab` walked
every panel individually before ever reaching Output/the command box —
on a large, real-world (e.g. imported Grafana) dashboard that meant
dozens of `Tab` presses just to leave the grid. The arrow keys still
step focus one panel at a time, but only mean anything once you're
*on* `Main` — see the table below.

Reaching the command box via `:` doesn't cost you anywhere you were:
`region` is untouched, so cancelling with `Esc` returns to exactly
wherever you were. Reaching it via `Tab` is different, deliberately:
`region` advances to `Main` right then, as part of the ring step, not
left alone the way `:` leaves it. **This was a real bug, not a
hypothetical one**: an earlier version left `region` untouched for
`Tab`-entry too, which meant `Esc` bounced straight back to whichever
region you `Tab`-ed in from (`Log`, the stop right before the command
box, in the common case of `Tab`-ing all the way through) — and since
`Tab` never leaves the command box while editing (only `Esc`/`Enter`
do — see below), the very next `Tab` just re-entered it. `Tab` → `Esc`
→ `Tab` → `Esc` looped between `Log` and the command box forever,
never reaching `Main`/`Output` at all. Setting `region` to `Main`
(the stop right after the command box) when `Tab` lands there fixes
it: `Esc` now resumes the ring's forward progress instead of
dead-ending.

| Key | Effect |
|---|---|
| `:` | Enter the command box (starts an empty buffer) without disturbing `region` underneath. |
| `Esc` | While editing: cancel input, discard the buffer, leave the command box — `region` returns to wherever it was if you got here via `:`, or advances to `Main` if you got here via `Tab` (see above). Else, layered: if the detail pane (Section G.1) is open, close it; only once it's closed does Esc fall through to zoom "home" — one hop to Grid from Layout or Focus (Section G); no-op once at Grid with detail closed. One press always does at most one thing. **Never quits**, regardless of how many times pressed. |
| `Enter` | While editing: submits the buffer, then **immediately reopens an empty one** — the command box never loses focus on its own, so a second (or third...) command can be typed right away without re-pressing `:` or `Tab`-ing back around the ring. An empty/whitespace-only line submits nothing (no log entry, no handler call) but still reopens the same way. `Tab`/`Shift+Tab` (deliberate navigation) or `Esc` (deliberate cancel) are the only ways to actually leave. |
| `Tab` / `Shift+Tab` | Outside the command box: cycle forward/backward through `Main` → `Output` → `Log` → the command box, wrapping — one `Tab` per region, independent of panel count (see above); in `Main` + Grid zoom, also scrolls the viewport to keep the focused panel visible. While editing: **never leaves the command box** — only `Esc`/`Enter` do that — but still cycles which panel is focused underneath (a plain `panel_count()`-stop ring, unrelated to the region ring above), so you can glance at another panel's chart mid-command without losing what you've typed. |
| `→`/`↓`, `←`/`↑` (not editing) | Only while `region == Main`: step focus one panel forward/backward (`↓`/`↑` are flat aliases for `→`/`←` — index-stepping, not 2D spatial navigation), wrapping, in any zoom level, independent of `Tab`'s Main→Output→Log ring. **Replaces the original `1`-`9` direct-select design** (Section G's design review): confirmed live as not actually useful — a 2-digit-plus panel count makes single-digit jump-to unreachable for most panels anyway, and arrow-key stepping plus the zoom bar's paging affordance covers the same need without a second, redundant selection mechanism. |
| `↑` / `↓` | While editing: cycle command history, most recent first; `↓` past the newest clears back to an empty buffer. |
| `PageUp` / `PageDown` | Contextual to the active region (`docs/specs/session-layout.md` Section C, extended by Section F below): scrolls the log while editing, in or out of the command box; scrolls the log's own text when `region == Log`, or the output pane's when `region == Output`; otherwise pages the `Main` grid viewport vertically when zoomed to Grid or Layout (Section G) — Layout only actually moves anything once its dashboard can't shrink to fit, a no-op otherwise, same as it always was for a dashboard small enough to fit either level; a no-op in Focus. Submitting a new command always resets the log scroll to the tail, and any new `Result` line always resets the output pane's scroll to its top (Section F); background poller/assistant results never reset the log, so reading old output there is never interrupted out from under you. |
| `+` / `=` | While `region == Main`: zoom in one level (Section G): Layout → Grid → Focus. No-op already at Focus. While `region == Output`/`Log`: maximize that pane instead — takes over `Main`'s space (Section F). |
| `-` / `_` | While `region == Main`: zoom out one level: Focus → Grid → Layout. No-op already at Layout. While `region == Output`/`Log`: restore the maximized pane to its normal size. |
| `Space` | Outside the command box: toggles the focused panel's detail pane (Section G.1) open/closed. Independent of both zoom and `region` — works, and stays open, whichever of Layout/Grid/Focus is active and whichever region has `Tab`-focus, and pressing it never changes either. While editing, `Space` is a literal character (as it must be — most multi-arg commands need it, e.g. `ds add`). **Was `i`** until this section's region model landed; switched because a plain `i` collided with nothing functionally, but the previous design review flagged it as worth reconsidering once digit keys became region-gated — no other reason. |
| `y` / `n` | Only while a proposal is pending (assist-only, `docs/specs/assist.md`): accept/execute or dismiss the oldest pending proposal. Inert with nothing pending. |
| `a` | Toggle the assistant on/off (assist sessions only; unlike `/ai on`/`/ai off`, this always flips the current state rather than setting an explicit one). |
| `q` | Outside the command box: quit. While editing, a literal character. |
| `Ctrl+C` | Quit, unconditionally — in or out of the command box, checked before any other key handling. Needed because raw terminal mode delivers it as a normal keypress rather than a real `SIGINT`; without this override it would either type a literal `c` or do nothing. |

## F. The command log and the output pane

Every submitted command is appended to the session's log as one
`Command` entry (who sent it — `user` or `assistant` — the verbatim
text, and a millisecond timestamp), followed by one `Result` entry
once a response arrives; background results (panel pollers are not
logged individually, but ad-hoc `q`/`ds metrics` results and assistant
replies are) are appended the same way whenever they complete. The log
is kept bounded (oldest entries drop off past a fixed cap) and is what
`/record` (Section J) mirrors to disk when recording is on —
`Command`/`Result` entries interleaved, unchanged, since `/record`'s
JSONL transcript is meant to be the complete session history.

That combined log is the data model; it is **not** rendered as one
box. Mixing full result text (a `/help` listing, a query result, an
error message) into the same compact strip as command echoes made both
hard to read, so the two are rendered separately:

- **The command log** (`Region::Log`, `dash9_tui::command_bar::draw_log`,
  bottom of the screen, above the input line): `Command` entries only —
  a compact, scrollable history of what was typed and when (`"> /range
  5m"`, `"* panel type gauge"` for an assistant-issued one). `Result`
  entries never appear here. Dynamically sized to its content
  (`command_bar::log_height`, clamped between `MIN_LOG_HEIGHT` and
  `MAX_LOG_HEIGHT` terminal rows, never more than the space available) —
  see below for why this cap exists at all.
- **The output pane** (`Region::Output`, `dash9_tui::output::draw_output`,
  between the main area and the command log — see Section G/H): the
  most recent `Result`'s full text, dynamically sized to its content
  (`output_height`, clamped between `MIN_OUTPUT_HEIGHT` and
  `MAX_OUTPUT_HEIGHT` terminal rows, never more than the space
  available) rather than a fixed size that wastes room on a one-line
  result or crowds out the grid for a long one. Shows `"(no output
  yet)"` before anything has run.

Nothing about the underlying `LogLine`/`ShellState.log` model changed
to support this — it is a rendering-only split, two filtered views onto
the same append-only log.

**The log's height cap is a fix, not just symmetry with output.** An
earlier version gave the whole command bar (log + input line) an
uncapped `Constraint::Min(0)` in `open::draw_session`'s outer layout —
"whatever's left after everything else." On a short dashboard or a tall
terminal, once the grid/detail/output all took only what their content
actually needed, *all* of the difference piled into the log, since
nothing else claimed it: confirmed live, 17 blank rows showing
`"(empty)"` on a 50-row terminal — the least-used pane silently
claiming the most screen space. `log_height` closes this the same way
`output_height` already worked: every pane in `draw_session` is now an
exact `Constraint::Length`, computed up front (`open::pane_heights`),
and genuinely leftover space (everything already capped/sized and still
not filling the terminal) becomes unlabeled blank space at the very
bottom instead of being attributed to any one pane.

**Both panes are focusable and scrollable** — each is a real `Tab`-ring
stop (Section E), not a fixed, unreachable box. When focused, a pane's
border brightens and shows a `"PageUp/PageDown scroll"` hint (the same
shared pane chrome every bordered area uses, `pane::pane_block`,
Section G.2), and `PageUp`/`PageDown` scroll its own text instead of
paging the Grid. Scroll direction differs between the two, deliberately:
the output pane is top-anchored (`ShellState::output_scroll` —
`PageDown` moves further into the content, `PageUp` back toward the
top), since it shows one block of text meant to be read top-to-bottom;
the log is tail-anchored (`ShellState::log_scroll`, unchanged from
before regions existed — `PageUp` grows the offset, walking back from
the newest entry), since it's a running history naturally read
newest-first, the same whether you got there by `region == Log` or by
editing (Section E). Both scroll fields self-clamp against whatever's
actually rendered this frame
(`max_output_scroll`/`command_bar::visible_window`), and `output_scroll`
additionally resets to `0` whenever a new `Result` line arrives
(submitted or background), so a stale scroll position from the
*previous* result is never carried into a new one.

**Maximize/restore** (`ShellState::pane_maximized`): `+`/`=` and `-`/`_`
(Section E) already zoom `Main` through Layout/Grid/Focus; the same two
keys maximize/restore whichever of `Output`/`Log` currently has
`Tab`-focus instead, since `Main` isn't reachable while `region` is
`Output` or `Log` (they're mutually exclusive). `+` on either pane
takes over the space `Main` normally gets — the direct parallel to
`Zoom::Focus` doing that for a single panel — grid and detail both go
to `0` while a pane is maximized (`Main` is fully hidden, not just
shrunk), and the *other* of `Output`/`Log` keeps its own normal small
size, so it stays visible rather than also disappearing. `-` restores
the shared layout. The zoom bar (Section H) reflects this: the region
label gets a `" (maximized)"` suffix and the hint swaps between `"+
maximize"` and `"- restore"`. Tabbing away from a maximized pane
restores it automatically (`shell::ShellState::advance_focus`) — a
maximized pane you've since navigated away from would just be stuck
oversized with nothing on screen explaining why, so any `Tab`/
`Shift+Tab` press clears `pane_maximized` unconditionally, regardless
of which direction or where it lands.

## G. Zoom levels: Layout, Grid, Focus

The main area has three zoom levels (full design in
`docs/specs/session-layout.md` Sections A-D). `+`/`=` and `-`/`_`
(Section E) step one level along the line Layout ↔ Grid ↔ Focus; `Esc`
jumps straight to Grid ("home") from either end, once the detail pane
(Section G.1) — a separate concern from zoom — is closed.

1. **Layout** (`-` from Grid) — every panel, all at once, title-and-
   border only (`dash9_tui::draw_panel_outline`), positioned by
   `dash9_tui::layout::grid_layout_fit` which scales the row-unit
   height down so nothing is clipped or paged — the point is
   confirming the dashboard's *arrangement*, not reading data. Once
   even the row-unit-height floor can't make everything fit (a large
   Grafana import, `docs/specs/grafana-dashboards.md` Section H),
   Layout falls back to the exact same `PageUp`/`PageDown`-driven
   scrolling as Grid (`docs/specs/session-layout.md` Section A.1's
   "Revised after shipping" note) — the "nothing is ever paged"
   property holds only while the dashboard actually fits.
2. **Grid** (the fixed "home" level, `open.md`'s original and still
   default behavior) — real charts at readable size. When the
   dashboard has more panel-rows than the terminal has height for, the
   viewport pages vertically with `PageUp`/`PageDown` (Section E)
   instead of clipping silently; the zoom bar (Section H) shows
   `"panels X-Y of Z — PageDown/PageUp for more"` whenever there's
   more to see.
3. **Focus** (`+` from Grid) — the focused panel's ordinary
   chart/gauge/stat/table rendering, just at full-pane size instead of
   its small grid cell. Nothing more — no config/data overlay here;
   that's the detail pane below, which Focus does not replace or imply.

### G.1. The detail pane

The `Space` key toggles a panel's config+data detail pane open or
closed — entirely independent of zoom (Section G) and of which region
has `Tab`-focus (Section E): it works the same way, and stays open the
same way, whichever of Layout/Grid/Focus is active, and pressing it
never changes which zoom level is active. It
is rendered in its own area **below** the main grid/layout/focus area
(`dash9_tui::draw_panel_detail`, sized by `dash9_tui::detail_height`),
never in place of it — the chart(s) above stay visible and usable the
entire time a panel's detail is open. (An earlier iteration of this
feature made the detail view a `Focus` sub-view that replaced the
whole main area when open, with no visible way back to the charts
short of `Esc`/`-`; this was reworked after exactly that "how do I get
back to the chart" friction confirmed it was the wrong shape.)

The pane has two parts:

1. **Config** — title, panel type, datasource (name and connection),
   query text, `"Panel: N of TOTAL"` (1-indexed position among every
   panel, not raw `SPEC.md` C.1 grid coordinates — an earlier version
   showed `"Grid: row X, col Y, w W, h H"` directly, confirmed live as
   confusing once collapsed-row rebasing on a Grafana import
   (`docs/specs/grafana-dashboards.md` Section H) pushed some panels'
   `row` into the thousands), `allow_empty` and effective latency
   budget, and every configured threshold (`SPEC.md` C.1's
   `[[panels.thresholds]]`, name/op/value).
2. **Data** — every row the panel's last query actually returned,
   rendered as a table through the same `table_for_export` path
   `/save` (Section I) uses, so what you see here and what a
   `/save csv` produces are always the same data. Shows a placeholder
   for "no result yet," an error message for a failed query, or "no
   data" for an empty-but-successful result — never a blank pane.

It has no separate "which panel" state: `Tab`-ing, or stepping focus
with the arrow keys, to a different panel while it's open just follows
the newly focused one,
and a command that affects the focused panel (e.g. `/panel threshold
crit gte 95`) shows its effect immediately, since it redraws from live
session state every frame like everything else.

### G.2. Pane chrome: name, status, hint, and focus color

Every bordered pane (panel charts, Layout's outlines, the detail
pane's config block) shares one border-embedded convention
(`dash9_tui::pane::pane_block`) instead of a separate hint row per
pane — the same density-reducing idea `loremesh-tui` uses (identity +
focus-state live on the border itself, e.g. its `focus_block`), applied
one step further: every corner of the border is meaningful, not just
the title.

- **Top-left: name.** Bold and `theme::FOCUS`-colored when the pane is
  focused; plain `theme::TEXT` otherwise — color is emphasis on top of
  an already-legible name, never the only signal (Mechanism 4).
- **Top-right: status**, when the pane has one — shown uniformly across
  every panel type on purpose, even where the body already conveys the
  same thing (a stat panel's big centered value), rather than some
  panel types having border status and others not: that inconsistency
  read as broken chrome when panels of different types sat side by
  side. Chart, stat, and gauge panels all show the latest value plus
  severity marker/label via the same `status_for` helper, colored with
  `theme::severity_color` (e.g. `0.070 ● ok`, green) — meaningful with
  color stripped, same as everywhere else in this codebase. This is the
  *only* place a gauge panel's severity marker/label appears at all —
  its bar only carries severity as color, which alone isn't
  monochrome-safe (Mechanism 4), so this border status closes a real
  gap there, not just adds uniform styling. A table panel (including
  the detail pane's data sub-block) shows its row count instead.
  Nothing shown at all (not even a placeholder) when there's no data
  yet — the panel body's own "(no data)"/"(loading…)" placeholder
  already says so.
- **Bottom-left: key hint**, shown only on the currently focused pane
  — an unfocused pane's keys don't do anything right now, so it stays
  quiet rather than advertising something inert. A panel's border
  focus (and so this hint) only lights up while `region == Main`
  (Section E) — Tabbing away to Output/Log dims whichever panel was
  highlighted, since only one thing on screen should ever look focused
  at a time. The one genuinely panel-specific action is `Space`
  (`"space detail"`, `dash9_tui::draw::PANEL_HINT`) — broader navigation
  (`Tab`/arrows, `PageUp`/`PageDown`, `+`/`-`) is region-level, not a
  property of any one panel, so it stays in the zoom bar (Section H)
  instead of being repeated on every panel's border; the zoom bar's own
  hint text deliberately excludes `Space` for the same reason, in
  reverse — showing it in both places would just be the same text
  twice on screen at once. The output pane and the log get this same
  bottom-left hint treatment (`"PageUp/PageDown scroll"`) while they
  have `region`'s focus, Section F.
- **Bottom-right:** reserved, not yet used.
- **Border color** itself is the same `theme::FOCUS`/`theme::MUTED`
  split the border color already used before this section existed —
  now applied natively via `Block::border_style` inside each
  panel-type draw function instead of `open.rs` post-hoc recoloring
  already-rendered buffer cells (the earlier approach could only ever
  recolor a border, never add the status/hint content this section
  adds — which needed real parameters on `draw_chart`/`draw_stat`/
  `draw_gauge`/`draw_table`/`draw_panel_outline`, not a buffer patch).
  A third color, `theme::FOCUS_DIM` (plain `Cyan`, not bold), was added
  later for the focused panel while the command box is capturing
  keystrokes: the panel really is still focused (arrows/`Tab` still
  move it), just not where keystrokes land *right now*, and the
  original two-color split made every "hot" pane look identical —
  command box, detail pane, and the focused chart were all bright
  `theme::FOCUS` at once, confirmed live as genuinely confusing ("command
  is selected, detail is selected, and also the chart is selected
  too"). The zoom bar's `" + editing"` label suffix (Section H) is the
  companion fix for the same confusion, named where it's actually read
  rather than only shown as a border color.

Phase 2 (deferred, Section M): a temporary per-pane pop-up showing a
pane's *full* reference on demand, building on this section's hint
text as its content once it exists.

## H. Status bar

A one-line bar above the panel grid: dashboard title, panel count, and
a datasource health marker (`●` healthy, `▲` degraded, `○` unknown —
derived from whether any panel's last result was an error, not a
separate connectivity probe; panels already surface their own errors
inline, this is a summary of that same signal). When assist wiring is
available, it appends: assistant on/off, the active model name, a
short connectivity label (`idle` / `waiting` / `error: ...`), and
cumulative tokens sent/received for the session. The AI segment is
omitted entirely (not shown "off") when the session has no assist
wiring at all — there is nothing configured to toggle.

Directly below it, a second one-line zoom bar
(`dash9_tui::status_bar::draw_zoom_bar`) shows whichever region
currently has `Tab`-focus (Section E) and that region's own key hint —
`docs/specs/session-layout.md` Section D's "per bordered region" key
discoverability, kept as its own small bar rather than folded into the
status bar above since it tracks a different concern (region/keys, not
dashboard/AI state). While `region == Main`, it shows `Main`'s active
zoom level (Layout/Grid/Focus, Section G) instead of the literal word
"Main" — zoom is Main-specific state, so naming the zoom level is more
useful there than a region name that's true of three different views —
and, in Grid or Layout, appends the `"panels X-Y of Z — PageDown/PageUp
for more"` paging indicator whenever the dashboard doesn't fully fit the
viewport — for Layout, only once it's actually fallen back to scrolling
(Section G's revision); a dashboard small enough to shrink-to-fit shows
no suffix, same as it always showed none for Grid when everything
already fit. While `region == Output` or `Log`, it shows that region's
name and its own `"PageUp/PageDown scroll"` hint instead (Section F).
The label itself appends `" + detail"` (e.g. `"[Grid + detail]"`,
`"[Output + detail]"`) whenever the detail pane (Section G.1) is open,
and `" + editing"` (e.g. `"[Grid + editing]"`, `"[Grid + detail +
editing]"`) whenever the command box is capturing keystrokes — **added
after live confirmation that editing state was easy to miss**: several
separate-seeming bug reports ("`PageDown` isn't working," "arrows are
flipping panels," "`Up`/`Down` are captured by the command box") all
traced back to the same real cause — editing changes what several keys
do (Section E's `PageUp`/`PageDown` row, and `Tab`/arrows' own rows
above), but was previously visible only as a small `:` in the command
box, easy to enter (`Tab`-cycling, or a submitted command silently
reopening an empty buffer, Section E's `Enter` row) and easy not to
notice. Naming it right in the bracket label — the first thing on the
line — fixed the discoverability gap the hint text alone didn't.

## I. Panel export (`/save`)

`/save <format> [path]` writes the focused panel's last result to a
file: `csv` (RFC-4180-ish, fields quoted only when they contain a
comma/quote/newline) or `md` (a pipe table). `png` is a recognized,
documented format that always reports unavailable — Ratatui has no
terminal-to-image path, and this is reported honestly rather than
faked or silently downgraded to another format. An unrecognized format
falls through to the same "unknown command" error shape any other bad
`/`-prefixed input gets, rather than a bespoke message. Given no path,
a default path is generated; a path that resolves outside the
workspace root (absolute, `..` traversal, or a symlink escape) is
rejected as `E107` (`SPEC.md` B.5, `docs/specs/assist.md` C.2) — the
same check `dash save`/`dash open` and `/record` (Section J) share.

## J. Continuous log recording (`/record`)

`/record on [path]` opens a file in append mode (never truncate — an
earlier recording at the same path accumulates, it is never
overwritten by restarting) and begins writing one JSON object per log
line (Section F), streamed the instant each line is added. This is
deliberately distinct from `/save`: `/save` is a one-shot snapshot of
one panel's current data; `/record` is a running transcript of the
whole session (queries run, results, help text, assistant replies) —
the motivating use case is building new commands/skills from a
session's history, which needs a parseable sequence you didn't have to
remember to trigger before the interesting part had already scrolled
past. `/record off` stops and reports how many lines were written this
session; bare `/record` reports current on/off status and, when on,
the path and running line count. A path outside the workspace root is
rejected as `E107`, same as Section I.

Each JSONL record is one of:

```json
{"type": "command", "source": "user", "text": "/range 5m", "timestamp_ms": 1700000015000}
{"type": "result", "text": "range set to 5m", "timestamp_ms": 1700000015010}
```

`source` is `"user"` or `"assistant"` (`SPEC.md`-adjacent —
`CommandSource` is a `dash9-core` type shared with the log itself, not
something invented for recording). A `Result` record's timestamp is
recorded at write time, since `LogLine::Result` carries no timestamp
of its own — this is an accurate stand-in, not an approximation of a
missing field. Recording is a no-op when off: every code path that
appends to the log offers those lines to the recorder unconditionally,
and the recorder itself decides whether that's a write or nothing —
callers never branch on "is recording on" first.

## K. Shell command execution (`!`)

A line starting with `!` (Section C) runs the rest of the line through
the user's shell: `$SHELL -c <command>`, falling back to `/bin/sh` if
`$SHELL` isn't set — the same pattern any REPL that shells out uses
(`psql`'s `\!`, `ipython`'s `!`). Runs on a blocking thread
(`tokio::task::spawn_blocking`) so the render loop never stalls
waiting on it; the command bar gets an immediate `"! <command>:
running…"` acknowledgment (`LiveSession::spawn_shell_command`) and the
real result — exit status, stdout, then stderr, trimmed of trailing
newlines — arrives later as a `Result` log line in the output pane
(Section F), the same async-ack shape ad-hoc `q` and `ds metrics`
already use. A signal-terminated process reports "(terminated by
signal)" instead of a fake exit code, since `ExitStatus::code()` itself
returns `None` in that case. A bare `!` (no command text) is rejected
synchronously — nothing to run, no task spawned.

**Deliberately no gating.** No allow-list, no confirmation prompt, no
sandboxing: `dash9 open` is a local dev/ops tool driven by hand, and
`!` is trusted the same as a real shell prompt would be. This is a
different trust model from `dash9-assist`'s command proposals
(`docs/specs/assist.md` Section H's blast-radius classification) on
purpose — the assistant is untrusted, autonomous input that only ever
emits grammar text through the same validated path a human's keystrokes
go through (`docs/specs/assist.md`'s "exactly one effector" invariant);
`!` is not reachable through the assistant's contract loop at all, only
through a human typing at the command bar, so the two never need the
same gating.

## L. Mouse selection and clipboard copy

`dash9 open` enables mouse capture (`crossterm::event::EnableMouseCapture`,
layered onto `ratatui::init()`'s raw-mode/alternate-screen setup in
`open::shell_loop`) so it can draw its own left-button drag-to-select
instead of relying on the terminal's/tmux's native selection. This is
a deliberate choice, not decoration: `dash9 open` redraws live-
refreshing panel data continuously, and a terminal's or tmux's own
click-drag selection tracks *screen cells*, not the text that was
there when you started dragging — a panel refresh mid-drag silently
changes what's under the selection or breaks it outright. Owning
selection end-to-end sidesteps that: dash9 knows exactly what it drew
and when.

- **Drag** (`crate::selection::Selection`, screen cell coordinates —
  the same space `MouseEvent::{column,row}` reports): `Down` starts a
  fresh selection at that cell (replacing any previous one, same as a
  new click in a real terminal); `Drag` extends it; `Up` finalizes it.
  A `Down`/`Up` at the same cell with no drag between (a plain click)
  clears any existing selection instead of "selecting" one character.
  Every other mouse event (scroll, right/middle click, plain move) is
  a deliberate no-op — nothing else has a binding yet.
- **Highlight**: reverse-video (`Modifier::REVERSED`) applied to every
  selected cell each frame the selection is active
  (`Selection::highlight`, rendered via a `Widget for &Selection` impl
  since `ratatui::Frame::buffer` is crate-private — a widget is the
  only way application code reaches a `&mut Buffer` to paint into).
  Style-only; cell content is untouched, so what gets highlighted is
  exactly what `extract_text` later reads.
- **Extraction** (`Selection::extract_text`) is reading-order (linear)
  text selection, not a rectangular block: the first row runs from the
  selection's start column to the row's right edge, the last row from
  the row's left edge to the end column, full rows in between — matching
  how terminal-native click-drag selection reads, not tmux's rectangle-
  toggle mode. Each line is trimmed of trailing whitespace (buffer cells
  pad unused width with spaces) before a multi-row selection is joined
  with `\n`. Reads from the most recently rendered frame's buffer
  (`Terminal::draw`'s returned `CompletedFrame::buffer`, cloned into
  `shell_loop`'s `last_buffer` after every draw) — exactly what was on
  screen when the button was released, never a re-render.
- **Copy** (`crate::selection::copy_to_clipboard`) writes a bare OSC 52
  escape (`\x1b]52;c;<base64>\x07`) straight to stdout on `Up`, for any
  selection covering more than one cell — **never wrapped** in tmux's
  DCS passthrough, even when `$TMUX` is set. An earlier version did
  wrap it there, on the assumption tmux needed help forwarding the
  sequence; that was wrong and actively broke copying under tmux's
  default config: DCS passthrough is gated behind `allow-passthrough`,
  which defaults to **off**, so the wrapped sequence was silently
  dropped (confirmed live: wrapped writes produced no tmux paste
  buffer; the identical bare sequence did). tmux recognizes and handles
  bare OSC 52 natively — that's what `set-clipboard` (on by default)
  configures — so sending it unwrapped is what actually works, both
  inside and outside tmux, without depending on a tmux option dash9
  doesn't control and can't assume is set. A copy failure (e.g. the
  terminal doesn't support OSC 52 at all) is silently ignored — the
  highlight and the text underneath it are still correct, only the
  clipboard hand-off may not land; **holding Shift while dragging**
  bypasses dash9's mouse capture entirely in most terminals (iTerm2,
  GNOME Terminal, Alacritty, kitty, Windows Terminal) and falls back to
  the terminal's own native selection, same as with any full-screen
  terminal app that captures the mouse.
- Mouse capture is disabled on exit and on panic alike — `shell_loop`
  wraps whatever panic hook `ratatui::init()` installed with one that
  also disables mouse capture first, so a crash never leaves the
  terminal with mouse reporting stuck on.

Any keypress dismisses a lingering post-copy selection highlight
(typing means you've moved on from what you just selected) — `Down`
starting a fresh drag does too, implicitly, since it always replaces
`selection` outright.

## M. Non-goals

- **No configurable keybindings.** Every binding in Section E is fixed
  for v1; remapping is not implemented.
- **No persistent command history across sessions.** History
  (`↑`/`↓`, Section E) lives only for the lifetime of one `dash9 open`
  process.
- **No PNG export.** See Section I — reported unavailable, not planned
  for this phase.
- **No scrolling within a single Focus panel or the detail pane.**
  `PageUp`/`PageDown` are reserved but a no-op in Focus (Section E/G,
  `docs/specs/session-layout.md` Section C); the detail pane's data
  table (Section G.1) is likewise not scrollable — for a panel's own
  long content to eventually scroll, not built now.
- **No multi-session / multi-window support.** One `dash9 open`
  process is one session against one dashboard file at a time (`dash
  open` inside the grammar replaces the current session; it does not
  open a second one alongside it).
- **No per-pane temporary help pop-up.** Section G.2's border-embedded
  name/status/hint quadrants (Phase 1) are built; a follow-up
  ("Phase 2") — a dismissable overlay showing one pane's *full*
  reference on demand (not just its one-line footer hint) — is a
  deliberately separate, deferred piece. Tracked here explicitly so it
  isn't lost: it needs a genuinely new mechanism neither this codebase
  nor `loremesh` has prior art for (a temporary/dismissable overlay
  state — everything else in `dash9 open` is either permanent chrome
  or driven straight by live session state, never a transient UI
  layer), and makes the most sense to build once Phase 1's per-pane
  hint text already exists to reuse/expand into the pop-up's content.
- **No shell command gating.** Section K's `!` runs with no allow-list,
  confirmation, or sandboxing — deliberate, see Section K.
- **No stdin piping into `!` commands, no interactivity, no timeout.**
  A shell command runs to completion and its full output appears at
  once; there's no way to feed it input, attach to a long-running/
  interactive process (`vim`, `top`), or cap how long it can run.
- **No selection beyond one screen's cells.** Section L's selection is
  screen-coordinate state with no notion of scrollback — it cannot
  reach content that has scrolled out of the log/output pane, only
  whatever is currently rendered on screen.
- **No block/rectangular selection mode.** Section L's drag-select is
  always reading-order (linear) text selection; there's no toggle for
  tmux-style rectangular selection.
