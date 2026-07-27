//! Grafana dashboard JSON import. See `docs/specs/grafana-dashboards.md`.
//!
//! This is a *read* path only — parsing real, exported Grafana JSON
//! into the same [`ValidatedDashboard`]/[`ValidatedPanel`] model a
//! TOML dashboard produces, so every existing renderer/poller/`dash9
//! test` code path handles a Grafana-sourced dashboard for free.
//! Losslessly preserving every field for re-export (Section A) is a
//! separate, not-yet-built piece; this module never writes JSON back
//! out.
//!
//! Grounded against a real dashboard (Grafana.com "Node Exporter
//! Full", ID 1860, schemaVersion 41), not just the spec's worked
//! example, which surfaced three things the spec didn't anticipate:
//!
//! - **Row panels.** An expanded row's children are flat siblings in
//!   `panels[]`, positioned by their own `gridPos`; a *collapsed*
//!   row's children are nested inside the row's own `panels[]`
//!   instead. [`flatten_panels`] handles both; the row entries
//!   themselves carry no query and are dropped, not imported as
//!   panels.
//! - **Grafana build-in variables** (`$__rate_interval`,
//!   `$__interval`, ...) never appear in `templating.list[]`, so
//!   Section E's "substitute `current.value`" rule has nothing to
//!   substitute. dash9 fills in exactly one, `$__rate_interval`,
//!   using Grafana's own documented default (see
//!   [`RATE_INTERVAL_DEFAULT`]); every other builtin is left
//!   unresolved like any other missing variable, not guessed.
//! - **Variables exported with no saved value.** A dashboard shared on
//!   grafana.com (rather than exported live from a running instance)
//!   normally has `current: {}` for every variable — there is no
//!   value to substitute, and dash9 has no variable runtime to ask for
//!   one live (Section E's non-goal). A panel whose query still
//!   contains an unresolved `$name` after substitution is imported
//!   preserved-but-inert (`ValidatedPanel::executable = false`) —
//!   visible, positioned, never queried — the same treatment Section B
//!   gives a panel with an unsupported datasource type, not a guess.

use std::collections::HashMap;
use std::path::Path;

use serde_json::Value;

use crate::dashboard::{
    e006, DatasourceType, GridSpec, PanelType, ThresholdOp, ValidatedDashboard,
    ValidatedDatasource, ValidatedPanel, ValidatedThreshold, DEFAULT_TEST_LATENCY_BUDGET,
};
use crate::duration::{Duration, DurationUnit, RefreshInterval};
use crate::error::CommandError;

/// Grafana's own documented floor for `$__rate_interval` when no
/// panel-specific interval is known: `max($__interval +
/// scrape_interval, 4 * scrape_interval)`, evaluated at Grafana's
/// documented default `scrape_interval` of 15s — 60s. The one built-in
/// intrinsic dash9 fills in; every other `$__`-prefixed Grafana global
/// (`$__interval`, `$__range`, ...) is left unresolved like any other
/// variable with no value, not guessed.
const RATE_INTERVAL_DEFAULT: &str = "1m";

/// Which loader `dash9 open`/`dash9 test` should use for a dashboard
/// file — detected from the file itself, never a separate flag/verb
/// (`docs/specs/grafana-dashboards.md` Section F, "Decided").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DashboardFormat {
    Toml,
    Json,
}

/// Detects [`DashboardFormat`] from `path`'s extension, content-sniffed
/// from `contents` when the extension is missing or unrecognized.
pub fn detect_dashboard_format(path: &Path, contents: &str) -> DashboardFormat {
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) if ext.eq_ignore_ascii_case("json") => DashboardFormat::Json,
        Some(ext) if ext.eq_ignore_ascii_case("toml") => DashboardFormat::Toml,
        _ => {
            if contents.trim_start().starts_with('{') {
                DashboardFormat::Json
            } else {
                DashboardFormat::Toml
            }
        }
    }
}

