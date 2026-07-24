//! The command grammar. See `SPEC.md` Section B.

use crate::dashboard::{DatasourceType, PanelType, ThresholdOp};
use crate::duration::{Duration, RefreshInterval};
use crate::error::{CommandError, ErrorCode};

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    DsAdd {
        name: String,
        datasource_type: DatasourceType,
        url: String,
    },
    DsList,
    /// Raw-tail: `query` is the remainder of the line verbatim, with
    /// no quote stripping (SPEC.md B.2).
    Q {
        query: String,
    },
    PanelType {
        panel_type: PanelType,
    },
    PanelThreshold {
        name: String,
        op: ThresholdOp,
        value: f64,
    },
    PanelTitle {
        text: String,
    },
    Range {
        duration: Duration,
    },
    Refresh {
        interval: RefreshInterval,
    },
    DashSave {
        path: String,
    },
    DashOpen {
        path: String,
    },
    Quit,
}

fn is_command_ws(c: char) -> bool {
    c == ' ' || c == '\t'
}

/// Splits off the first whitespace-delimited word, trimming any
/// leading whitespace from the remainder.
fn split_first_word(s: &str) -> (&str, &str) {
    match s.find(is_command_ws) {
        Some(idx) => (&s[..idx], s[idx..].trim_start_matches(is_command_ws)),
        None => (s, ""),
    }
}

/// Tokenizes according to the lexical grammar in SPEC.md B.2. Bare
/// words are scanned up to the next whitespace; this is a pragmatic
/// simplification of the formal `bare-word = [^ \t"]+` production
/// (which forbids embedded `"`), safe because every verb that could
/// plausibly need an embedded quote (query text) goes through the
/// raw-tail `q` path instead and never reaches this tokenizer.
fn tokenize(s: &str) -> Result<Vec<String>, CommandError> {
    let chars: Vec<char> = s.chars().collect();
    let mut byte_pos = 0usize;
    let mut i = 0usize;
    let mut tokens = Vec::new();

    while i < chars.len() {
        while i < chars.len() && is_command_ws(chars[i]) {
            byte_pos += chars[i].len_utf8();
            i += 1;
        }
        if i >= chars.len() {
            break;
        }

        if chars[i] == '"' {
            let start_byte = byte_pos;
            byte_pos += chars[i].len_utf8();
            i += 1;
            let mut buf = String::new();
            let mut closed = false;
            while i < chars.len() {
                if chars[i] == '\\'
                    && i + 1 < chars.len()
                    && matches!(chars[i + 1], '"' | '\\' | 'n' | 't')
                {
                    let escaped = match chars[i + 1] {
                        '"' => '"',
                        '\\' => '\\',
                        'n' => '\n',
                        't' => '\t',
                        _ => unreachable!(),
                    };
                    buf.push(escaped);
                    byte_pos += chars[i].len_utf8() + chars[i + 1].len_utf8();
                    i += 2;
                } else if chars[i] == '"' {
                    byte_pos += chars[i].len_utf8();
                    i += 1;
                    closed = true;
                    break;
                } else {
                    buf.push(chars[i]);
                    byte_pos += chars[i].len_utf8();
                    i += 1;
                }
            }
            if !closed {
                return Err(CommandError::new(
                    ErrorCode::E004,
                    "unterminated quoted string",
                    Some((start_byte, byte_pos)),
                ));
            }
            tokens.push(buf);
        } else {
            let mut buf = String::new();
            while i < chars.len() && !is_command_ws(chars[i]) {
                buf.push(chars[i]);
                byte_pos += chars[i].len_utf8();
                i += 1;
            }
            tokens.push(buf);
        }
    }

    Ok(tokens)
}

fn arity_error(verb: &str, expected: usize, got: usize) -> CommandError {
    let plural = if expected == 1 { "" } else { "s" };
    CommandError::new(
        ErrorCode::E005,
        format!("\"{verb}\" expects {expected} argument{plural}, got {got}"),
        None,
    )
}

/// Same as [`arity_error`] but with a description of the expected
/// arguments in the message, matching the worked example in B.5
/// (`"ds add" expects 3 arguments (name, type, url), got 1`).
fn arity_error_described(
    verb: &str,
    expected: usize,
    description: &str,
    got: usize,
) -> CommandError {
    CommandError::new(
        ErrorCode::E005,
        format!(
            "\"{verb}\" expects {expected} argument{} ({description}), got {got}",
            if expected == 1 { "" } else { "s" }
        ),
        None,
    )
}

fn invalid_value(message: impl Into<String>) -> CommandError {
    CommandError::new(ErrorCode::E006, message, None)
}

