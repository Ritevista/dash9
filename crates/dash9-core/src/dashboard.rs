//! The dashboard TOML schema, loader, and validator. See `SPEC.md`
//! Section C.

use crate::duration::{Duration, DurationParseError, RefreshInterval};
use crate::error::{CommandError, ErrorCode};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fmt;
use std::path::Path;

/// The grid's column count (SPEC.md C.1). Matches Grafana's own
/// 24-column `gridPos` grid exactly (`docs/specs/grafana-dashboards.md`
/// Section F), so importing/exporting Grafana JSON never has to scale
/// `x`/`w` and round — a dashboard imported and re-exported with zero
/// edits round-trips its panel positions exactly.
pub const GRID_COLUMNS: u32 = 24;

/// Default `[dashboard].test_latency_budget` when unset (SPEC.md C.1).
pub const DEFAULT_TEST_LATENCY_BUDGET: &str = "5s";

/// The canonical panel visualization kinds. This is the single
/// definition referenced by both `[[panels]].type` (Section C.1) and
/// the `panel type` command (Section B.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelType {
    Timeseries,
    Gauge,
    Table,
    Stat,
}

impl PanelType {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "timeseries" => Self::Timeseries,
            "gauge" => Self::Gauge,
            "table" => Self::Table,
            "stat" => Self::Stat,
            _ => return None,
        })
    }
}

impl fmt::Display for PanelType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Timeseries => "timeseries",
            Self::Gauge => "gauge",
            Self::Table => "table",
            Self::Stat => "stat",
        };
        f.write_str(s)
    }
}

/// The canonical threshold comparison operators. This is the single
/// definition referenced by both `[[panels.thresholds]].op` (Section
/// C.1) and the `panel threshold` command (Section B.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThresholdOp {
    Gt,
    Gte,
    Lt,
    Lte,
}

impl ThresholdOp {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "gt" => Self::Gt,
            "gte" => Self::Gte,
            "lt" => Self::Lt,
            "lte" => Self::Lte,
            _ => return None,
        })
    }
}

impl fmt::Display for ThresholdOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Gt => "gt",
            Self::Gte => "gte",
            Self::Lt => "lt",
            Self::Lte => "lte",
        };
        f.write_str(s)
    }
}

/// The only datasource type accepted in v0.1 (SPEC.md D).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatasourceType {
    Prometheus,
}

impl DatasourceType {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "prometheus" => Some(Self::Prometheus),
            _ => None,
        }
    }
}

impl fmt::Display for DatasourceType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("prometheus")
    }
}