/// Reads and parses a Grafana dashboard JSON file. `prometheus_url` is
/// where every Prometheus-typed panel's datasource resolves to — a
/// Grafana export only ever carries an internal `uid` (or, exported
/// from the public dashboard library, an unresolved `${variable}`
/// reference to one), never a queryable URL, so dash9 has to be told
/// one (`docs/specs/grafana-dashboards.md` Section D's "prompts for
/// one on first import rather than guessing," implemented here as an
/// explicit input rather than an interactive dialog).
pub fn load_grafana_path(
    path: &Path,
    prometheus_url: &str,
) -> Result<ValidatedDashboard, CommandError> {
    let source = std::fs::read_to_string(path).map_err(|e| {
        CommandError::new(
            crate::error::ErrorCode::E104,
            format!("cannot read {}: {e}", path.display()),
            None,
        )
    })?;
    parse_grafana_json(&source, prometheus_url)
}

/// Parses Grafana dashboard JSON text into a [`ValidatedDashboard`].
/// Every panel dash9 can fully resolve (recognized panel type,
/// Prometheus datasource, every `$variable` in its query substituted)
/// comes back `executable: true`, ready for the same live poller every
/// TOML-sourced panel uses; everything else comes back
/// `executable: false` with a human-readable `inert_reason`, still
/// positioned in the grid (module docs).
pub fn parse_grafana_json(
    text: &str,
    prometheus_url: &str,
) -> Result<ValidatedDashboard, CommandError> {
    let root: Value = serde_json::from_str(text)
        .map_err(|e| e006(format!("invalid Grafana dashboard JSON: {e}")))?;

    let title = root
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("Untitled Dashboard")
        .to_string();
    let refresh = parse_refresh(root.get("refresh"))?;
    let default_range = parse_default_range(root.get("time"))?;
    let test_latency_budget = Duration::parse(DEFAULT_TEST_LATENCY_BUDGET).unwrap_or(Duration {
        magnitude: 5,
        unit: DurationUnit::Seconds,
    });

    let vars = build_variable_map(root.get("templating"));

    let raw_panels = root
        .get("panels")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let panels: Vec<ValidatedPanel> = flatten_panels(&raw_panels)
        .into_iter()
        .map(|raw| import_panel(raw, &vars))
        .collect();

    let datasources = vec![ValidatedDatasource {
        name: "prom".to_string(),
        datasource_type: DatasourceType::Prometheus,
        url: prometheus_url.to_string(),
    }];

    Ok(ValidatedDashboard {
        title,
        refresh,
        default_range,
        test_latency_budget,
        datasources,
        panels,
    })
}

fn parse_refresh(value: Option<&Value>) -> Result<RefreshInterval, CommandError> {
    match value {
        None | Some(Value::Null | Value::Bool(false)) => Ok(RefreshInterval::Off),
        Some(Value::String(s)) if s.is_empty() => Ok(RefreshInterval::Off),
        Some(Value::String(s)) => Duration::parse(s)
            .map(RefreshInterval::Duration)
            .map_err(|_| {
                e006(format!(
                    "refresh: \"{s}\" is not a valid duration or \"off\""
                ))
            }),
        Some(other) => Err(e006(format!("refresh: unexpected value {other}"))),
    }
}

/// Only the common `"now-<duration>"`/`"now"` shape maps to a single
/// `default_range` duration; an absolute or non-`now`-relative range
/// has no dash9 equivalent and is rejected with a clear error, not
/// silently approximated (`docs/specs/grafana-dashboards.md` Section
/// D). A dashboard with no `time` block at all defaults to `1h`,
/// dash9's own worked-example default (SPEC.md C.2).
fn parse_default_range(value: Option<&Value>) -> Result<Duration, CommandError> {
    let Some(time) = value else {
        return Ok(Duration {
            magnitude: 1,
            unit: DurationUnit::Hours,
        });
    };
    let from = time.get("from").and_then(Value::as_str).unwrap_or("now-1h");
    let to = time.get("to").and_then(Value::as_str).unwrap_or("now");
    if to != "now" {
        return Err(e006(format!(
            "time.to: \"{to}\" is not \"now\" — absolute/non-\"now\" ranges have no dash9 equivalent"
        )));
    }
    let Some(duration_str) = from.strip_prefix("now-") else {
        return Err(e006(format!(
            "time.from: \"{from}\" is not \"now-<duration>\" — absolute/non-\"now\"-relative ranges have no dash9 equivalent"
        )));
    };
    Duration::parse(duration_str)
        .map_err(|_| e006(format!("time.from: \"{from}\" duration is not valid")))
}

