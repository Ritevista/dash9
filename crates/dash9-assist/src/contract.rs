//! The reply contract: parsing, structural validation, and the
//! validate/repair loop's core (`process_reply`). See
//! `docs/specs/assist.md` Sections F and G.

use std::path::Path;

use dash9_core::{Command, CommandError, ErrorCode};

/// A reply parsed per the format contract (Section F), before any
/// command-level validation.
#[derive(Debug, Clone, PartialEq)]
pub enum ParsedReply {
    /// No fenced block at all — a complete, valid "I can't do that"
    /// answer, not a contract violation.
    Refusal(String),
    Commands {
        intent_sentence: Option<String>,
        lines: Vec<String>,
    },
}

/// A reply that had a fenced block but didn't satisfy its shape
/// rules. Distinct from [`ContractFailure::InvalidLine`], which is a
/// well-shaped block containing a command that fails to parse.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ContractViolation {
    #[error("more than one fenced code block")]
    MultipleFencedBlocks,
    #[error("text after the fenced code block")]
    TextAfterFencedBlock,
    #[error("a blank line inside the fenced code block")]
    BlankLineInBlock,
}

/// Splits a raw reply into a fenced-block-free intent sentence
/// (refusal) or an intent sentence plus the block's lines, applying
/// the structural rules from Section F. Does not touch
/// `dash9_core::parse()` at all — that happens one layer up, in
/// [`process_reply`], once we know we have a well-shaped block.
pub fn parse_reply(raw: &str) -> Result<ParsedReply, ContractViolation> {
    let trimmed = raw.trim();
    let parts: Vec<&str> = trimmed.split("```").collect();
    match parts.as_slice() {
        [_] => Ok(ParsedReply::Refusal(trimmed.to_string())),
        [before, inside, after] => {
            if !after.trim().is_empty() {
                return Err(ContractViolation::TextAfterFencedBlock);
            }
            let inside_trimmed = inside.trim_matches('\n');
            let lines: Vec<&str> = if inside_trimmed.is_empty() {
                Vec::new()
            } else {
                inside_trimmed.split('\n').collect()
            };
            if lines.iter().any(|line| line.trim().is_empty()) {
                return Err(ContractViolation::BlankLineInBlock);
            }
            let intent_sentence = {
                let before = before.trim();
                (!before.is_empty()).then(|| before.to_string())
            };
            Ok(ParsedReply::Commands {
                intent_sentence,
                lines: lines.into_iter().map(str::to_string).collect(),
            })
        }
        _ => Err(ContractViolation::MultipleFencedBlocks),
    }
}

/// Everything that can go wrong validating one attempt's reply,
/// unified so the repair loop only has one failure type to branch on.
#[derive(Debug, Clone, PartialEq)]
pub enum ContractFailure {
    Violation(ContractViolation),
    /// A well-shaped block, but line `line_number` (1-indexed) failed
    /// to parse or failed semantic validation (this module's private
    /// `validate_for_assist`).
    InvalidLine {
        line_number: usize,
        line: String,
        error: CommandError,
    },
}

impl From<ContractViolation> for ContractFailure {
    fn from(violation: ContractViolation) -> Self {
        ContractFailure::Violation(violation)
    }
}

impl ContractFailure {
    /// The repair-turn message sent back to the model: error code +
    /// message + the offending line only, never the whole original
    /// block (Section G) — the model's own prior full reply is
    /// already in the conversation history, so it doesn't need to be
    /// re-pasted here.
    pub fn repair_message(&self) -> String {
        match self {
            ContractFailure::Violation(violation) => format!(
                "Your reply did not follow the required format ({violation}). Reply again \
with an optional one-sentence intent followed by exactly one fenced block of commands \
(one per line, no blank lines, no prose inside it) — or just one sentence and no fenced \
block if the request can't be expressed as commands."
            ),
            ContractFailure::InvalidLine {
                line_number,
                line,
                error,
            } => format!(
                "Command on line {line_number} failed to parse: `{line}`\n\
Parser error {}: {}\n\
Reply again with the complete corrected command block (same format as before).",
                error.code, error.message
            ),
        }
    }

