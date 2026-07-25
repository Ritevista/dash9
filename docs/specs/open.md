# `dash9 open` — the interactive session — docs/specs/open.md

`dash9 open <path>` is the live, interactive multi-panel viewer: a
panel grid, a scrollable session log, and a command bar, all driven by
the one command grammar `SPEC.md` Section B defines. This document is
the single source of truth for everything `open` adds on top of that
grammar — the shell meta-commands, keybindings, the detail-view
overlay, the status bar, and continuous log recording — none of which
is part of SPEC.md's append-only grammar (Section B.1) because it is
shell/UI behavior, not something a dashboard TOML file or a scripted
`dash9 test` run ever needs to express.

Status: **Accepted** — every behavior below is implemented and has
unit or integration coverage (`crates/dash9-tui/src/shell.rs`,
`crates/dash9-tui/src/{detail_view,status_bar,command_bar}.rs`,
`crates/dash9/src/{open,live_session,log_recorder}.rs`). Prerequisites:
`SPEC.md` (grammar, error codes, dashboard schema), `docs/architecture/rendering.md`.
This spec does not cover the `--assist` flag's AI behavior (context
assembly, the contract loop, the LLM client) — that is
[`docs/specs/assist.md`](assist.md); it does cover the parts of the
interactive session every `dash9 open` invocation has regardless of
`--assist` (Section H below is the one exception, since the `/ai`
verbs and the assist status-bar segment only do anything when the
session was built with assist wiring).

## Contents

