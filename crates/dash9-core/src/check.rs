//! Pure pass/fail evaluation for `dash9 test` (SPEC.md Section C.3
//! step 2). This module makes no network call and performs no I/O —
//! it only interprets a query outcome (a `Frame`, or the
//! `CommandError` a failed query produced) that the composition root
//! already obtained, against one panel's configured budget and
//! `allow_empty` flag.
//!
//! Keeping this decision logic here rather than inline in the
//! `dash9` binary makes it unit-testable without a datasource and
//! reusable by anything else that needs the same verdict — a future
//! LLM-driven test/repair loop, for instance (SPEC.md D notes the
//! command grammar is designed so an LLM adapter can be added later
//! without a bespoke API; the same reasoning applies to reusing this
//! verdict logic rather than re-deriving it).

use crate::dashboard::ValidatedPanel;
use crate::duration::Duration;
use crate::error::CommandError;
use crate::frame::Frame;

/// The outcome of checking one panel's query result against SPEC.md
/// C.3 step 2's three criteria: (a) did it execute, (b) is it
/// non-empty unless excused, (c) was it within budget.
#[derive(Debug, Clone, PartialEq)]
pub enum PanelCheckResult {
    Pass,
    /// (a): the query failed to parse/execute. Carries the same
    /// `E106` `CommandError` the command-grammar boundary uses for
    /// the same failure (SPEC.md B.5), so both surfaces report query
    /// failures identically.
    QueryFailed(CommandError),
    /// (b): the result was empty and the panel did not set
    /// `allow_empty = true`.
    UnexpectedlyEmpty,
    /// (c): wall-clock query time exceeded the effective budget, even
    /// though (a) and (b) passed.
    LatencyExceeded {
        budget_ms: i64,
        actual_ms: i64,
    },
}

impl PanelCheckResult {
    pub fn is_pass(&self) -> bool {
        matches!(self, PanelCheckResult::Pass)
    }
}

