//! Replays every canned `dash9 demo --assist` fixture through the
//! real contract parser and `dash9_core::parse()`/semantic validation
//! (`docs/specs/assist.md` Section K). A fixture that fails here is a
//! bug in the fixture — these are curated, reviewed examples, not
//! live model output — caught in CI before it ever reaches a
//! recording.

use dash9_assist::classify::{classify, ProposedCommand};
use dash9_assist::contract::{process_reply, ProcessedReply};
use dash9_assist::{Fixture, DEMO_FIXTURES_JSON};

fn load_fixtures() -> Vec<Fixture> {
    serde_json::from_str(DEMO_FIXTURES_JSON).expect("fixtures/demo.json must be valid JSON")
}

#[test]
fn fixtures_file_is_not_empty() {
    assert!(!load_fixtures().is_empty());
}

#[test]
fn every_fixture_replays_with_zero_repairs_needed() {
    let workspace_root = std::env::temp_dir();
    for fixture in load_fixtures() {
        let result = process_reply(&fixture.reply, true, &workspace_root);
        match result {
            Ok(ProcessedReply::Refusal(_) | ProcessedReply::Commands { .. }) => {}
            Err(failure) => panic!(
                "fixture {:?} failed to validate cleanly (a bug in the fixture, not something \
a real repair turn should have to fix): {failure:?}",
                fixture.request
            ),
        }
    }
}

#[test]
fn every_command_fixture_classifies_without_panicking() {
    let workspace_root = std::env::temp_dir();
    for fixture in load_fixtures() {
        if let Ok(ProcessedReply::Commands { commands, .. }) =
            process_reply(&fixture.reply, true, &workspace_root)
        {
            for command in commands {
                let _ = classify(command);
            }
        }
    }
}

#[test]
fn the_cpu_load_fixture_produces_two_auto_run_commands() {
    let workspace_root = std::env::temp_dir();
    let fixtures = load_fixtures();
    let fixture = fixtures
        .iter()
        .find(|f| f.request == "show cpu load over the last hour")
        .expect("the cpu load fixture must exist");
    let result = process_reply(&fixture.reply, true, &workspace_root).unwrap();
    match result {
        ProcessedReply::Commands {
            intent_sentence,
            commands,
        } => {
            assert!(intent_sentence.is_some());
            assert_eq!(commands.len(), 2);
            for command in commands {
                assert!(matches!(classify(command), ProposedCommand::AutoRun(_)));
            }
        }
        other @ ProcessedReply::Refusal(_) => panic!("expected Commands, got {other:?}"),
    }
}

#[test]
fn the_save_fixture_produces_one_proposal() {
    let workspace_root = std::env::temp_dir();
    let fixtures = load_fixtures();
    let fixture = fixtures
        .iter()
        .find(|f| f.request == "save this as examples/load.toml")
        .expect("the save fixture must exist");
    let result = process_reply(&fixture.reply, true, &workspace_root).unwrap();
    match result {
        ProcessedReply::Commands { commands, .. } => {
            assert_eq!(commands.len(), 1);
            assert!(matches!(
                classify(commands.into_iter().next().unwrap()),
                ProposedCommand::Proposal(_)
            ));
        }
        other @ ProcessedReply::Refusal(_) => panic!("expected Commands, got {other:?}"),
    }
}

#[test]
fn the_weather_fixture_is_a_refusal() {
    let workspace_root = std::env::temp_dir();
    let fixtures = load_fixtures();
    let fixture = fixtures
        .iter()
        .find(|f| f.request == "what's the weather today")
        .expect("the weather fixture must exist");
    let result = process_reply(&fixture.reply, true, &workspace_root).unwrap();
    assert!(matches!(result, ProcessedReply::Refusal(_)));
}