    /// The `CommandError` carried in
    /// `AssistError::ContractViolationAfterRepairs` (Section J). A
    /// structural [`ContractViolation`] has no natural `CommandError`
    /// of its own, so it's reported as `E006` ("argument fails
    /// value-level validation") — the closest existing fit for "the
    /// reply didn't match the required shape," rather than inventing
    /// a parallel error vocabulary for a case that's really about
    /// output format, not command syntax.
    pub fn into_command_error(self) -> CommandError {
        match self {
            ContractFailure::Violation(violation) => {
                CommandError::new(ErrorCode::E006, violation.to_string(), None)
            }
            ContractFailure::InvalidLine { error, .. } => error,
        }
    }
}

/// A reply's commands after both syntactic (`dash9_core::parse`) and
/// semantic (`validate_for_assist`) validation have passed for every
/// line.
#[derive(Debug, Clone, PartialEq)]
pub enum ProcessedReply {
    Refusal(String),
    Commands {
        intent_sentence: Option<String>,
        commands: Vec<Command>,
    },
}

/// Runs the full validate step (Section G, step 4): parses the
/// contract shape, then parses and semantically validates every
/// resulting line, in order. The first failing line wins — later
/// lines are not even attempted once one fails, matching the
/// "offending line only" repair message.
pub fn process_reply(
    raw: &str,
    focused_panel: bool,
    workspace_root: &Path,
) -> Result<ProcessedReply, ContractFailure> {
    match parse_reply(raw)? {
        ParsedReply::Refusal(sentence) => Ok(ProcessedReply::Refusal(sentence)),
        ParsedReply::Commands {
            intent_sentence,
            lines,
        } => {
            let mut commands = Vec::with_capacity(lines.len());
            for (index, line) in lines.iter().enumerate() {
                let line_number = index + 1;
                let command =
                    dash9_core::parse(line).map_err(|error| ContractFailure::InvalidLine {
                        line_number,
                        line: line.clone(),
                        error,
                    })?;
                validate_for_assist(&command, focused_panel, workspace_root).map_err(|error| {
                    ContractFailure::InvalidLine {
                        line_number,
                        line: line.clone(),
                        error,
                    }
                })?;
                commands.push(command);
            }
            Ok(ProcessedReply::Commands {
                intent_sentence,
                commands,
            })
        }
    }
}

