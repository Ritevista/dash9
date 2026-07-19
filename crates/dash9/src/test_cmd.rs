//! `dash9 test`: headless dashboard validation (SPEC.md Section C.3).
//!
//! All I/O — loading the dashboard file, querying datasources, timing
//! each call — lives here in the composition root. The pass/fail
//! interpretation of a query's outcome is `dash9_core::check_panel`,
//! so that decision logic stays unit-testable without a datasource;
//! this module is only responsible for wiring real `Frame`s and real
//! elapsed times into it and reporting the result.

use std::collections::HashMap;
use std::path::Path;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use dash9_core::{
    check_panel, load_path, validate, CommandError, Datasource, ErrorCode, Frame, PanelCheckResult,
    PanelType, ValidatedDashboard, ValidatedPanel,
};
use dash9_prom::PrometheusDatasource;

/// Target sample count when deriving a `query_range` step from a
/// panel's effective time range — the same rough cadence
/// `dash9 demo`'s synthetic chart uses.
const TARGET_RANGE_SAMPLES: i64 = 120;
const MIN_STEP_MS: i64 = 1_000;

/// Runs `dash9 test <path>` and returns the process exit code per
/// SPEC.md C.3: `0` all panels passed, `1` the file was valid but a
/// panel failed, `2` the dashboard file itself failed to load or
/// validate (no panel is attempted in that case).
pub async fn run(path: &Path) -> anyhow::Result<i32> {
    let dashboard = match load_path(path).and_then(|file| validate(&file)) {
        Ok(dashboard) => dashboard,
        Err(err) => {
            println!("dashboard invalid: {err}");
            return Ok(2);
        }
    };

    let datasources = build_datasources(&dashboard);
    let mut all_passed = true;

    for panel in &dashboard.panels {
        let Some(datasource) = datasources.get(&panel.datasource) else {
            // `validate()` already guarantees every panel's
            // `datasource` matches a configured entry, so this is an
            // internal invariant violation, not a user-facing
            // dashboard error.
            anyhow::bail!(
                "internal error: panel \"{}\" references unconfigured datasource \"{}\"",
                panel.title,
                panel.datasource
            );
        };

        let now_ms = epoch_ms_now();
        let started = Instant::now();
        let query_result = execute_panel_query(datasource, panel, &dashboard, now_ms).await;
        let elapsed_ms = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);

        let result = check_panel(
            panel,
            &query_result,
            elapsed_ms,
            dashboard.test_latency_budget,
        );
        all_passed &= result.is_pass();
        print_panel_result(panel, &result);
    }

    println!(
        "{}",
        if all_passed {
            "all panels passed"
        } else {
            "one or more panels failed"
        }
    );
    Ok(i32::from(!all_passed))
}

fn build_datasources(dashboard: &ValidatedDashboard) -> HashMap<String, PrometheusDatasource> {
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

async fn execute_panel_query(
    datasource: &PrometheusDatasource,
    panel: &ValidatedPanel,
    dashboard: &ValidatedDashboard,
    now_ms: i64,
) -> Result<Frame, CommandError> {
    let to_command_error = |source: <PrometheusDatasource as Datasource>::Error| {
        CommandError::new(ErrorCode::E106, source.to_string(), None)
    };
    match panel.panel_type {
        PanelType::Timeseries => {
            let range_ms = dashboard.default_range.as_millis();
            let start_ms = now_ms - range_ms;
            let step_ms = (range_ms / TARGET_RANGE_SAMPLES).max(MIN_STEP_MS);
            datasource
                .query_range(&panel.query, start_ms, now_ms, step_ms)
                .await
                .map_err(to_command_error)
        }
        PanelType::Gauge | PanelType::Stat | PanelType::Table => datasource
            .query(&panel.query, now_ms)
            .await
            .map_err(to_command_error),
    }
}

fn print_panel_result(panel: &ValidatedPanel, result: &PanelCheckResult) {
    match result {
        PanelCheckResult::Pass => println!("PASS  {}", panel.title),
        PanelCheckResult::QueryFailed(err) => println!("FAIL  {}: {err}", panel.title),
        PanelCheckResult::UnexpectedlyEmpty => println!(
            "FAIL  {}: query returned no data (allow_empty is false)",
            panel.title
        ),
        PanelCheckResult::LatencyExceeded {
            budget_ms,
            actual_ms,
        } => println!(
            "FAIL  {}: query took {actual_ms}ms, exceeding the {budget_ms}ms budget",
            panel.title
        ),
    }
}

fn epoch_ms_now() -> i64 {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
}