fn unknown_subverb(ns: &str, subverb: &str) -> CommandError {
    CommandError::new(
        ErrorCode::E003,
        format!("unknown subverb \"{subverb}\" for namespace \"{ns}\""),
        None,
    )
}

fn missing_subverb(ns: &str) -> CommandError {
    CommandError::new(
        ErrorCode::E005,
        format!("\"{ns}\" requires a subverb"),
        None,
    )
}

fn parse_ds(tokens: &[String]) -> Result<Command, CommandError> {
    let Some(sub) = tokens.get(1) else {
        return Err(missing_subverb("ds"));
    };
    match sub.as_str() {
        "add" => {
            let args = &tokens[2..];
            if args.len() != 3 {
                return Err(arity_error_described(
                    "ds add",
                    3,
                    "name, type, url",
                    args.len(),
                ));
            }
            let datasource_type = DatasourceType::parse(&args[1]).ok_or_else(|| {
                invalid_value(format!(
                    "\"{}\" is not a valid datasource type (expected: prometheus)",
                    args[1]
                ))
            })?;
            Ok(Command::DsAdd {
                name: args[0].clone(),
                datasource_type,
                url: args[2].clone(),
            })
        }
        "list" => {
            let args = &tokens[2..];
            if !args.is_empty() {
                return Err(arity_error("ds list", 0, args.len()));
            }
            Ok(Command::DsList)
        }
        other => Err(unknown_subverb("ds", other)),
    }
}

fn parse_panel(tokens: &[String]) -> Result<Command, CommandError> {
    let Some(sub) = tokens.get(1) else {
        return Err(missing_subverb("panel"));
    };
    match sub.as_str() {
        "type" => {
            let args = &tokens[2..];
            if args.len() != 1 {
                return Err(arity_error("panel type", 1, args.len()));
            }
            let panel_type = PanelType::parse(&args[0]).ok_or_else(|| {
                invalid_value(format!(
                    "\"{}\" is not a valid panel type (expected: timeseries, gauge, table, stat)",
                    args[0]
                ))
            })?;
            Ok(Command::PanelType { panel_type })
        }
        "threshold" => {
            let args = &tokens[2..];
            if args.len() != 3 {
                return Err(arity_error_described(
                    "panel threshold",
                    3,
                    "name, op, value",
                    args.len(),
                ));
            }
            let op = ThresholdOp::parse(&args[1]).ok_or_else(|| {
                invalid_value(format!(
                    "\"{}\" is not a valid operator (expected: gt, gte, lt, lte)",
                    args[1]
                ))
            })?;
            let value: f64 = args[2].parse().map_err(|_| {
                invalid_value(format!(
                    "\"{}\" is not a valid numeric threshold value",
                    args[2]
                ))
            })?;
            Ok(Command::PanelThreshold {
                name: args[0].clone(),
                op,
                value,
            })
        }
        "title" => {
            let args = &tokens[2..];
            if args.len() != 1 {
                return Err(arity_error("panel title", 1, args.len()));
            }
            Ok(Command::PanelTitle {
                text: args[0].clone(),
            })
        }
        other => Err(unknown_subverb("panel", other)),
    }
}

fn parse_dash(tokens: &[String]) -> Result<Command, CommandError> {
    let Some(sub) = tokens.get(1) else {
        return Err(missing_subverb("dash"));
    };
    match sub.as_str() {
        "save" => {
            let args = &tokens[2..];
            if args.len() != 1 {
                return Err(arity_error("dash save", 1, args.len()));
            }
            Ok(Command::DashSave {
                path: args[0].clone(),
            })
        }
        "open" => {
            let args = &tokens[2..];
            if args.len() != 1 {
                return Err(arity_error("dash open", 1, args.len()));
            }
            Ok(Command::DashOpen {
                path: args[0].clone(),
            })
        }
        other => Err(unknown_subverb("dash", other)),
    }
}

fn parse_range(tokens: &[String]) -> Result<Command, CommandError> {
    let args = &tokens[1..];
    if args.len() != 1 {
        return Err(arity_error("range", 1, args.len()));
    }
    let duration = Duration::parse(&args[0])
        .map_err(|_| invalid_value(format!("\"{}\" is not a valid duration", args[0])))?;
    Ok(Command::Range { duration })
}

fn parse_refresh(tokens: &[String]) -> Result<Command, CommandError> {
    let args = &tokens[1..];
    if args.len() != 1 {
        return Err(arity_error("refresh", 1, args.len()));
    }
    let interval = RefreshInterval::parse(&args[0]).map_err(|_| {
        invalid_value(format!(
            "\"{}\" is not a valid duration or \"off\"",
            args[0]
        ))
    })?;
    Ok(Command::Refresh { interval })
}

