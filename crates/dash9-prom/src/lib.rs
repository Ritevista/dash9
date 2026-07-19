//! Prometheus datasource adapter.
//!
//! Implements the `Datasource` trait from `dash9-core` against the
//! Prometheus HTTP API (`/api/v1/query`, `/api/v1/query_range`),
//! normalizing responses to `dash9_core::Frame` at the boundary.