/// Semantic validation beyond bare syntax (Section G step 4.2,
/// Section H's blocklist). Reuses existing, stable error codes rather
/// than inventing assist-specific ones — a datasource or focused-panel
/// problem looks identical whether a human or the assistant caused it.
///
/// Note on scope: today's command grammar has no verb that names a
/// datasource by reference (only `ds add`, which *defines* one) — so
/// there is currently nothing to check for "does a referenced
/// datasource exist" (`E101`). If a future verb ever names one, it
/// plugs into this same function; there is no special assist-only
/// code path to remember to update.
fn validate_for_assist(
    command: &Command,
    focused_panel: bool,
    workspace_root: &Path,
) -> Result<(), CommandError> {
    match command {
        Command::Quit => Err(CommandError::new(
            ErrorCode::E002,
            "\"quit\" is not available to the assistant",
            None,
        )),
        Command::PanelType { .. } | Command::PanelThreshold { .. } | Command::PanelTitle { .. } => {
            if focused_panel {
                Ok(())
            } else {
                Err(CommandError::new(
                    ErrorCode::E103,
                    "\"panel *\" issued with no panel focused",
                    None,
                ))
            }
        }
        Command::DashSave { path } | Command::DashOpen { path } => {
            dash9_core::validate_workspace_relative_path(workspace_root, path).map(|_| ())
        }
        Command::DsAdd { .. }
        | Command::DsList
        | Command::Q { .. }
        | Command::Range { .. }
        | Command::Refresh { .. } => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reply_with_no_fence_is_a_refusal() {
        let parsed = parse_reply("I can't do that with the commands I have.").unwrap();
        assert_eq!(
            parsed,
            ParsedReply::Refusal("I can't do that with the commands I have.".to_string())
        );
    }

    #[test]
    fn reply_with_intent_and_block_parses() {
        let parsed = parse_reply("Sure, here goes.\n```\nq up\nrange 1h\n```").unwrap();
        assert_eq!(
            parsed,
            ParsedReply::Commands {
                intent_sentence: Some("Sure, here goes.".to_string()),
                lines: vec!["q up".to_string(), "range 1h".to_string()],
            }
        );
    }

    #[test]
    fn reply_with_block_and_no_intent_sentence_parses() {
        let parsed = parse_reply("```\nds list\n```").unwrap();
        assert_eq!(
            parsed,
            ParsedReply::Commands {
                intent_sentence: None,
                lines: vec!["ds list".to_string()],
            }
        );
    }

    #[test]
    fn text_after_the_block_is_a_violation() {
        let err = parse_reply("```\nq up\n```\nhope that helps!").unwrap_err();
        assert_eq!(err, ContractViolation::TextAfterFencedBlock);
    }

    #[test]
    fn a_second_fenced_block_is_a_violation() {
        let err = parse_reply("```\nq up\n```\n```\nrange 1h\n```").unwrap_err();
        assert_eq!(err, ContractViolation::MultipleFencedBlocks);
    }

    #[test]
    fn a_blank_line_inside_the_block_is_a_violation() {
        let err = parse_reply("```\nq up\n\nrange 1h\n```").unwrap_err();
        assert_eq!(err, ContractViolation::BlankLineInBlock);
    }

    #[test]
    fn process_reply_rejects_quit_even_though_it_parses() {
        let root = std::env::temp_dir();
        let err = process_reply("```\nquit\n```", true, &root).unwrap_err();
        match err {
            ContractFailure::InvalidLine { error, .. } => assert_eq!(error.code, ErrorCode::E002),
            other @ ContractFailure::Violation(_) => panic!("expected InvalidLine, got {other:?}"),
        }
    }

    #[test]
    fn process_reply_rejects_panel_verbs_with_no_panel_focused() {
        let root = std::env::temp_dir();
        let err = process_reply("```\npanel title \"CPU\"\n```", false, &root).unwrap_err();
        match err {
            ContractFailure::InvalidLine { error, .. } => assert_eq!(error.code, ErrorCode::E103),
            other @ ContractFailure::Violation(_) => panic!("expected InvalidLine, got {other:?}"),
        }
    }

    #[test]
    fn process_reply_accepts_panel_verbs_when_a_panel_is_focused() {
        let root = std::env::temp_dir();
        let result = process_reply("```\npanel title \"CPU\"\n```", true, &root).unwrap();
        assert!(matches!(result, ProcessedReply::Commands { .. }));
    }

    #[test]
    fn process_reply_rejects_dash_save_escaping_the_workspace() {
        let root = std::env::temp_dir();
        let err = process_reply("```\ndash save /etc/passwd\n```", true, &root).unwrap_err();
        match err {
            ContractFailure::InvalidLine { error, .. } => assert_eq!(error.code, ErrorCode::E107),
            other @ ContractFailure::Violation(_) => panic!("expected InvalidLine, got {other:?}"),
        }
    }

    #[test]
    fn process_reply_reports_the_first_failing_line_only() {
        let root = std::env::temp_dir();
        let err =
            process_reply("```\nq up\nnot a real verb\nrange 1h\n```", true, &root).unwrap_err();
        match err {
            ContractFailure::InvalidLine {
                line_number, line, ..
            } => {
                assert_eq!(line_number, 2);
                assert_eq!(line, "not a real verb");
            }
            other @ ContractFailure::Violation(_) => panic!("expected InvalidLine, got {other:?}"),
        }
    }

    #[test]
    fn process_reply_passes_through_a_refusal() {
        let root = std::env::temp_dir();
        let result = process_reply("I can't do that.", true, &root).unwrap();
        assert_eq!(
            result,
            ProcessedReply::Refusal("I can't do that.".to_string())
        );
    }

    #[test]
    fn repair_message_names_the_offending_line_not_the_whole_block() {
        let failure = ContractFailure::InvalidLine {
            line_number: 2,
            line: "not a real verb".to_string(),
            error: CommandError::new(ErrorCode::E002, "unknown verb \"not\"", None),
        };
        let message = failure.repair_message();
        assert!(message.contains("line 2"));
        assert!(message.contains("not a real verb"));
        assert!(message.contains("E002"));
    }
}