fn parse_quit(tokens: &[String]) -> Result<Command, CommandError> {
    let args = &tokens[1..];
    if !args.is_empty() {
        return Err(arity_error("quit", 0, args.len()));
    }
    Ok(Command::Quit)
}

/// Parses one line of the command grammar (SPEC.md Section B).
pub fn parse(input: &str) -> Result<Command, CommandError> {
    let trimmed = input.trim_matches(is_command_ws);
    if trimmed.is_empty() {
        return Err(CommandError::new(ErrorCode::E001, "command is empty", None));
    }

    // `q` is raw-tail: everything after the verb and one run of
    // whitespace is a single opaque argument, not tokenized
    // (SPEC.md B.2).
    let (first_word, rest) = split_first_word(trimmed);
    if first_word == "q" {
        if rest.is_empty() {
            return Err(arity_error("q", 1, 0));
        }
        return Ok(Command::Q {
            query: rest.to_string(),
        });
    }

    let tokens = tokenize(trimmed)?;
    match tokens[0].as_str() {
        "ds" => parse_ds(&tokens),
        "panel" => parse_panel(&tokens),
        "dash" => parse_dash(&tokens),
        "range" => parse_range(&tokens),
        "refresh" => parse_refresh(&tokens),
        "quit" => parse_quit(&tokens),
        other => Err(CommandError::new(
            ErrorCode::E002,
            format!("unknown verb \"{other}\""),
            None,
        )),
    }
}

/// One entry per verb an automated caller (e.g. `dash9-assist`) may
/// reference. Colocated with `parse()` so a verb addition and its
/// reference-table entry land in the same review; the
/// `verb_reference_matches_parser` test below fails if they ever
/// diverge — see `docs/specs/assist.md` Section C.1.
#[derive(Debug, Clone, Copy)]
pub struct VerbSpec {
    pub verb: &'static str,
    pub args: &'static [&'static str],
    pub example: &'static str,
    pub description: &'static str,
}