/// Checks one panel's query outcome. `query_result` is what the
/// composition root got from calling the panel's datasource;
/// `elapsed_ms` is the wall-clock time that call took; `dashboard_
/// default_budget` is `[dashboard].test_latency_budget` (SPEC.md
/// C.1), used when the panel sets no `latency_budget` of its own.
///
/// Ordering follows SPEC.md C.3 step 2's own listing: emptiness (b)
/// is checked before latency (c). A panel that is both empty and over
/// budget is therefore reported as [`PanelCheckResult::UnexpectedlyEmpty`],
/// not [`PanelCheckResult::LatencyExceeded`] — this is a deliberate
/// tie-break, not an oversight, since only one outcome is surfaced
/// per panel.
pub fn check_panel(
    panel: &ValidatedPanel,
    query_result: &Result<Frame, CommandError>,
    elapsed_ms: i64,
    dashboard_default_budget: Duration,
) -> PanelCheckResult {
    let frame = match query_result {
        Err(err) => return PanelCheckResult::QueryFailed(err.clone()),
        Ok(frame) => frame,
    };

    if frame.is_empty() && !panel.allow_empty {
        return PanelCheckResult::UnexpectedlyEmpty;
    }

    let budget_ms = panel
        .latency_budget
        .unwrap_or(dashboard_default_budget)
        .as_millis();
    if elapsed_ms > budget_ms {
        return PanelCheckResult::LatencyExceeded {
            budget_ms,
            actual_ms: elapsed_ms,
        };
    }

    PanelCheckResult::Pass
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dashboard::GridSpec;
    use crate::duration::DurationUnit;
    use crate::error::ErrorCode;
    use crate::frame::{FrameKind, FrameMeta};
    use crate::PanelType;

    fn budget(magnitude: u64, unit: DurationUnit) -> Duration {
        Duration { magnitude, unit }
    }

    fn panel(allow_empty: bool, latency_budget: Option<Duration>) -> ValidatedPanel {
        ValidatedPanel {
            title: "p".to_string(),
            panel_type: PanelType::Stat,
            datasource: "prom".to_string(),
            query: "up".to_string(),
            allow_empty,
            latency_budget,
            grid: GridSpec {
                row: 0,
                col: 0,
                w: 1,
                h: 1,
            },
            thresholds: vec![],
            executable: true,
            inert_reason: None,
            gauge_min: 0.0,
            gauge_max: Some(100.0),
        }
    }

    fn non_empty_frame() -> Frame {
        Frame {
            kind: FrameKind::InstantVector,
            series: vec![crate::frame::Series {
                labels: crate::frame::Labels::new(),
                points: vec![crate::frame::Point {
                    timestamp_ms: 0,
                    value: 1.0,
                }],
            }],
            table: None,
            meta: FrameMeta {
                query: "up".to_string(),
                datasource: "prom".to_string(),
                executed_at_ms: 0,
                warnings: vec![],
            },
        }
    }

    fn empty_frame() -> Frame {
        Frame {
            kind: FrameKind::InstantVector,
            series: vec![],
            table: None,
            meta: FrameMeta {
                query: "up".to_string(),
                datasource: "prom".to_string(),
                executed_at_ms: 0,
                warnings: vec![],
            },
        }
    }

    const DEFAULT_BUDGET: Duration = Duration {
        magnitude: 5,
        unit: DurationUnit::Seconds,
    };

    #[test]
    fn non_empty_result_within_budget_passes() {
        let result = check_panel(
            &panel(false, None),
            &Ok(non_empty_frame()),
            100,
            DEFAULT_BUDGET,
        );
        assert_eq!(result, PanelCheckResult::Pass);
    }

    #[test]
    fn query_failure_is_reported_regardless_of_emptiness_or_timing() {
        let err = CommandError::new(ErrorCode::E106, "boom", None);
        let result = check_panel(&panel(false, None), &Err(err.clone()), 0, DEFAULT_BUDGET);
        assert_eq!(result, PanelCheckResult::QueryFailed(err));
    }

    #[test]
    fn empty_result_without_allow_empty_fails() {
        let result = check_panel(&panel(false, None), &Ok(empty_frame()), 100, DEFAULT_BUDGET);
        assert_eq!(result, PanelCheckResult::UnexpectedlyEmpty);
    }

    #[test]
    fn empty_result_with_allow_empty_passes() {
        let result = check_panel(&panel(true, None), &Ok(empty_frame()), 100, DEFAULT_BUDGET);
        assert_eq!(result, PanelCheckResult::Pass);
    }

    #[test]
    fn exceeding_the_dashboard_default_budget_fails() {
        let result = check_panel(
            &panel(false, None),
            &Ok(non_empty_frame()),
            5_001,
            DEFAULT_BUDGET,
        );
        assert_eq!(
            result,
            PanelCheckResult::LatencyExceeded {
                budget_ms: 5_000,
                actual_ms: 5_001
            }
        );
    }

    #[test]
    fn exactly_at_budget_is_not_exceeding() {
        let result = check_panel(
            &panel(false, None),
            &Ok(non_empty_frame()),
            5_000,
            DEFAULT_BUDGET,
        );
        assert_eq!(result, PanelCheckResult::Pass);
    }

    #[test]
    fn panel_latency_budget_overrides_the_dashboard_default() {
        let panel_budget = budget(1, DurationUnit::Seconds);
        let result = check_panel(
            &panel(false, Some(panel_budget)),
            &Ok(non_empty_frame()),
            1_500,
            DEFAULT_BUDGET,
        );
        assert_eq!(
            result,
            PanelCheckResult::LatencyExceeded {
                budget_ms: 1_000,
                actual_ms: 1_500
            }
        );
    }

    #[test]
    fn empty_and_over_budget_reports_emptiness_first() {
        let result = check_panel(
            &panel(false, None),
            &Ok(empty_frame()),
            9_999,
            DEFAULT_BUDGET,
        );
        assert_eq!(result, PanelCheckResult::UnexpectedlyEmpty);
    }
}