/// Builds the `$name`/`${name}` substitution table from
/// `templating.list[]`: only variables with a non-empty `current.value`
/// (module docs — usually none, for a dashboard shared rather than
/// exported live) plus the one documented built-in
/// (`RATE_INTERVAL_DEFAULT`). A `type: "datasource"` variable is
/// skipped here — its uid is resolved per-panel by `datasource.type`
/// (`panel_datasource_is_prometheus`), not substituted into query
/// text, since dash9 only ever has one Prometheus datasource to offer
/// regardless of which uid a Grafana export names.
fn build_variable_map(templating: Option<&Value>) -> HashMap<String, String> {
    let mut vars = HashMap::new();
    vars.insert(
        "__rate_interval".to_string(),
        RATE_INTERVAL_DEFAULT.to_string(),
    );

    let Some(list) = templating
        .and_then(|t| t.get("list"))
        .and_then(Value::as_array)
    else {
        return vars;
    };
    for var in list {
        let Some(name) = var.get("name").and_then(Value::as_str) else {
            continue;
        };
        if var.get("type").and_then(Value::as_str) == Some("datasource") {
            continue;
        }
        if let Some(value) = current_value_as_string(var.get("current")) {
            vars.insert(name.to_string(), value);
        }
    }
    vars
}

fn current_value_as_string(current: Option<&Value>) -> Option<String> {
    let value = current?.get("value")?;
    match value {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Array(items) => {
            let joined: Vec<&str> = items.iter().filter_map(Value::as_str).collect();
            if joined.is_empty() {
                None
            } else {
                Some(joined.join("|"))
            }
        }
        _ => None,
    }
}

/// Flattens Grafana's row structure into a plain panel list: an
/// expanded row's children are already flat siblings (pushed via the
/// `else` branch when their own turn comes); a *collapsed* row's
/// children live nested inside the row's own `panels[]` and are
/// spliced in here. Either way the row entry itself carries no query
/// and is dropped, not imported as a panel (module docs).
fn flatten_panels(raw_panels: &[Value]) -> Vec<&Value> {
    let mut out = Vec::new();
    for p in raw_panels {
        if p.get("type").and_then(Value::as_str) == Some("row") {
            if let Some(nested) = p.get("panels").and_then(Value::as_array) {
                out.extend(nested.iter());
            }
        } else {
            out.push(p);
        }
    }
    out
}

fn import_panel(raw: &Value, vars: &HashMap<String, String>) -> ValidatedPanel {
    let title = raw
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("Untitled Panel")
        .to_string();
    let grafana_type = raw.get("type").and_then(Value::as_str).unwrap_or("");
    let grid = parse_grid_pos(raw.get("gridPos"));
    let thresholds = parse_thresholds(raw.get("fieldConfig"));
    let panel_type = PanelType::parse(grafana_type);
    let datasource_is_prometheus = panel_datasource_is_prometheus(raw);
    // Only the first target executes; a multi-target panel's
    // remaining queries have no home in dash9's one-query-per-panel
    // schema (SPEC.md C.1) — a full accounting of what's dropped is
    // deferred to the not-yet-built export/round-trip path (module
    // docs), not silently claimed as complete here.
    let expr = raw
        .get("targets")
        .and_then(Value::as_array)
        .and_then(|targets| targets.first())
        .and_then(|t| t.get("expr"))
        .and_then(Value::as_str);

    let (executable, query, datasource, inert_reason) = match (panel_type, datasource_is_prometheus, expr) {
        (None, _, _) => (
            false,
            String::new(),
            String::new(),
            Some(format!(
                "Grafana panel type \"{grafana_type}\" has no dash9 equivalent (dash9 renders timeseries, gauge, table, stat)"
            )),
        ),
        (Some(_), false, _) => (
            false,
            String::new(),
            String::new(),
            Some("panel's datasource is not Prometheus (or could not be resolved)".to_string()),
        ),
        (Some(_), true, None) => (
            false,
            String::new(),
            String::new(),
            Some("panel has no query target".to_string()),
        ),
        (Some(_), true, Some(raw_expr)) => {
            let (substituted, unresolved) = substitute_variables(raw_expr, vars);
            if unresolved.is_empty() {
                (true, substituted, "prom".to_string(), None)
            } else {
                (
                    false,
                    substituted,
                    String::new(),
                    Some(format!(
                        "unresolved variable(s) with no value in this dashboard export: {}",
                        unresolved.join(", ")
                    )),
                )
            }
        }
    };

    ValidatedPanel {
        title,
        panel_type: panel_type.unwrap_or(PanelType::Stat),
        datasource,
        query,
        // Unlike a hand-authored TOML panel (where empty data usually
        // signals something's actually wrong, so `false` is the right
        // strict default), a general-purpose community dashboard like
        // Grafana's own "Node Exporter Full" covers many *optional*
        // exporter collectors no single environment enables all of —
        // e.g. `node_tcp_connection_states` needs node_exporter's
        // tcpstat collector, off by default. Treating that as a hard
        // `dash9 test` failure would flag a completely normal
        // deployment difference the same way as a genuinely broken
        // query. `true` here only changes the pass/fail verdict for
        // *imported* panels; TOML dashboards keep their strict default
        // (`dashboard.rs`'s `PanelSpec::allow_empty` still defaults to
        // `false`).
        allow_empty: true,
        latency_budget: None,
        grid,
        thresholds,
        executable,
        inert_reason,
    }
}

