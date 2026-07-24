//! The system prompt. See `docs/specs/assist.md` Section F. The
//! command reference is generated from `dash9_core::VERB_REFERENCE`
//! at call time — never a hand-maintained copy that can drift out of
//! sync with the parser (Section C.1).

use std::fmt::Write as _;

use crate::context::AssistContext;

const INTRO: &str = "You are the dash9 assistant. You have exactly one capability: emitting \
dash9 commands. You cannot run shell commands, read or write files \
directly, or do anything except emit commands from the reference \
below. If a request cannot be expressed as one or more of these \
commands, say so in one sentence and emit no command block at all.";

const OUTPUT_FORMAT: &str = "Output format (follow exactly):\n\
- Optionally, one sentence describing your intent.\n\
- Then, if and only if you are proposing commands, exactly one \
fenced code block containing one command per line and nothing else \
— no comments, no blank lines, no prose inside the block.\n\
- If you cannot express the request as commands, reply with one \
sentence and no fenced code block. That is a complete, valid reply.";

/// Builds the full system prompt: intro, the generated command
/// reference, the output-format contract, then `context`'s rendered
/// (and possibly truncated) `Context:` block.
pub fn system_prompt(context: &AssistContext) -> String {
    let mut prompt = String::new();
    prompt.push_str(INTRO);
    prompt.push_str("\n\nCommand reference:\n");
    for spec in dash9_core::VERB_REFERENCE {
        let args = spec.args.join(" ");
        let _ = writeln!(
            prompt,
            "{} {} — {} Example: {}",
            spec.verb, args, spec.description, spec.example
        );
    }
    prompt.push('\n');
    prompt.push_str(OUTPUT_FORMAT);
    prompt.push_str("\n\nContext:\n");
    prompt.push_str(&context.render_within_budget());
    prompt
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::TimeRangeSummary;

    fn empty_context() -> AssistContext {
        AssistContext {
            datasources: vec![],
            active_datasource_metadata: None,
            dashboard_toml: None,
            time_range: TimeRangeSummary {
                start_ms: 0,
                end_ms: 1000,
            },
        }
    }

    #[test]
    fn prompt_includes_every_verb_reference_entry() {
        let prompt = system_prompt(&empty_context());
        for spec in dash9_core::VERB_REFERENCE {
            assert!(
                prompt.contains(spec.verb),
                "prompt missing verb {:?}",
                spec.verb
            );
            assert!(
                prompt.contains(spec.example),
                "prompt missing example for {:?}",
                spec.verb
            );
        }
    }

    #[test]
    fn prompt_never_mentions_quit() {
        // quit is deliberately excluded from VERB_REFERENCE (Section H);
        // this guards against ever adding it back without reconsidering.
        let prompt = system_prompt(&empty_context());
        assert!(!prompt.to_lowercase().contains("\"quit\""));
    }

    #[test]
    fn prompt_states_the_refusal_contract() {
        let prompt = system_prompt(&empty_context());
        assert!(prompt.contains("say so in one sentence"));
        assert!(prompt.contains("exactly one fenced code block"));
    }

    #[test]
    fn prompt_embeds_the_context_block() {
        let prompt = system_prompt(&empty_context());
        assert!(prompt.contains("Current time range: 0 to 1000"));
    }
}
