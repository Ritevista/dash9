//! Shared datasource wiring for `dash9 test` and `dash9 open`: builds
//! one `PrometheusDatasource` per configured `[[datasources]]` entry
//! and dispatches a panel's query the same way regardless of caller
//! (`query_range` for `Timeseries` panels, an instant `query`
//! otherwise — SPEC.md Section C.1/C.3).

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use dash9_core::{
    CommandError, Datasource, Duration, ErrorCode, Frame, PanelType, ValidatedDashboard,
};
use dash9_prom::PrometheusDatasource;

/// Target sample count when deriving a `query_range` step from a
/// panel's effective time range.
const TARGET_RANGE_SAMPLES: i64 = 120;
const MIN_STEP_MS: i64 = 1_000;

pub fn build_datasources(dashboard: &ValidatedDashboard) -> HashMap<String, PrometheusDatasource> {
    dashboard
        .datasources
        .iter()
        .map(|ds| {
            // `dash9-core::validate()` only accepts `type =
            // "prometheus"` in v0.1 (SPEC.md C.1/D), so this is the
            // only adapter constructed today; a second datasource
            // type will need a match here.
            let adapter = PrometheusDatasource::new(ds.name.clone(), ds.url.clone());
            (ds.name.clone(), adapter)
        })
        .collect()
}

/// Dispatches a panel's query the way its `panel_type` demands
/// (`query_range` for `Timeseries`, an instant `query` otherwise).
/// Takes only the three fields this decision actually depends on
/// (`ValidatedPanel`'s other fields — title, datasource, grid,
/// thresholds, ... — are irrelevant here), so a caller building this
/// from a live, runtime-mutable panel (`dash9::live_session`) doesn't
/// need to fabricate a full `ValidatedPanel`/`ValidatedDashboard` just
/// to make this call.
pub async fn execute_panel_query(
    datasource: &PrometheusDatasource,
    panel_type: PanelType,
    query: &str,
    default_range: Duration,
    now_ms: i64,
) -> Result<Frame, CommandError> {
    let to_command_error = |source: <PrometheusDatasource as Datasource>::Error| {
        CommandError::new(ErrorCode::E106, source.to_string(), None)
    };
    match panel_type {
        PanelType::Timeseries => {
            let range_ms = default_range.as_millis();
            let start_ms = now_ms - range_ms;
            let step_ms = (range_ms / TARGET_RANGE_SAMPLES).max(MIN_STEP_MS);
            datasource
                .query_range(query, start_ms, now_ms, step_ms)
                .await
                .map_err(to_command_error)
        }
        PanelType::Gauge | PanelType::Stat | PanelType::Table => datasource
            .query(query, now_ms)
            .await
            .map_err(to_command_error),
    }
}

pub fn epoch_ms_now() -> i64 {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
}
