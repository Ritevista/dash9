//! Panel data export: CSV/Markdown table rendering from a `Frame`,
//! reusing the exact `Table` fallback already built for the table
//! panel renderer (`draw::series_as_table`) — a chart panel, a stat
//! panel, and a table panel all export through one code path. Pure,
//! no I/O: the composition root does the actual file write (same
//! split as every other `dash9-tui` module).
//!
//! PNG is deliberately not implemented — see [`ExportFormat::Png`].

use dash9_core::{Frame, Table};

use crate::draw::{column_cell, series_as_table};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Csv,
    Markdown,
    /// Not implemented. Ratatui has no terminal-to-image path; doing
    /// this for real would mean pulling in a rasterization dependency
    /// (e.g. `plotters`). Reported honestly as unavailable rather
    /// than faked or half-built — matches `LoreMesh`'s own stance on
    /// the same gap ("PNG reports that an optional local renderer is
    /// not configured").
    Png,
}

impl ExportFormat {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "csv" => Some(Self::Csv),
            "md" | "markdown" => Some(Self::Markdown),
            "png" => Some(Self::Png),
            _ => None,
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Csv => "csv",
            Self::Markdown => "md",
            Self::Png => "png",
        }
    }
}

/// The `Table` to export: the frame's native one if it has one, else
/// synthesized from series (the common case — Prometheus never
/// returns a native `Table` frame, see `series_as_table`'s docs).
/// `None` only when there's nothing to export at all (empty frame).
pub fn table_for_export(frame: &Frame) -> Option<Table> {
    frame.table.clone().or_else(|| series_as_table(frame))
}

/// RFC-4180-ish: quotes a field only when it contains a comma,
/// double quote, or newline; doubles embedded quotes.
fn csv_field(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

pub fn table_to_csv(table: &Table) -> String {
    let mut out = String::new();
    let header: Vec<String> = table.columns.iter().map(|c| csv_field(&c.name)).collect();
    out.push_str(&header.join(","));
    out.push('\n');
    for row in 0..table.row_count {
        let cells: Vec<String> = table
            .columns
            .iter()
            .map(|c| csv_field(&column_cell(c, row)))
            .collect();
        out.push_str(&cells.join(","));
        out.push('\n');
    }
    out
}

pub fn table_to_markdown(table: &Table) -> String {
    let mut out = String::new();
    let headers: Vec<&str> = table.columns.iter().map(|c| c.name.as_str()).collect();
    out.push_str("| ");
    out.push_str(&headers.join(" | "));
    out.push_str(" |\n|");
    for _ in &headers {
        out.push_str("---|");
    }
    out.push('\n');
    for row in 0..table.row_count {
        let cells: Vec<String> = table.columns.iter().map(|c| column_cell(c, row)).collect();
        out.push_str("| ");
        out.push_str(&cells.join(" | "));
        out.push_str(" |\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use dash9_core::{ColumnKind, ColumnValues, FrameKind, FrameMeta, Point, Series, TableColumn};
    use std::collections::BTreeMap;

    fn sample_table() -> Table {
        Table {
            columns: vec![
                TableColumn {
                    name: "name".to_string(),
                    kind: ColumnKind::String,
                    values: ColumnValues::String(vec![Some("has, comma".to_string()), None]),
                },
                TableColumn {
                    name: "value".to_string(),
                    kind: ColumnKind::Float,
                    values: ColumnValues::Float(vec![Some(1.5), Some(2.25)]),
                },
            ],
            row_count: 2,
        }
    }

    #[test]
    fn csv_quotes_fields_containing_a_comma() {
        let csv = table_to_csv(&sample_table());
        assert!(csv.contains("\"has, comma\",1.500"));
        assert!(csv.contains("null,2.250"));
        assert!(csv.starts_with("name,value\n"));
    }

    #[test]
    fn markdown_renders_a_pipe_table() {
        let md = table_to_markdown(&sample_table());
        assert!(md.starts_with("| name | value |\n|---|---|\n"));
        assert!(md.contains("| has, comma | 1.500 |"));
        assert!(md.contains("| null | 2.250 |"));
    }

    #[test]
    fn export_format_parses_known_names_only() {
        assert_eq!(ExportFormat::parse("csv"), Some(ExportFormat::Csv));
        assert_eq!(ExportFormat::parse("md"), Some(ExportFormat::Markdown));
        assert_eq!(
            ExportFormat::parse("markdown"),
            Some(ExportFormat::Markdown)
        );
        assert_eq!(ExportFormat::parse("png"), Some(ExportFormat::Png));
        assert_eq!(ExportFormat::parse("pdf"), None);
    }

    fn timeseries_frame() -> Frame {
        let mut labels = BTreeMap::new();
        labels.insert("job".to_string(), "node".to_string());
        Frame {
            kind: FrameKind::Timeseries,
            series: vec![Series {
                labels,
                points: vec![Point {
                    timestamp_ms: 0,
                    value: 0.5,
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

    #[test]
    fn table_for_export_falls_back_to_series_as_table_for_timeseries_frames() {
        let table = table_for_export(&timeseries_frame()).expect("should synthesize a table");
        let names: Vec<&str> = table.columns.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"job"));
        assert!(names.contains(&"value"));
    }

    #[test]
    fn table_for_export_uses_native_table_when_present() {
        let mut frame = timeseries_frame();
        frame.kind = FrameKind::Table;
        frame.series = vec![];
        frame.table = Some(sample_table());
        let table = table_for_export(&frame).expect("native table present");
        assert_eq!(table.columns[0].name, "name");
    }
}
