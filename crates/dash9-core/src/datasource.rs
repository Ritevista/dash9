//! The `Datasource` port. `dash9-core` defines the trait; adapters
//! (`dash9-prom`, and any future backend) implement it and normalize
//! their native response shape to `Frame` entirely within the
//! implementing crate — nothing upstream of a `Frame` leaks through
//! (SPEC.md Section A). This crate must never depend on a concrete
//! adapter, an async runtime, or a network client.

use crate::frame::Frame;

/// A queryable backend that produces [`Frame`]s.
///
/// `query` backs the command grammar's `q` verb (SPEC.md B.3): an
/// instant evaluation. `query_range` backs a panel's scheduled
/// refresh over `[start_ms, end_ms]`. `Self: Sync` and the `Send`
/// bound on the returned futures let an adapter be driven from a
/// multi-threaded async runtime in the composition root without that
/// runtime type appearing here.
pub trait Datasource: Sync {
    /// Adapter-specific failure detail. SPEC.md B.5's `E106` wraps
    /// this at the command-grammar boundary.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Evaluates `query` at a single instant. `time_ms` is Unix epoch
    /// milliseconds, UTC (SPEC.md A.3).
    fn query(
        &self,
        query: &str,
        time_ms: i64,
    ) -> impl std::future::Future<Output = Result<Frame, Self::Error>> + Send;

    /// Evaluates `query` over `[start_ms, end_ms]` at `step_ms`
    /// resolution. All three are Unix epoch milliseconds/durations,
    /// UTC (SPEC.md A.3).
    fn query_range(
        &self,
        query: &str,
        start_ms: i64,
        end_ms: i64,
        step_ms: i64,
    ) -> impl std::future::Future<Output = Result<Frame, Self::Error>> + Send;

    /// Every metric name known to this datasource, alphabetically
    /// sorted. Added for `docs/specs/assist.md` Section E's context
    /// assembly (the composition root fetches and caches this; the
    /// assistant itself never calls a `Datasource` method directly).
    fn metric_names(
        &self,
    ) -> impl std::future::Future<Output = Result<Vec<String>, Self::Error>> + Send;

    /// Every label *name* (not value) known to this datasource,
    /// alphabetically sorted. Same caller/caching model as
    /// [`Datasource::metric_names`].
    fn label_names(
        &self,
    ) -> impl std::future::Future<Output = Result<Vec<String>, Self::Error>> + Send;

    /// Type and description for one specific metric, when the
    /// datasource has any (`ds metric <name>`, SPEC.md B.3) — `None`,
    /// not an error, for a metric with no metadata (common for custom
    /// or older exporters; not every metric ships a `HELP`/`TYPE`
    /// line). Filtered by `name` at the adapter's own boundary rather
    /// than fetching every metric's metadata and searching client-side
    /// — cheap regardless of how many metrics the datasource has.
    fn metric_metadata(
        &self,
        name: &str,
    ) -> impl std::future::Future<Output = Result<Option<MetricMetadata>, Self::Error>> + Send;
}

/// One metric's type and human-readable description, datasource-agnostic
/// the same way [`Frame`] is — Prometheus's `/api/v1/metadata` also
/// reports a `unit` field, deliberately not carried here since nothing
/// in `dash9 open` shows it yet (YAGNI; add it if/when something needs
/// it, same as any other field).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetricMetadata {
    /// e.g. `"counter"`, `"gauge"`, `"histogram"`, `"summary"`, or
    /// `"unknown"` — whatever string the datasource itself reports,
    /// not a closed `dash9-core` enum (a new Prometheus metric type
    /// someday must never require a `dash9-core` release to display).
    pub metric_type: String,
    pub help: String,
}
