//! Frame model, command grammar, dashboard spec, and validation.
//!
//! See `SPEC.md` at the repository root. This crate must never depend
//! on an async runtime, a UI toolkit, or a network client — see
//! `scripts/check-architecture.sh`.

mod check;
mod command;
mod dashboard;
mod datasource;
mod duration;
mod error;
mod frame;
mod path;
mod session;

pub use check::{check_panel, PanelCheckResult};
pub use command::{parse, Command, VerbSpec, VERB_REFERENCE};
pub use dashboard::{
    load_path, load_str, validate, DashboardFile, DashboardMeta, DatasourceSpec, DatasourceType,
    GridSpec, PanelSpec, PanelType, ThresholdOp, ThresholdSpec, ValidatedDashboard,
    ValidatedDatasource, ValidatedPanel, ValidatedThreshold, DEFAULT_TEST_LATENCY_BUDGET,
    GRID_COLUMNS,
};
pub use datasource::Datasource;
pub use duration::{Duration, DurationParseError, DurationUnit, RefreshInterval};
pub use error::{CommandError, ErrorCode};
pub use frame::{
    ColumnKind, ColumnValues, Frame, FrameKind, FrameMeta, Labels, Point, Series, Table,
    TableColumn,
};
pub use path::validate_workspace_relative_path;
pub use session::{CommandSource, LogLine, SessionLogEntry};