- [A. Invocation](#a-invocation)
- [B. Session model](#b-session-model)
- [C. Input routing](#c-input-routing)
- [D. Shell meta-commands](#d-shell-meta-commands)
- [E. Keybindings](#e-keybindings)
- [F. The command log](#f-the-command-log)
- [G. The detail view](#g-the-detail-view)
- [H. Status bar](#h-status-bar)
- [I. Panel export (`/save`)](#i-panel-export-save)
- [J. Continuous log recording (`/record`)](#j-continuous-log-recording-record)
- [K. Non-goals](#k-non-goals)

---

## A. Invocation

```
dash9 open <path> [--assist]
```

`<path>` is a dashboard TOML file (`SPEC.md` Section C); it is loaded
and validated exactly as `dash9 test` loads one (`SPEC.md` C.3 step 1)
— an invalid file is a startup error, not something the session
recovers from interactively. `--assist` enables natural-language input
alongside the command grammar, backed by an OpenAI-compatible endpoint
configured at `~/.config/dash9/assist.toml` (`docs/specs/assist.md`
Section D); it requires the `assist` Cargo feature (on by default). If
`dash9` was built with `--no-default-features` (dropping `assist`),
passing `--assist` at the CLI is a startup error naming the rebuild
command needed, not a silent downgrade to grammar-only mode.

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
which both the panel grid (small chart box) and the detail view
(Section G, full data table) render from; there is exactly one source
of truth for "what did this panel's query last return," not a
separate copy for each view.

## C. Input routing

The command bar has exactly one discriminator for what a submitted
line means, decided before anything else: a leading `/`.

- A line starting with `/` is always a **structured command**: a shell
  meta-command (Section D) or `SPEC.md` grammar via `dash9_core::parse`.
  Text after `/` that matches neither is a hard error (`E002`/`E003`,
  `SPEC.md` B.5) — it is never silently retried as natural language.
- A line with **no** leading `/` is always **natural language**,
  unconditionally — even if it happens to parse as valid grammar
  (`range 5m` with no `/` is sent as natural language, not executed).
  It is handed to the assistant only when the session has `--assist`
  and the assistant is currently on (Section D); otherwise it is
  reported as unavailable.

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
session was built with `--assist`; a plain `dash9 open` (no
`--assist`) still recognizes them but reports the assistant as
unavailable rather than treating them as unknown commands. Full
assistant behavior (context assembly, the contract loop, proposal
classification) is `docs/specs/assist.md`.

## E. Keybindings

| Key | Effect |
|---|---|
| `:` | Enter the command box (starts an empty buffer). |
| `Esc` | While editing: cancel input, discard the buffer. Else, if the detail view is open: close it. Else: no-op. **Never quits**, regardless of how many times pressed. |
| `Enter` | While editing: submit the buffer (no-op on an empty/whitespace-only line — nothing is logged and no handler call happens). |
| `Tab` / `Shift+Tab` | Outside the command box: cycle focus forward/backward around every panel, then the command box itself, then wrap (`panel_count() + 1` stops total). **Inert while editing** — once inside the command box composing text, `Tab`/`Shift+Tab` do nothing, so a stray press never silently discards an in-progress command by navigating away; only `Esc` or `Enter` leave the box. |
| `↑` / `↓` | While editing: cycle command history, most recent first; `↓` past the newest clears back to an empty buffer. |
| `PageUp` / `PageDown` | Scroll the log up/down by a fixed step, in or out of the command box, without touching the input buffer. Submitting a new command always resets scroll to the tail; background results (poller updates, assistant replies) never do, so reading old output is never interrupted out from under you. |
| `i` | Outside the command box: toggle the focused panel's full-screen detail view (Section G). While editing, `i` is a literal character. |
| `y` / `n` | Only while a proposal is pending (assist-only, `docs/specs/assist.md`): accept/execute or dismiss the oldest pending proposal. Inert with nothing pending. |
| `a` | Toggle the assistant on/off (assist sessions only; unlike `/ai on`/`/ai off`, this always flips the current state rather than setting an explicit one). |
| `q` | Outside the command box: quit. While editing, a literal character. |
| `Ctrl+C` | Quit, unconditionally — in or out of the command box, checked before any other key handling. Needed because raw terminal mode delivers it as a normal keypress rather than a real `SIGINT`; without this override it would either type a literal `c` or do nothing. |

## F. The command log

Every submitted command is appended to the log as one `Command` entry
(who sent it — `user` or `assistant` — the verbatim text, and a
millisecond timestamp), followed by one `Result` entry once a response
arrives; background results (panel pollers are not logged individually,
but ad-hoc `q`/`ds metrics` results and assistant replies are) are
appended the same way whenever they complete. The log is kept bounded
(oldest entries drop off past a fixed cap) and is what `/record`
(Section J) mirrors to disk when recording is on.

## G. The detail view

The `i` key toggles a full-screen overlay for whichever panel is
currently focused, replacing only the panel-grid area — the log and
command bar stay visible and usable underneath, so a command that
affects the focused panel (e.g. `/panel threshold crit gte 95`) shows
its effect in the overlay immediately, since it redraws from live
session state every frame like everything else. It has no separate
"which panel" state: Tab-ing to a different panel while it's open
just follows the newly focused one.

The overlay has two parts:

1. **Config** — title, panel type, datasource (name and connection),
   query text, grid position (`SPEC.md` C.1), `allow_empty` and
   effective latency budget, and every configured threshold
   (`SPEC.md` C.1's `[[panels.thresholds]]`, name/op/value).
2. **Data** — every row the panel's last query actually returned,
   rendered as a table through the same `table_for_export` path
   `/save` (Section I) uses, so what you see here and what a
   `/save csv` produces are always the same data. Shows a placeholder
   for "no result yet," an error message for a failed query, or "no
   data" for an empty-but-successful result — never a blank pane.

## H. Status bar

A one-line bar above the panel grid: dashboard title, panel count, and
a datasource health marker (`●` healthy, `▲` degraded, `○` unknown —
derived from whether any panel's last result was an error, not a
separate connectivity probe; panels already surface their own errors
inline, this is a summary of that same signal). When built with
`--assist`, it appends: assistant on/off, the active model name, a
short connectivity label (`idle` / `waiting` / `error: ...`), and
cumulative tokens sent/received for the session. The AI segment is
omitted entirely (not shown "off") when the session has no assist
wiring at all — there is nothing configured to toggle.

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

## K. Non-goals

- **No configurable keybindings.** Every binding in Section E is fixed
  for v1; remapping is not implemented.
- **No persistent command history across sessions.** History
  (`↑`/`↓`, Section E) lives only for the lifetime of one `dash9 open`
  process.
- **No PNG export.** See Section I — reported unavailable, not planned
  for this phase.
- **No multi-session / multi-window support.** One `dash9 open`
  process is one session against one dashboard file at a time (`dash
  open` inside the grammar replaces the current session; it does not
  open a second one alongside it).
