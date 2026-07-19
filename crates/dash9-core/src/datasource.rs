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
}
