//! Prompt context assembly and its token budget. See
//! `docs/specs/assist.md` Section E. `dash9-assist` never fetches any
//! of this itself — no network access beyond its own LLM endpoint —
//! it only ever turns an already-assembled `AssistContext` into
//! prompt text within budget.

use std::fmt::Write as _;

/// The budget-constrained section of the prompt (metric names, label
/// keys, dashboard TOML combined). Estimated with `len / 4` — the
/// standard rough characters-per-token heuristic, not an exact
/// tokenizer count (no `tiktoken`-class dependency). Exists to keep
/// the prompt from growing unbounded, not to hit an exact model
/// context limit; comfortably fits under the smallest context windows
/// the supported local runtimes default to.
const CONTEXT_TOKEN_BUDGET: usize = 2000;

fn estimate_tokens(text: &str) -> usize {
    text.len() / 4
}

pub struct DatasourceSummary {
    pub name: String,
    pub datasource_type: String,
}

pub struct ActiveDatasourceMetadata {
    pub datasource_name: String,
    /// Alphabetically sorted; deterministic truncation point.
    pub metric_names: Vec<String>,
    pub label_keys: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct TimeRangeSummary {
    pub start_ms: i64,
    pub end_ms: i64,
}

pub struct AssistContext {
    pub datasources: Vec<DatasourceSummary>,
    pub active_datasource_metadata: Option<ActiveDatasourceMetadata>,
    pub dashboard_toml: Option<String>,
    pub time_range: TimeRangeSummary,
}

impl AssistContext {
    /// Renders the `Context:` block of the system prompt (Section F),
    /// spending the token budget in the pinned priority order: metric
    /// names, then label keys, then dashboard TOML. Datasources and
    /// the time range are always included in full — they're tiny and
    /// never worth truncating.
    pub fn render_within_budget(&self) -> String {
        let mut out = String::new();

        let _ = write!(out, "Datasources: ");
        if self.datasources.is_empty() {
            out.push_str("none configured");
        } else {
            let list: Vec<String> = self
                .datasources
                .iter()
                .map(|d| format!("{} ({})", d.name, d.datasource_type))
                .collect();
            out.push_str(&list.join(", "));
        }
        out.push('\n');

        let mut budget = CONTEXT_TOKEN_BUDGET;

        let _ = write!(out, "Active datasource metadata: ");
        match &self.active_datasource_metadata {
            None => out.push_str("none\n"),
            Some(metadata) => {
                let _ = writeln!(out, "({})", metadata.datasource_name);
                let (metric_list, metric_omitted) =
                    take_within_budget(&metadata.metric_names, &mut budget);
                let _ = write!(out, "  metric names: {}", metric_list.join(", "));
                if metric_omitted > 0 {
                    let _ = write!(out, " (+{metric_omitted} more not shown)");
                }
                out.push('\n');

                let (label_list, label_omitted) =
                    take_within_budget(&metadata.label_keys, &mut budget);
                let _ = write!(out, "  label keys: {}", label_list.join(", "));
                if label_omitted > 0 {
                    let _ = write!(out, " (+{label_omitted} more not shown)");
                }
                out.push('\n');
            }
        }

        let _ = write!(out, "Current dashboard: ");
        match &self.dashboard_toml {
            None => out.push_str("none open\n"),
            Some(toml) => {
                let allowed_chars = budget.saturating_mul(4);
                if toml.len() <= allowed_chars {
                    let _ = writeln!(out, "\n{toml}");
                } else {
                    let cut = floor_char_boundary(toml, allowed_chars);
                    let _ = writeln!(
                        out,
                        "\n{}\n... (truncated, {} characters omitted)",
                        &toml[..cut],
                        toml.len() - cut
                    );
                }
            }
        }

        let _ = writeln!(
            out,
            "Current time range: {} to {}",
            self.time_range.start_ms, self.time_range.end_ms
        );

        out
    }
}

/// Greedily takes items (in order) while their estimated token cost
/// fits in `budget`, decrementing it as it goes. Returns the taken
/// slice and how many items were left out.
fn take_within_budget<'a>(items: &'a [String], budget: &mut usize) -> (Vec<&'a str>, usize) {
    let mut taken = Vec::with_capacity(items.len());
    for item in items {
        let cost = estimate_tokens(item) + 1; // +1 for the join separator
        if cost > *budget {
            break;
        }
        *budget -= cost;
        taken.push(item.as_str());
    }
    let omitted = items.len() - taken.len();
    (taken, omitted)
}

