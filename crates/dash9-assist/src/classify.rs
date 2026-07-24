//! Verb blast-radius classification. See `docs/specs/assist.md`
//! Section H for the full table and the read-only-vs-state-changing
//! principle. `Command::Quit` never reaches this function — it's
//! rejected earlier, in `contract::validate_for_assist`, since it's
//! excluded from the assistant's vocabulary entirely (not merely
//! reclassified here).

use dash9_core::Command;

#[derive(Debug, Clone, PartialEq)]
pub enum ProposedCommand {
    /// Renders/executes immediately: current-session view only, no
    /// external or persistent effect.
    AutoRun(Command),
    /// Staged in the log; applied only on the user's explicit
    /// keypress: persists to disk, opens a new network endpoint, or
    /// changes unattended background behavior.
    Proposal(Command),
}

/// Classifies a validated `Command`. Panics only on `Quit`, which is
/// a programming error to reach here — see the module doc comment.
pub fn classify(cmd: Command) -> ProposedCommand {
    match cmd {
        Command::DsList
        | Command::Q { .. }
        | Command::Range { .. }
        | Command::PanelType { .. }
        | Command::PanelThreshold { .. }
        | Command::PanelTitle { .. } => ProposedCommand::AutoRun(cmd),
        Command::DsAdd { .. }
        | Command::Refresh { .. }
        | Command::DashSave { .. }
        | Command::DashOpen { .. } => ProposedCommand::Proposal(cmd),
        Command::Quit => {
            unreachable!(
                "Command::Quit is rejected by validate_for_assist before classify() is ever called"
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dash9_core::{DatasourceType, PanelType};

    #[test]
    fn read_only_verbs_auto_run() {
        assert!(matches!(
            classify(Command::DsList),
            ProposedCommand::AutoRun(Command::DsList)
        ));
        assert!(matches!(
            classify(Command::Q { query: "up".into() }),
            ProposedCommand::AutoRun(Command::Q { .. })
        ));
        assert!(matches!(
            classify(Command::PanelType {
                panel_type: PanelType::Gauge
            }),
            ProposedCommand::AutoRun(Command::PanelType { .. })
        ));
    }

    #[test]
    fn state_changing_verbs_are_proposals() {
        assert!(matches!(
            classify(Command::DsAdd {
                name: "prom".into(),
                datasource_type: DatasourceType::Prometheus,
                url: "http://localhost:9090".into(),
            }),
            ProposedCommand::Proposal(Command::DsAdd { .. })
        ));
        assert!(matches!(
            classify(Command::DashSave {
                path: "examples/demo.toml".into()
            }),
            ProposedCommand::Proposal(Command::DashSave { .. })
        ));
    }

    #[test]
    #[should_panic(expected = "Command::Quit is rejected")]
    fn quit_reaching_classify_is_a_programming_error() {
        classify(Command::Quit);
    }
}