/// Excludes `quit` deliberately (`docs/specs/assist.md` Section H):
/// session-control verbs are never something an automated caller
/// should propose, so they are not part of this vocabulary at all,
/// not merely blocked at validation time.
pub const VERB_REFERENCE: &[VerbSpec] = &[
    VerbSpec {
        verb: "ds add",
        args: &["name", "type", "url"],
        example: "ds add prom prometheus http://localhost:9090",
        description: "Add a named Prometheus datasource instance.",
    },
    VerbSpec {
        verb: "ds list",
        args: &[],
        example: "ds list",
        description: "List configured datasources.",
    },
    VerbSpec {
        verb: "q",
        args: &["query"],
        example: r#"q up{job="api"}"#,
        description: "Run an instant query against the focused datasource.",
    },
    VerbSpec {
        verb: "panel type",
        args: &["type"],
        example: "panel type gauge",
        description: "Set the focused panel's visualization type (timeseries, gauge, table, stat).",
    },
    VerbSpec {
        verb: "panel threshold",
        args: &["name", "op", "value"],
        example: "panel threshold crit gte 95",
        description: "Set a named threshold (gt, gte, lt, lte) on the focused panel.",
    },
    VerbSpec {
        verb: "panel title",
        args: &["text"],
        example: r#"panel title "CPU Usage""#,
        description: "Set the focused panel's title.",
    },
    VerbSpec {
        verb: "range",
        args: &["duration"],
        example: "range 1h",
        description: "Set the current view's time range (e.g. 30s, 5m, 1h, 2d).",
    },
    VerbSpec {
        verb: "refresh",
        args: &["duration_or_off"],
        example: "refresh 30s",
        description: "Set the auto-refresh interval, or \"off\" to disable it.",
    },
    VerbSpec {
        verb: "dash save",
        args: &["path"],
        example: "dash save examples/demo.toml",
        description: "Save the current dashboard state to a workspace-relative TOML path.",
    },
    VerbSpec {
        verb: "dash open",
        args: &["path"],
        example: "dash open examples/demo.toml",
        description: "Open a dashboard TOML file from a workspace-relative path.",
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::duration::DurationUnit;

    /// Exhaustive on purpose: adding a new `Command` variant is a
    /// compile error here until this match is updated, which is what
    /// forces a corresponding `VERB_REFERENCE` entry to be considered
    /// in the same change (see `verb_reference_matches_parser`).
    fn variant_tag(cmd: &Command) -> &'static str {
        match cmd {
            Command::DsAdd { .. } => "ds add",
            Command::DsList => "ds list",
            Command::Q { .. } => "q",
            Command::PanelType { .. } => "panel type",
            Command::PanelThreshold { .. } => "panel threshold",
            Command::PanelTitle { .. } => "panel title",
            Command::Range { .. } => "range",
            Command::Refresh { .. } => "refresh",
            Command::DashSave { .. } => "dash save",
            Command::DashOpen { .. } => "dash open",
            Command::Quit => "quit",
        }
    }

    #[test]
    fn verb_reference_matches_parser() {
        for spec in VERB_REFERENCE {
            let parsed = parse(spec.example).unwrap_or_else(|e| {
                panic!(
                    "VERB_REFERENCE example {:?} failed to parse: {e}",
                    spec.example
                )
            });
            assert_eq!(
                variant_tag(&parsed),
                spec.verb,
                "VERB_REFERENCE entry {:?} parsed to a different verb",
                spec.verb
            );
        }

        let all_verbs = [
            "ds add",
            "ds list",
            "q",
            "panel type",
            "panel threshold",
            "panel title",
            "range",
            "refresh",
            "dash save",
            "dash open",
            // "quit" deliberately excluded from VERB_REFERENCE.
        ];
        for verb in all_verbs {
            assert!(
                VERB_REFERENCE.iter().any(|spec| spec.verb == verb),
                "verb {verb:?} exists in the parser but has no VERB_REFERENCE entry"
            );
        }
        assert_eq!(VERB_REFERENCE.len(), all_verbs.len());
    }

    // --- SPEC.md B.3 verb reference table: one test per row ---

    #[test]
    fn ds_add() {
        let cmd = parse("ds add prom prometheus http://localhost:9090").unwrap();
        assert_eq!(
            cmd,
            Command::DsAdd {
                name: "prom".to_string(),
                datasource_type: DatasourceType::Prometheus,
                url: "http://localhost:9090".to_string(),
            }
        );
    }

    #[test]
    fn ds_list() {
        assert_eq!(parse("ds list").unwrap(), Command::DsList);
    }

    #[test]
    fn q_is_raw_tail_and_preserves_promql_quoting() {
        let cmd = parse(r#"q up{job="api"}"#).unwrap();
        assert_eq!(
            cmd,
            Command::Q {
                query: r#"up{job="api"}"#.to_string()
            }
        );
    }

    #[test]
    fn panel_type() {
        let cmd = parse("panel type gauge").unwrap();
        assert_eq!(
            cmd,
            Command::PanelType {
                panel_type: PanelType::Gauge
            }
        );
    }

    #[test]
    fn panel_threshold() {
        let cmd = parse("panel threshold crit gte 95").unwrap();
        assert_eq!(
            cmd,
            Command::PanelThreshold {
                name: "crit".to_string(),
                op: ThresholdOp::Gte,
                value: 95.0,
            }
        );
    }

    #[test]
    fn panel_title_with_quoted_text() {
        let cmd = parse(r#"panel title "CPU Usage""#).unwrap();
        assert_eq!(
            cmd,
            Command::PanelTitle {
                text: "CPU Usage".to_string()
            }
        );
    }

    #[test]
    fn range_() {
        let cmd = parse("range 1h").unwrap();
        assert_eq!(
            cmd,
            Command::Range {
                duration: Duration {
                    magnitude: 1,
                    unit: DurationUnit::Hours
                }
            }
        );
    }

    #[test]
    fn refresh_() {
        let cmd = parse("refresh 30s").unwrap();
        assert_eq!(
            cmd,
            Command::Refresh {
                interval: RefreshInterval::Duration(Duration {
                    magnitude: 30,
                    unit: DurationUnit::Seconds
                })
            }
        );
    }

    #[test]
    fn refresh_off() {
        let cmd = parse("refresh off").unwrap();
        assert_eq!(
            cmd,
            Command::Refresh {
                interval: RefreshInterval::Off
            }
        );
    }

    #[test]
    fn dash_save() {
        let cmd = parse("dash save examples/demo.toml").unwrap();
        assert_eq!(
            cmd,
            Command::DashSave {
                path: "examples/demo.toml".to_string()
            }
        );
    }

    #[test]
    fn dash_open() {
        let cmd = parse("dash open examples/demo.toml").unwrap();
        assert_eq!(
            cmd,
            Command::DashOpen {
                path: "examples/demo.toml".to_string()
            }
        );
    }

    #[test]
    fn quit_() {
        assert_eq!(parse("quit").unwrap(), Command::Quit);
    }

    // --- SPEC.md B.5 worked error examples ---

    #[test]
    fn unknown_verb_is_e002() {
        let err = parse("xyz foo").unwrap_err();
        assert_eq!(err.code, ErrorCode::E002);
        assert_eq!(err.message, "unknown verb \"xyz\"");
    }

    #[test]
    fn ds_add_arity_mismatch_is_e005() {
        let err = parse("ds add prom").unwrap_err();
        assert_eq!(err.code, ErrorCode::E005);
        assert_eq!(
            err.message,
            "\"ds add\" expects 3 arguments (name, type, url), got 1"
        );
    }

    #[test]
    fn panel_type_invalid_value_is_e006() {
        let err = parse("panel type pie").unwrap_err();
        assert_eq!(err.code, ErrorCode::E006);
        assert_eq!(
            err.message,
            "\"pie\" is not a valid panel type (expected: timeseries, gauge, table, stat)"
        );
    }

    #[test]
    fn ds_unknown_subverb_is_e003() {
        let err = parse("ds frobnicate prom").unwrap_err();
        assert_eq!(err.code, ErrorCode::E003);
        assert_eq!(
            err.message,
            "unknown subverb \"frobnicate\" for namespace \"ds\""
        );
    }

    #[test]
    fn q_with_unterminated_promql_brace_still_parses() {
        // The command grammar's parser does not look inside a raw-tail
        // `q` argument; a malformed PromQL expression only surfaces as
        // E106 when the adapter tries to execute it (SPEC.md B.5).
        let cmd = parse(r#"q up{job="api""#).unwrap();
        assert_eq!(
            cmd,
            Command::Q {
                query: r#"up{job="api""#.to_string()
            }
        );
    }

    // --- other grammar rules ---

    #[test]
    fn empty_command_is_e001() {
        assert_eq!(parse("").unwrap_err().code, ErrorCode::E001);
        assert_eq!(parse("   ").unwrap_err().code, ErrorCode::E001);
    }

    #[test]
    fn q_with_no_query_is_e005() {
        assert_eq!(parse("q").unwrap_err().code, ErrorCode::E005);
        assert_eq!(parse("q   ").unwrap_err().code, ErrorCode::E005);
    }

    #[test]
    fn unterminated_quote_is_e004() {
        let err = parse(r#"panel title "CPU"#).unwrap_err();
        assert_eq!(err.code, ErrorCode::E004);
        assert!(err.span.is_some());
    }

    #[test]
    fn whitespace_runs_collapse() {
        assert_eq!(parse("ds   list").unwrap(), Command::DsList);
    }

    #[test]
    fn ds_with_no_subverb_is_arity_error() {
        assert_eq!(parse("ds").unwrap_err().code, ErrorCode::E005);
    }

    #[test]
    fn panel_verb_with_too_many_args_is_e005() {
        assert_eq!(
            parse("panel type gauge extra").unwrap_err().code,
            ErrorCode::E005
        );
    }

    #[test]
    fn quoted_arg_with_escapes() {
        let cmd = parse(r#"panel title "quote: \" backslash: \\""#).unwrap();
        assert_eq!(
            cmd,
            Command::PanelTitle {
                text: "quote: \" backslash: \\".to_string()
            }
        );
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// The parser must return a `Result`, never panic, no matter
        /// what garbage it's fed — this is the safety net a future
        /// LLM repair loop relies on (SPEC.md B.5).
        #[test]
        fn parse_never_panics(input in ".*") {
            let _ = parse(&input);
        }

        /// Every duration accepted by `Duration::parse` formats back
        /// to the exact string it was parsed from (SPEC.md B.4).
        #[test]
        fn duration_round_trips(magnitude in 0u64..1_000_000, unit_idx in 0u8..4) {
            let unit_char = match unit_idx {
                0 => 's',
                1 => 'm',
                2 => 'h',
                _ => 'd',
            };
            let s = format!("{magnitude}{unit_char}");
            let parsed = Duration::parse(&s).unwrap();
            prop_assert_eq!(parsed.magnitude, magnitude);
            prop_assert_eq!(parsed.to_string(), s);
        }

        /// Space-joining a list of bare (unquoted, whitespace-free)
        /// words and tokenizing it must reproduce the original list
        /// (SPEC.md B.2).
        #[test]
        fn tokenizer_round_trips_bare_words(words in proptest::collection::vec("[a-zA-Z0-9_./:-]+", 1..6)) {
            let joined = words.join(" ");
            let tokens = tokenize(&joined).unwrap();
            prop_assert_eq!(tokens, words);
        }
    }
}
