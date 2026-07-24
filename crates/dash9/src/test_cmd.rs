//! `dash9 test`: headless dashboard validation (SPEC.md Section C.3).
//!
//! All I/O — loading the dashboard file, querying datasources, timing
//! each call — lives here in the composition root. The pass/fail
//! interpretation of a query's outcome is `dash9_core::check_panel`,
//! so that decision logic stays unit-testable without a datasource;
//! this module is only responsible for wiring real `Frame`s and real
//! elapsed times into it and reporting the result.

use std::path::Path;
use std::time::Instant;

use dash9_core::{check_panel, load_path, validate, PanelCheckResult, ValidatedPanel};

use crate::datasources::{build_datasources, epoch_ms_now, execute_panel_query};

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
        let query_result = execute_panel_query(
            datasource,
            panel.panel_type,
            &panel.query,
            dashboard.default_range,
            now_ms,
        )
        .await;
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
