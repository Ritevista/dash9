//! The Frame model. See `SPEC.md` Section A.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A label set attached to a [`Series`]. `BTreeMap` so serialization
/// and comparison are deterministic (SPEC.md A.1).
pub type Labels = BTreeMap<String, String>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Frame {
    pub kind: FrameKind,
    /// Populated when `kind` is `Timeseries` or `InstantVector`. Empty otherwise.
    pub series: Vec<Series>,
    /// Populated when `kind` is `Table`. `None` otherwise.
    pub table: Option<Table>,
    pub meta: FrameMeta,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrameKind {
    Timeseries,
    InstantVector,
    Table,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Series {
    pub labels: Labels,
    pub points: Vec<Point>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Point {
    /// Unix epoch milliseconds, UTC (SPEC.md A.3).
    pub timestamp_ms: i64,
    pub value: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Table {
    pub columns: Vec<TableColumn>,
    pub row_count: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableColumn {
    pub name: String,
    pub kind: ColumnKind,
    pub values: ColumnValues,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColumnKind {
    Time,
    Float,
    Int,
    String,
    Bool,
}

/// One variant per [`ColumnKind`]. Every column's `Vec` has exactly
/// `Table::row_count` entries. Non-time columns carry `Option<T>` so a
/// table can represent SQL-style NULL; the `Time` column never has gaps.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ColumnValues {
    Time(Vec<i64>),
    Float(Vec<Option<f64>>),
    Int(Vec<Option<i64>>),
    String(Vec<Option<String>>),
    Bool(Vec<Option<bool>>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrameMeta {
    /// The query text as sent to the datasource, verbatim.
    pub query: String,
    /// The datasource name (matches a `[[datasources]]` entry), not
    /// the datasource type.
    pub datasource: String,
    pub executed_at_ms: i64,
    /// Non-fatal adapter warnings. Never populated for fatal errors —
    /// those are `Err(...)` at the adapter trait boundary, never a
    /// `Frame` with warnings.
    #[serde(default)]
    pub warnings: Vec<String>,
}

impl Frame {
    /// The single definition of "empty" (SPEC.md A.4). Every caller —
    /// the TUI's "no data" placeholder, `dash9 test`'s `allow_empty`
    /// check — uses this rather than re-deriving emptiness.
    pub fn is_empty(&self) -> bool {
        match self.kind {
            FrameKind::Timeseries | FrameKind::InstantVector => {
                self.series.is_empty() || self.series.iter().all(|s| s.points.is_empty())
            }
            FrameKind::Table => self.table.as_ref().is_none_or(|t| t.row_count == 0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timeseries_json() -> &'static str {
        r#"{
          "kind": "timeseries",
          "series": [
            {
              "labels": { "instance": "10.0.0.1:9100", "job": "node" },
              "points": [
                { "timestamp_ms": 1700000000000, "value": 0.42 },
                { "timestamp_ms": 1700000015000, "value": 0.47 }
              ]
            },
            {
              "labels": { "instance": "10.0.0.2:9100", "job": "node" },
              "points": [
                { "timestamp_ms": 1700000000000, "value": 0.11 },
                { "timestamp_ms": 1700000015000, "value": 0.13 }
              ]
            }
          ],
          "table": null,
          "meta": {
            "query": "rate(node_cpu_seconds_total[5m])",
            "datasource": "prom",
            "executed_at_ms": 1700000015421,
            "warnings": []
          }
        }"#
    }

    fn instant_vector_json() -> &'static str {
        r#"{
          "kind": "instant_vector",
          "series": [
            {
              "labels": { "instance": "10.0.0.1:9100", "job": "node" },
              "points": [{ "timestamp_ms": 1700000015000, "value": 0.47 }]
            }
          ],
          "table": null,
          "meta": {
            "query": "node_load1",
            "datasource": "prom",
            "executed_at_ms": 1700000015421,
            "warnings": []
          }
        }"#
    }

    fn table_json() -> &'static str {
        r#"{
          "kind": "table",
          "series": [],
          "table": {
            "columns": [
              { "name": "instance", "kind": "string", "values": { "String": ["10.0.0.1:9100", "10.0.0.2:9100"] } },
              { "name": "load1", "kind": "float", "values": { "Float": [0.47, 0.13] } }
            ],
            "row_count": 2
          },
          "meta": {
            "query": "node_load1",
            "datasource": "prom",
            "executed_at_ms": 1700000015421,
            "warnings": []
          }
        }"#
    }

    fn empty_json() -> &'static str {
        r#"{
          "kind": "timeseries",
          "series": [],
          "table": null,
          "meta": {
            "query": "rate(nonexistent_metric[5m])",
            "datasource": "prom",
            "executed_at_ms": 1700000015421,
            "warnings": []
          }
        }"#
    }

    #[test]
    fn timeseries_example_deserializes_and_round_trips() {
        let frame: Frame = serde_json::from_str(timeseries_json()).unwrap();
        assert_eq!(frame.kind, FrameKind::Timeseries);
        assert_eq!(frame.series.len(), 2);
        assert_eq!(frame.series[0].labels["instance"], "10.0.0.1:9100");
        assert_eq!(frame.series[0].points.len(), 2);
        assert!(!frame.is_empty());

        let re_serialized = serde_json::to_string(&frame).unwrap();
        let round_tripped: Frame = serde_json::from_str(&re_serialized).unwrap();
        assert_eq!(frame, round_tripped);
    }

    #[test]
    fn instant_vector_example_has_one_point_per_series() {
        let frame: Frame = serde_json::from_str(instant_vector_json()).unwrap();
        assert_eq!(frame.kind, FrameKind::InstantVector);
        assert_eq!(frame.series.len(), 1);
        assert_eq!(frame.series[0].points.len(), 1);
        assert!(!frame.is_empty());
    }

    #[test]
    fn table_example_deserializes_and_round_trips() {
        let frame: Frame = serde_json::from_str(table_json()).unwrap();
        assert_eq!(frame.kind, FrameKind::Table);
        let table = frame.table.as_ref().unwrap();
        assert_eq!(table.row_count, 2);
        assert_eq!(table.columns.len(), 2);
        assert_eq!(table.columns[0].kind, ColumnKind::String);
        match &table.columns[1].values {
            ColumnValues::Float(values) => assert_eq!(values, &vec![Some(0.47), Some(0.13)]),
            other => panic!("expected Float column, got {other:?}"),
        }
        assert!(!frame.is_empty());

        let re_serialized = serde_json::to_string(&frame).unwrap();
        let round_tripped: Frame = serde_json::from_str(&re_serialized).unwrap();
        assert_eq!(frame, round_tripped);
    }

    #[test]
    fn empty_example_is_empty() {
        let frame: Frame = serde_json::from_str(empty_json()).unwrap();
        assert_eq!(frame.kind, FrameKind::Timeseries);
        assert!(frame.series.is_empty());
        assert!(frame.is_empty());
    }

    #[test]
    fn series_matched_but_no_points_in_range_is_also_empty() {
        let frame = Frame {
            kind: FrameKind::Timeseries,
            series: vec![Series {
                labels: Labels::new(),
                points: vec![],
            }],
            table: None,
            meta: FrameMeta {
                query: "up".to_string(),
                datasource: "prom".to_string(),
                executed_at_ms: 0,
                warnings: vec![],
            },
        };
        assert!(frame.is_empty());
    }

    #[test]
    fn table_with_zero_rows_is_empty() {
        let frame = Frame {
            kind: FrameKind::Table,
            series: vec![],
            table: Some(Table {
                columns: vec![],
                row_count: 0,
            }),
            meta: FrameMeta {
                query: "up".to_string(),
                datasource: "prom".to_string(),
                executed_at_ms: 0,
                warnings: vec![],
            },
        };
        assert!(frame.is_empty());
    }

    #[test]
    fn table_with_no_table_payload_is_empty() {
        let frame = Frame {
            kind: FrameKind::Table,
            series: vec![],
            table: None,
            meta: FrameMeta {
                query: "up".to_string(),
                datasource: "prom".to_string(),
                executed_at_ms: 0,
                warnings: vec![],
            },
        };
        assert!(frame.is_empty());
    }
}