/// Largest byte index `<= max_bytes` that lies on a UTF-8 char
/// boundary, so a truncated dashboard TOML never splits a multi-byte
/// character.
fn floor_char_boundary(s: &str, max_bytes: usize) -> usize {
    if max_bytes >= s.len() {
        return s.len();
    }
    let mut cut = max_bytes;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(prefix: &str, count: usize) -> Vec<String> {
        (0..count).map(|i| format!("{prefix}_{i}")).collect()
    }

    #[test]
    fn small_context_renders_everything_in_full() {
        let context = AssistContext {
            datasources: vec![DatasourceSummary {
                name: "prom".to_string(),
                datasource_type: "prometheus".to_string(),
            }],
            active_datasource_metadata: Some(ActiveDatasourceMetadata {
                datasource_name: "prom".to_string(),
                metric_names: names("metric", 5),
                label_keys: names("label", 3),
            }),
            dashboard_toml: Some("[dashboard]\ntitle = \"t\"\n".to_string()),
            time_range: TimeRangeSummary {
                start_ms: 0,
                end_ms: 3_600_000,
            },
        };
        let rendered = context.render_within_budget();
        assert!(rendered.contains("prom (prometheus)"));
        assert!(rendered.contains("metric_0"));
        assert!(rendered.contains("metric_4"));
        assert!(rendered.contains("label_2"));
        assert!(!rendered.contains("not shown"));
        assert!(rendered.contains("[dashboard]"));
    }

    #[test]
    fn no_datasource_and_no_dashboard_render_as_none() {
        let context = AssistContext {
            datasources: vec![],
            active_datasource_metadata: None,
            dashboard_toml: None,
            time_range: TimeRangeSummary {
                start_ms: 0,
                end_ms: 1000,
            },
        };
        let rendered = context.render_within_budget();
        assert!(rendered.contains("none configured"));
        assert!(rendered.contains("Active datasource metadata: none"));
        assert!(rendered.contains("none open"));
    }

    #[test]
    fn metric_names_are_prioritized_over_label_keys_when_budget_is_tight() {
        // Sized to consume almost the entire budget by itself (but
        // still small enough to be taken at all), so label keys are
        // entirely omitted, not partially mixed in.
        let huge_metric = "m".repeat((CONTEXT_TOKEN_BUDGET - 1) * 4);
        let context = AssistContext {
            datasources: vec![],
            active_datasource_metadata: Some(ActiveDatasourceMetadata {
                datasource_name: "prom".to_string(),
                metric_names: vec![huge_metric],
                label_keys: names("label", 3),
            }),
            dashboard_toml: None,
            time_range: TimeRangeSummary {
                start_ms: 0,
                end_ms: 1000,
            },
        };
        let rendered = context.render_within_budget();
        assert!(rendered.contains("label keys: "));
        assert!(rendered.contains("(+3 more not shown)"));
    }

    #[test]
    fn oversized_dashboard_toml_is_truncated_with_a_marker() {
        let huge_toml = "x".repeat(CONTEXT_TOKEN_BUDGET * 8);
        let context = AssistContext {
            datasources: vec![],
            active_datasource_metadata: None,
            dashboard_toml: Some(huge_toml),
            time_range: TimeRangeSummary {
                start_ms: 0,
                end_ms: 1000,
            },
        };
        let rendered = context.render_within_budget();
        assert!(rendered.contains("truncated"));
        assert!(rendered.contains("characters omitted"));
    }

    #[test]
    fn floor_char_boundary_never_splits_a_multibyte_character() {
        let s = "a€b"; // '€' is 3 bytes
        for max in 0..=s.len() {
            let cut = floor_char_boundary(s, max);
            assert!(s.is_char_boundary(cut));
        }
    }
}