fn panel_datasource_is_prometheus(raw: &Value) -> bool {
    match raw.get("datasource") {
        Some(Value::Object(map)) => map.get("type").and_then(Value::as_str) == Some("prometheus"),
        // Pre-Grafana-8 bare-string datasource reference: no type
        // information available, not assumed to be Prometheus.
        _ => false,
    }
}

fn parse_grid_pos(grid_pos: Option<&Value>) -> GridSpec {
    let get = |key: &str| -> u32 {
        grid_pos
            .and_then(|g| g.get(key))
            .and_then(Value::as_u64)
            .and_then(|v| u32::try_from(v).ok())
            .unwrap_or(0)
    };
    GridSpec {
        row: get("y"),
        col: get("x"),
        w: get("w").max(1),
        h: get("h").max(1),
    }
}

/// Maps `fieldConfig.defaults.thresholds.steps` (`docs/specs/
/// grafana-dashboards.md` Section D). Two things the spec's worked
/// example didn't show, both real on the dashboard this was grounded
/// against: a step with no `value` at all (not even `null`) is
/// Grafana's "base" color for everything below the first real
/// threshold — skipped, since dash9's threshold model has no
/// unconditional-base concept, only concrete `gte` comparisons; and
/// `mode: "percentage"` steps are relative to the panel's configured
/// min/max, not absolute metric values — mapping those numbers
/// straight across would produce a threshold that looks valid but
/// compares on the wrong scale, so a non-`"absolute"` mode drops the
/// panel's thresholds entirely rather than misapplying them.
fn parse_thresholds(field_config: Option<&Value>) -> Vec<ValidatedThreshold> {
    let Some(thresholds) = field_config
        .and_then(|f| f.get("defaults"))
        .and_then(|d| d.get("thresholds"))
    else {
        return vec![];
    };
    let mode = thresholds
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("absolute");
    if mode != "absolute" {
        return vec![];
    }
    let Some(steps) = thresholds.get("steps").and_then(Value::as_array) else {
        return vec![];
    };

    let mut seen_names: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for step in steps {
        let Some(value) = step.get("value").and_then(Value::as_f64) else {
            continue;
        };
        // Grafana's `color` is a free-form CSS color, not always a
        // named one — an `rgba(245, 54, 54, 0.9)` string is common and
        // real (seen on the dashboard this was grounded against), and
        // makes an unreadable threshold name if used verbatim. Only a
        // plain word (`"red"`, `"dark-yellow"`) is used as-is;
        // anything else falls back to the generic name below.
        let color = step.get("color").and_then(Value::as_str).unwrap_or("");
        let is_plain_word = !color.is_empty()
            && color
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        let base = if is_plain_word { color } else { "threshold" };
        let mut name = base.to_string();
        let mut n = 1;
        while seen_names.contains(&name) {
            n += 1;
            name = format!("{base}-{n}");
        }
        seen_names.push(name.clone());
        out.push(ValidatedThreshold {
            name,
            op: ThresholdOp::Gte,
            value,
        });
    }
    out
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Substitutes `$name`/`${name}`/`${name:format}` references found in
/// `vars`; anything not found is left as a literal `$name`/`${name}`
/// token in the output, and its bare name is collected into the
/// returned `Vec` (deduplicated, first-seen order) so the caller can
/// tell a fully-resolved query from one still carrying unresolved
/// variables (`import_panel`).
fn substitute_variables(text: &str, vars: &HashMap<String, String>) -> (String, Vec<String>) {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut unresolved: Vec<String> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '$' && chars.get(i + 1) == Some(&'{') {
            if let Some(close_rel) = chars[i + 2..].iter().position(|&c| c == '}') {
                let inner: String = chars[i + 2..i + 2 + close_rel].iter().collect();
                let name = inner.split(':').next().unwrap_or(&inner).to_string();
                if !name.is_empty() && name.chars().all(is_ident_char) {
                    if let Some(v) = vars.get(&name) {
                        out.push_str(v);
                    } else {
                        out.push_str("${");
                        out.push_str(&inner);
                        out.push('}');
                        if !unresolved.contains(&name) {
                            unresolved.push(name);
                        }
                    }
                    i += 2 + close_rel + 1;
                    continue;
                }
            }
            out.push('$');
            i += 1;
        } else if chars[i] == '$' && chars.get(i + 1).is_some_and(|&c| is_ident_start(c)) {
            let start = i + 1;
            let mut end = start;
            while end < chars.len() && is_ident_char(chars[end]) {
                end += 1;
            }
            let name: String = chars[start..end].iter().collect();
            if let Some(v) = vars.get(&name) {
                out.push_str(v);
            } else {
                out.push('$');
                out.push_str(&name);
                if !unresolved.contains(&name) {
                    unresolved.push(name);
                }
            }
            i = end;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    (out, unresolved)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_dashboard(panels_json: &str) -> String {
        format!(
            r#"{{
                "title": "Test",
                "refresh": "30s",
                "time": {{"from": "now-1h", "to": "now"}},
                "panels": [{panels_json}],
                "templating": {{"list": []}}
            }}"#
        )
    }

    #[test]
    fn detects_format_by_extension() {
        assert_eq!(
            detect_dashboard_format(Path::new("d.json"), ""),
            DashboardFormat::Json
        );
        assert_eq!(
            detect_dashboard_format(Path::new("d.toml"), ""),
            DashboardFormat::Toml
        );
    }

    #[test]
    fn detects_format_by_content_when_extension_is_ambiguous() {
        assert_eq!(
            detect_dashboard_format(Path::new("d"), "  {\"title\": \"x\"}"),
            DashboardFormat::Json
        );
        assert_eq!(
            detect_dashboard_format(Path::new("d"), "[dashboard]\ntitle=\"x\""),
            DashboardFormat::Toml
        );
    }

    #[test]
    fn malformed_json_is_e006() {
        let err = parse_grafana_json("not json", "http://localhost:9090").unwrap_err();
        assert_eq!(err.code, crate::error::ErrorCode::E006);
    }

    #[test]
    fn a_fully_static_query_imports_as_executable() {
        let json = minimal_dashboard(
            r#"{
                "title": "Load",
                "type": "stat",
                "gridPos": {"x": 0, "y": 0, "w": 6, "h": 4},
                "datasource": {"type": "prometheus", "uid": "abc"},
                "targets": [{"expr": "node_load1", "refId": "A"}]
            }"#,
        );
        let dashboard = parse_grafana_json(&json, "http://localhost:9090").unwrap();
        assert_eq!(dashboard.title, "Test");
        assert_eq!(dashboard.panels.len(), 1);
        let panel = &dashboard.panels[0];
        assert!(panel.executable);
        assert_eq!(panel.query, "node_load1");
        assert_eq!(panel.datasource, "prom");
        assert!(
            panel.allow_empty,
            "imported panels default to allow_empty=true — a community dashboard covers optional collectors no single environment enables all of"
        );
        assert_eq!(
            panel.grid,
            GridSpec {
                row: 0,
                col: 0,
                w: 6,
                h: 4
            }
        );
    }

    #[test]
    fn an_unresolved_variable_imports_as_preserved_but_inert() {
        let json = minimal_dashboard(
            r#"{
                "title": "CPU",
                "type": "timeseries",
                "gridPos": {"x": 0, "y": 0, "w": 12, "h": 8},
                "datasource": {"type": "prometheus", "uid": "${ds_prometheus}"},
                "targets": [{"expr": "rate(node_cpu_seconds_total{job=\"$job\"}[$__rate_interval])", "refId": "A"}]
            }"#,
        );
        let dashboard = parse_grafana_json(&json, "http://localhost:9090").unwrap();
        let panel = &dashboard.panels[0];
        assert!(!panel.executable);
        assert!(
            panel.inert_reason.as_deref().unwrap().contains("job"),
            "reason should name the unresolved variable: {:?}",
            panel.inert_reason
        );
        assert!(
            !panel
                .inert_reason
                .as_deref()
                .unwrap()
                .contains("__rate_interval"),
            "rate_interval has a documented default and should resolve, not be reported unresolved"
        );
        assert!(
            panel.query.contains("$job"),
            "best-effort-substituted query is kept for display even when inert: {:?}",
            panel.query
        );
        assert!(panel.datasource.is_empty());
    }

    #[test]
    fn a_variable_with_a_saved_current_value_substitutes() {
        let json = r#"{
                "title": "Test",
                "refresh": "30s",
                "time": {"from": "now-1h", "to": "now"},
                "templating": {"list": [
                    {"name": "job", "type": "query", "current": {"value": "node"}}
                ]},
                "panels": [{
                    "title": "Up",
                    "type": "stat",
                    "gridPos": {"x": 0, "y": 0, "w": 6, "h": 4},
                    "datasource": {"type": "prometheus", "uid": "abc"},
                    "targets": [{"expr": "up{job=\"$job\"}", "refId": "A"}]
                }]
            }"#;
        let dashboard = parse_grafana_json(json, "http://localhost:9090").unwrap();
        let panel = &dashboard.panels[0];
        assert!(panel.executable);
        assert_eq!(panel.query, "up{job=\"node\"}");
    }

    #[test]
    fn rate_interval_resolves_to_the_documented_default() {
        let json = minimal_dashboard(
            r#"{
                "title": "CPU",
                "type": "timeseries",
                "gridPos": {"x": 0, "y": 0, "w": 12, "h": 8},
                "datasource": {"type": "prometheus", "uid": "abc"},
                "targets": [{"expr": "rate(node_cpu_seconds_total[$__rate_interval])", "refId": "A"}]
            }"#,
        );
        let dashboard = parse_grafana_json(&json, "http://localhost:9090").unwrap();
        let panel = &dashboard.panels[0];
        assert!(panel.executable);
        assert_eq!(panel.query, "rate(node_cpu_seconds_total[1m])");
    }

    #[test]
    fn an_unmapped_panel_type_is_preserved_but_inert() {
        let json = minimal_dashboard(
            r#"{
                "title": "Pressure",
                "type": "bargauge",
                "gridPos": {"x": 0, "y": 0, "w": 3, "h": 4},
                "datasource": {"type": "prometheus", "uid": "abc"},
                "targets": [{"expr": "up", "refId": "A"}]
            }"#,
        );
        let dashboard = parse_grafana_json(&json, "http://localhost:9090").unwrap();
        let panel = &dashboard.panels[0];
        assert!(!panel.executable);
        assert!(panel.inert_reason.as_deref().unwrap().contains("bargauge"));
    }

    #[test]
    fn a_non_prometheus_datasource_is_preserved_but_inert() {
        let json = minimal_dashboard(
            r#"{
                "title": "Logs",
                "type": "timeseries",
                "gridPos": {"x": 0, "y": 0, "w": 12, "h": 8},
                "datasource": {"type": "loki", "uid": "abc"},
                "targets": [{"expr": "{job=\"varlogs\"}", "refId": "A"}]
            }"#,
        );
        let dashboard = parse_grafana_json(&json, "http://localhost:9090").unwrap();
        let panel = &dashboard.panels[0];
        assert!(!panel.executable);
        assert_eq!(panel.panel_type, PanelType::Timeseries);
    }

    #[test]
    fn a_collapsed_row_s_nested_panels_are_flattened_in() {
        let json = minimal_dashboard(
            r#"{
                "title": "CPU / Mem",
                "type": "row",
                "collapsed": true,
                "gridPos": {"x": 0, "y": 0, "w": 24, "h": 1},
                "panels": [
                    {
                        "title": "CPU",
                        "type": "stat",
                        "gridPos": {"x": 0, "y": 1, "w": 12, "h": 8},
                        "datasource": {"type": "prometheus", "uid": "abc"},
                        "targets": [{"expr": "up", "refId": "A"}]
                    },
                    {
                        "title": "Mem",
                        "type": "stat",
                        "gridPos": {"x": 12, "y": 1, "w": 12, "h": 8},
                        "datasource": {"type": "prometheus", "uid": "abc"},
                        "targets": [{"expr": "node_memory_MemFree_bytes", "refId": "A"}]
                    }
                ]
            }"#,
        );
        let dashboard = parse_grafana_json(&json, "http://localhost:9090").unwrap();
        assert_eq!(
            dashboard.panels.len(),
            2,
            "row itself is dropped, both children imported"
        );
        assert_eq!(dashboard.panels[0].title, "CPU");
        assert_eq!(dashboard.panels[1].title, "Mem");
    }

    #[test]
    fn an_expanded_row_s_sibling_panels_import_normally() {
        let json = r#"{
                "title": "Test",
                "refresh": "30s",
                "time": {"from": "now-1h", "to": "now"},
                "templating": {"list": []},
                "panels": [
                    {"title": "Row", "type": "row", "collapsed": false, "gridPos": {"x": 0, "y": 0, "w": 24, "h": 1}},
                    {
                        "title": "CPU",
                        "type": "stat",
                        "gridPos": {"x": 0, "y": 1, "w": 12, "h": 8},
                        "datasource": {"type": "prometheus", "uid": "abc"},
                        "targets": [{"expr": "up", "refId": "A"}]
                    }
                ]
            }"#;
        let dashboard = parse_grafana_json(json, "http://localhost:9090").unwrap();
        assert_eq!(dashboard.panels.len(), 1);
        assert_eq!(dashboard.panels[0].title, "CPU");
    }

    #[test]
    fn absolute_time_range_is_rejected_not_approximated() {
        let json = r#"{
            "title": "Test",
            "refresh": "30s",
            "time": {"from": "2024-01-01T00:00:00Z", "to": "2024-01-02T00:00:00Z"},
            "panels": []
        }"#;
        let err = parse_grafana_json(json, "http://localhost:9090").unwrap_err();
        assert_eq!(err.code, crate::error::ErrorCode::E006);
    }

    #[test]
    fn absolute_thresholds_map_and_the_valueless_base_step_is_skipped() {
        let json = minimal_dashboard(
            r#"{
                "title": "Pressure",
                "type": "gauge",
                "gridPos": {"x": 0, "y": 0, "w": 6, "h": 4},
                "datasource": {"type": "prometheus", "uid": "abc"},
                "targets": [{"expr": "up", "refId": "A"}],
                "fieldConfig": {"defaults": {"thresholds": {"mode": "absolute", "steps": [
                    {"color": "green"},
                    {"color": "yellow", "value": 70},
                    {"color": "red", "value": 90}
                ]}}}
            }"#,
        );
        let dashboard = parse_grafana_json(&json, "http://localhost:9090").unwrap();
        let thresholds = &dashboard.panels[0].thresholds;
        assert_eq!(thresholds.len(), 2, "the valueless base step is skipped");
        assert_eq!(thresholds[0].name, "yellow");
        assert!((thresholds[0].value - 70.0).abs() < f64::EPSILON);
        assert_eq!(thresholds[1].name, "red");
    }

    #[test]
    fn percentage_mode_thresholds_are_dropped_not_misapplied() {
        let json = minimal_dashboard(
            r#"{
                "title": "Pressure",
                "type": "gauge",
                "gridPos": {"x": 0, "y": 0, "w": 6, "h": 4},
                "datasource": {"type": "prometheus", "uid": "abc"},
                "targets": [{"expr": "up", "refId": "A"}],
                "fieldConfig": {"defaults": {"thresholds": {"mode": "percentage", "steps": [
                    {"color": "green"},
                    {"color": "red", "value": 90}
                ]}}}
            }"#,
        );
        let dashboard = parse_grafana_json(&json, "http://localhost:9090").unwrap();
        assert!(dashboard.panels[0].thresholds.is_empty());
    }

    #[test]
    fn duplicate_threshold_colors_get_a_disambiguating_suffix() {
        let json = minimal_dashboard(
            r#"{
                "title": "Pressure",
                "type": "gauge",
                "gridPos": {"x": 0, "y": 0, "w": 6, "h": 4},
                "datasource": {"type": "prometheus", "uid": "abc"},
                "targets": [{"expr": "up", "refId": "A"}],
                "fieldConfig": {"defaults": {"thresholds": {"mode": "absolute", "steps": [
                    {"color": "red", "value": 50},
                    {"color": "red", "value": 90}
                ]}}}
            }"#,
        );
        let dashboard = parse_grafana_json(&json, "http://localhost:9090").unwrap();
        let thresholds = &dashboard.panels[0].thresholds;
        assert_eq!(thresholds[0].name, "red");
        assert_eq!(thresholds[1].name, "red-2");
    }

    #[test]
    fn a_non_named_css_color_falls_back_to_a_generic_threshold_name() {
        // Grafana's `color` is a free-form CSS color, not always a
        // named one — real dashboards use `rgba(...)` strings, which
        // make an unreadable threshold name if used verbatim.
        let json = minimal_dashboard(
            r#"{
                "title": "Uptime",
                "type": "stat",
                "gridPos": {"x": 0, "y": 0, "w": 6, "h": 4},
                "datasource": {"type": "prometheus", "uid": "abc"},
                "targets": [{"expr": "up", "refId": "A"}],
                "fieldConfig": {"defaults": {"thresholds": {"mode": "absolute", "steps": [
                    {"color": "rgba(245, 54, 54, 0.9)", "value": 90}
                ]}}}
            }"#,
        );
        let dashboard = parse_grafana_json(&json, "http://localhost:9090").unwrap();
        assert_eq!(dashboard.panels[0].thresholds[0].name, "threshold");
    }

    #[test]
    fn braced_variable_with_a_format_specifier_still_resolves() {
        let (out, unresolved) = substitute_variables(
            "up{job=\"${job:regex}\"}",
            &HashMap::from([("job".to_string(), "node".to_string())]),
        );
        assert_eq!(out, "up{job=\"node\"}");
        assert!(unresolved.is_empty());
    }

    #[test]
    fn the_same_unresolved_variable_is_only_reported_once() {
        let (_, unresolved) = substitute_variables("$job / $job", &HashMap::new());
        assert_eq!(unresolved, vec!["job".to_string()]);
    }

    #[test]
    fn refresh_off_variants_all_map_to_off() {
        for value in ["null", "false", "\"\""] {
            let json = format!(
                r#"{{"title": "t", "refresh": {value}, "time": {{"from": "now-1h", "to": "now"}}, "panels": []}}"#
            );
            let dashboard = parse_grafana_json(&json, "http://localhost:9090").unwrap();
            assert_eq!(dashboard.refresh, RefreshInterval::Off, "refresh={value}");
        }
    }

    #[test]
    fn missing_time_block_defaults_to_one_hour() {
        let json = r#"{"title": "t", "panels": []}"#;
        let dashboard = parse_grafana_json(json, "http://localhost:9090").unwrap();
        assert_eq!(
            dashboard.default_range,
            Duration {
                magnitude: 1,
                unit: DurationUnit::Hours
            }
        );
    }
}
