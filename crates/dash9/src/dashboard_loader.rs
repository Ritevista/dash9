//! Format-aware dashboard loading shared by `dash9 open`/`dash9 test`
//! (`docs/specs/grafana-dashboards.md` Section F, "Decided": no new
//! verbs — `.json` vs `.toml` is detected from the file itself).
//!
//! `prometheus_url` only matters for a `.json` file: a Grafana export
//! carries an internal datasource `uid` (sometimes itself an
//! unresolved `${variable}`, `dash9_core::grafana` module docs), never
//! a queryable URL, so dash9 has to be told one. A `.toml` file
//! declares its own `[[datasources]] url` and ignores it.

use std::path::Path;

use dash9_core::{
    detect_dashboard_format, load_str, parse_grafana_json, validate, CommandError, DashboardFormat,
    ValidatedDashboard,
};

pub fn load_dashboard(
    path: &Path,
    prometheus_url: &str,
) -> Result<ValidatedDashboard, CommandError> {
    let contents = std::fs::read_to_string(path).map_err(|e| {
        CommandError::new(
            dash9_core::ErrorCode::E104,
            format!("cannot read {}: {e}", path.display()),
            None,
        )
    })?;
    match detect_dashboard_format(path, &contents) {
        DashboardFormat::Json => parse_grafana_json(&contents, prometheus_url),
        DashboardFormat::Toml => load_str(&contents).and_then(|file| validate(&file)),
    }
}