// --- Raw schema, matching the TOML text field-for-field (SPEC.md C.1) ---

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DashboardFile {
    pub dashboard: DashboardMeta,
    #[serde(default)]
    pub datasources: Vec<DatasourceSpec>,
    #[serde(default)]
    pub panels: Vec<PanelSpec>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DashboardMeta {
    pub title: String,
    pub refresh: String,
    pub default_range: String,
    #[serde(default = "default_test_latency_budget")]
    pub test_latency_budget: String,
}

fn default_test_latency_budget() -> String {
    DEFAULT_TEST_LATENCY_BUDGET.to_string()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DatasourceSpec {
    pub name: String,
    #[serde(rename = "type")]
    pub type_: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PanelSpec {
    pub title: String,
    #[serde(rename = "type")]
    pub type_: String,
    pub datasource: String,
    pub query: String,
    #[serde(default)]
    pub allow_empty: bool,
    #[serde(default)]
    pub latency_budget: Option<String>,
    pub grid: GridSpec,
    #[serde(default)]
    pub thresholds: Vec<ThresholdSpec>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GridSpec {
    pub row: u32,
    pub col: u32,
    pub w: u32,
    pub h: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThresholdSpec {
    pub name: String,
    pub op: String,
    pub value: f64,
}

/// Parses dashboard TOML text. A syntax error is reported as `E006`
/// (SPEC.md C.3: "argument fails value-level validation" is the
/// closest fit in the fixed B.5 code set for malformed file content).
pub fn load_str(source: &str) -> Result<DashboardFile, CommandError> {
    toml::from_str(source).map_err(|e| {
        CommandError::new(
            ErrorCode::E006,
            format!("invalid dashboard TOML: {e}"),
            None,
        )
    })
}

/// Reads and parses a dashboard TOML file. A missing/unreadable file
/// reuses `E104` (SPEC.md B.5: "path does not exist or is not
/// readable" — the same condition `dash open` hits).
pub fn load_path(path: &Path) -> Result<DashboardFile, CommandError> {
    let source = std::fs::read_to_string(path).map_err(|e| {
        CommandError::new(
            ErrorCode::E104,
            format!("cannot read {}: {e}", path.display()),
            None,
        )
    })?;
    load_str(&source)
}

// --- Validated schema, ready for `dash9 test` / the TUI to consume ---

#[derive(Debug, Clone, PartialEq)]
pub struct ValidatedDashboard {
    pub title: String,
    pub refresh: RefreshInterval,
    pub default_range: Duration,
    pub test_latency_budget: Duration,
    pub datasources: Vec<ValidatedDatasource>,
    pub panels: Vec<ValidatedPanel>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ValidatedDatasource {
    pub name: String,
    pub datasource_type: DatasourceType,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ValidatedPanel {
    pub title: String,
    pub panel_type: PanelType,
    pub datasource: String,
    pub query: String,
    pub allow_empty: bool,
    pub latency_budget: Option<Duration>,
    pub grid: GridSpec,
    pub thresholds: Vec<ValidatedThreshold>,
    /// `false` only for a panel imported from Grafana JSON
    /// (`crate::grafana`) that couldn't be resolved to a runnable
    /// query — an unmapped panel type, a non-Prometheus datasource, or
    /// an unresolved template variable
    /// (`docs/specs/grafana-dashboards.md` Section A/B). Always `true`
    /// for a TOML-sourced dashboard. When `false`, `query`/`datasource`
    /// are best-effort/display-only and must never reach a live
    /// datasource call.
    pub executable: bool,
    /// `Some(reason)` iff `executable` is `false` — why this panel is
    /// preserved but not queried, shown in place of a live result.
    pub inert_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ValidatedThreshold {
    pub name: String,
    pub op: ThresholdOp,
    pub value: f64,
}

pub(crate) fn e006(message: impl Into<String>) -> CommandError {
    CommandError::new(ErrorCode::E006, message, None)
}

fn parse_duration_field(field: &str, value: &str) -> Result<Duration, CommandError> {
    Duration::parse(value)
        .map_err(|DurationParseError| e006(format!("{field}: \"{value}\" is not a valid duration")))
}

/// Validates a loaded [`DashboardFile`], per SPEC.md C.3 step 1.
/// Fails fast on the first error found, in the order: dashboard
/// metadata durations, datasource names/types, then each panel in
/// file order (type, datasource reference, grid bounds, thresholds).
pub fn validate(file: &DashboardFile) -> Result<ValidatedDashboard, CommandError> {
    let refresh =
        RefreshInterval::parse(&file.dashboard.refresh).map_err(|DurationParseError| {
            e006(format!(
                "dashboard.refresh: \"{}\" is not a valid duration or \"off\"",
                file.dashboard.refresh
            ))
        })?;
    let default_range =
        parse_duration_field("dashboard.default_range", &file.dashboard.default_range)?;
    let test_latency_budget = parse_duration_field(
        "dashboard.test_latency_budget",
        &file.dashboard.test_latency_budget,
    )?;

    let mut seen_names: HashSet<&str> = HashSet::new();
    let mut datasources = Vec::with_capacity(file.datasources.len());
    for ds in &file.datasources {
        if !seen_names.insert(ds.name.as_str()) {
            return Err(CommandError::new(
                ErrorCode::E102,
                format!("duplicate datasource name \"{}\"", ds.name),
                None,
            ));
        }
        let datasource_type = DatasourceType::parse(&ds.type_).ok_or_else(|| {
            e006(format!(
                "datasources.{}: \"{}\" is not a valid datasource type (expected: prometheus)",
                ds.name, ds.type_
            ))
        })?;
        datasources.push(ValidatedDatasource {
            name: ds.name.clone(),
            datasource_type,
            url: ds.url.clone(),
        });
    }

    let mut panels = Vec::with_capacity(file.panels.len());
    for panel in &file.panels {
        panels.push(validate_panel(panel, &seen_names)?);
    }

    Ok(ValidatedDashboard {
        title: file.dashboard.title.clone(),
        refresh,
        default_range,
        test_latency_budget,
        datasources,
        panels,
    })
}

fn validate_panel(
    panel: &PanelSpec,
    seen_datasource_names: &HashSet<&str>,
) -> Result<ValidatedPanel, CommandError> {
    let panel_type = PanelType::parse(&panel.type_)
        .ok_or_else(|| e006(format!("panels.{}: \"{}\" is not a valid panel type (expected: timeseries, gauge, table, stat)", panel.title, panel.type_)))?;

    if !seen_datasource_names.contains(panel.datasource.as_str()) {
        return Err(CommandError::new(
            ErrorCode::E101,
            format!(
                "panels.{}: unknown datasource \"{}\"",
                panel.title, panel.datasource
            ),
            None,
        ));
    }

    if panel.grid.w == 0 || panel.grid.h == 0 {
        return Err(e006(format!(
            "panels.{}: grid.w and grid.h must be >= 1",
            panel.title
        )));
    }
    if panel.grid.col + panel.grid.w > GRID_COLUMNS {
        return Err(e006(format!(
            "panels.{}: grid.col ({}) + grid.w ({}) exceeds the {}-column grid",
            panel.title, panel.grid.col, panel.grid.w, GRID_COLUMNS
        )));
    }

    let latency_budget = panel
        .latency_budget
        .as_ref()
        .map(|s| parse_duration_field(&format!("panels.{}.latency_budget", panel.title), s))
        .transpose()?;

    let mut thresholds = Vec::with_capacity(panel.thresholds.len());
    for threshold in &panel.thresholds {
        let op = ThresholdOp::parse(&threshold.op).ok_or_else(|| {
            e006(format!(
                "panels.{}.thresholds.{}: \"{}\" is not a valid operator (expected: gt, gte, lt, lte)",
                panel.title, threshold.name, threshold.op
            ))
        })?;
        thresholds.push(ValidatedThreshold {
            name: threshold.name.clone(),
            op,
            value: threshold.value,
        });
    }

    Ok(ValidatedPanel {
        title: panel.title.clone(),
        panel_type,
        datasource: panel.datasource.clone(),
        query: panel.query.clone(),
        allow_empty: panel.allow_empty,
        latency_budget,
        grid: panel.grid,
        thresholds,
        executable: true,
        inert_reason: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::duration::DurationUnit;

    /// The exact worked example from SPEC.md C.2.
    const WORKED_EXAMPLE: &str = r#"
[dashboard]
title = "Node Overview"
refresh = "30s"
default_range = "1h"
test_latency_budget = "5s"

[[datasources]]
name = "prom"
type = "prometheus"
url = "http://localhost:9090"

[[panels]]
title = "CPU Usage"
type = "timeseries"
datasource = "prom"
query = "rate(node_cpu_seconds_total{mode=\"user\"}[5m])"
grid = { row = 0, col = 0, w = 12, h = 4 }

[[panels.thresholds]]
name = "warn"
op = "gte"
value = 0.75

[[panels.thresholds]]
name = "crit"
op = "gte"
value = 0.90

[[panels]]
title = "Load Average (1m)"
type = "stat"
datasource = "prom"
query = "node_load1"
grid = { row = 0, col = 12, w = 6, h = 4 }

[[panels]]
title = "Disk Free %"
type = "gauge"
datasource = "prom"
query = "node_filesystem_avail_bytes{mountpoint=\"/\"} / node_filesystem_size_bytes{mountpoint=\"/\"} * 100"
grid = { row = 0, col = 18, w = 6, h = 4 }

[[panels]]
title = "Top Processes by CPU"
type = "table"
datasource = "prom"
query = "topk(5, rate(process_cpu_seconds_total[5m]))"
allow_empty = true
grid = { row = 4, col = 0, w = 24, h = 4 }
"#;

    #[test]
    fn worked_example_loads() {
        let file = load_str(WORKED_EXAMPLE).unwrap();
        assert_eq!(file.dashboard.title, "Node Overview");
        assert_eq!(file.datasources.len(), 1);
        assert_eq!(file.panels.len(), 4);
        assert_eq!(file.panels[0].thresholds.len(), 2);
    }

    #[test]
    fn worked_example_validates() {
        let file = load_str(WORKED_EXAMPLE).unwrap();
        let validated = validate(&file).unwrap();
        assert_eq!(
            validated.refresh,
            RefreshInterval::Duration(Duration {
                magnitude: 30,
                unit: DurationUnit::Seconds
            })
        );
        assert_eq!(
            validated.default_range,
            Duration {
                magnitude: 1,
                unit: DurationUnit::Hours
            }
        );
        assert_eq!(validated.panels.len(), 4);
        assert_eq!(validated.panels[0].panel_type, PanelType::Timeseries);
        assert_eq!(validated.panels[0].thresholds[0].op, ThresholdOp::Gte);
        assert!(validated.panels[3].allow_empty);
        assert_eq!(
            validated.panels[3].grid.col + validated.panels[3].grid.w,
            GRID_COLUMNS
        );
    }

    #[test]
    fn worked_example_round_trips_through_toml() {
        let file = load_str(WORKED_EXAMPLE).unwrap();
        let serialized = toml::to_string(&file).unwrap();
        let reparsed = load_str(&serialized).unwrap();
        assert_eq!(file, reparsed);
    }

    #[test]
    fn duplicate_datasource_name_is_e102() {
        let toml = r#"
[dashboard]
title = "t"
refresh = "30s"
default_range = "1h"

[[datasources]]
name = "prom"
type = "prometheus"
url = "http://a"

[[datasources]]
name = "prom"
type = "prometheus"
url = "http://b"
"#;
        let file = load_str(toml).unwrap();
        let err = validate(&file).unwrap_err();
        assert_eq!(err.code, ErrorCode::E102);
    }

    #[test]
    fn unknown_datasource_reference_is_e101() {
        let toml = r#"
[dashboard]
title = "t"
refresh = "30s"
default_range = "1h"

[[panels]]
title = "p"
type = "stat"
datasource = "missing"
query = "up"
grid = { row = 0, col = 0, w = 1, h = 1 }
"#;
        let file = load_str(toml).unwrap();
        let err = validate(&file).unwrap_err();
        assert_eq!(err.code, ErrorCode::E101);
    }

    #[test]
    fn grid_overflow_is_e006() {
        let toml = r#"
[dashboard]
title = "t"
refresh = "30s"
default_range = "1h"

[[datasources]]
name = "prom"
type = "prometheus"
url = "http://a"

[[panels]]
title = "p"
type = "stat"
datasource = "prom"
query = "up"
grid = { row = 0, col = 20, w = 6, h = 1 }
"#;
        let file = load_str(toml).unwrap();
        let err = validate(&file).unwrap_err();
        assert_eq!(err.code, ErrorCode::E006);
    }

    #[test]
    fn invalid_panel_type_is_e006() {
        let toml = r#"
[dashboard]
title = "t"
refresh = "30s"
default_range = "1h"

[[datasources]]
name = "prom"
type = "prometheus"
url = "http://a"

[[panels]]
title = "p"
type = "pie"
datasource = "prom"
query = "up"
grid = { row = 0, col = 0, w = 1, h = 1 }
"#;
        let file = load_str(toml).unwrap();
        let err = validate(&file).unwrap_err();
        assert_eq!(err.code, ErrorCode::E006);
    }

    #[test]
    fn malformed_toml_is_e006() {
        let err = load_str("not = [valid").unwrap_err();
        assert_eq!(err.code, ErrorCode::E006);
    }

    #[test]
    fn missing_file_is_e104() {
        let err = load_path(Path::new("/nonexistent/dash9-does-not-exist.toml")).unwrap_err();
        assert_eq!(err.code, ErrorCode::E104);
    }

    #[test]
    fn allow_empty_defaults_to_false() {
        let toml = r#"
[dashboard]
title = "t"
refresh = "30s"
default_range = "1h"

[[datasources]]
name = "prom"
type = "prometheus"
url = "http://a"

[[panels]]
title = "p"
type = "stat"
datasource = "prom"
query = "up"
grid = { row = 0, col = 0, w = 1, h = 1 }
"#;
        let file = load_str(toml).unwrap();
        assert!(!file.panels[0].allow_empty);
        let validated = validate(&file).unwrap();
        assert!(!validated.panels[0].allow_empty);
    }

    #[test]
    fn test_latency_budget_defaults_when_unset() {
        let toml = r#"
[dashboard]
title = "t"
refresh = "30s"
default_range = "1h"
"#;
        let file = load_str(toml).unwrap();
        assert_eq!(
            file.dashboard.test_latency_budget,
            DEFAULT_TEST_LATENCY_BUDGET
        );
    }
}
