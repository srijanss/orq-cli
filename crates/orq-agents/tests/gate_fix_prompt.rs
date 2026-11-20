//! A fix session is told what happens once it ends: orq runs the gate again itself, so the session
//! neither has to prove the gate green nor asks anyone to run it.

use orq_agents::{adapter_for, FixRequest, ImplementRequest, ModelSpec};

const HARNESSES: [(&str, &str); 2] = [("claude", "opus"), ("pi", "team/model")];
const MARKER: &str = "/logs/gate-fix.done";

fn spec(harness: &str, model: &str) -> ModelSpec {
    ModelSpec { harness: harness.to_string(), model: model.to_string(), thinking: None }
}

/// Every fix prompt: each harness, attended or not, with or without an exit marker.
fn each_fix_prompt(check: impl Fn(&str, bool, Option<&str>, &str)) {
    for (harness, model) in HARNESSES {
        for attended in [false, true] {
            for marker in [None, Some(MARKER)] {
                let fix = FixRequest {
                    implementer: spec(harness, model),
                    task: "The gate failed. Fix it.",
                    exit_marker: marker,
                    attended,
                    allowed_commands: &[],
                };
                let prompt = adapter_for(harness).unwrap().fix_argv(&fix).unwrap().last().unwrap().clone();
                check(harness, attended, marker, &prompt);
            }
        }
    }
}

fn implement_prompt(harness: &str, model: &str) -> String {
    let request = ImplementRequest {
        features: &["COM-01"],
        implementer: spec(harness, model),
        reviewer: spec("pi", "openai/gpt-6-astra"),
        context: None,
        exit_marker: Some("/logs/implement.done"),
        attended: true,
        unattended: false,
        remote_control: None,
        handoff: None,
        continuing: None,
        review_again: None,
        leave_full_suite_to_gate: true,
        allowed_commands: &[],
        declared_files: &[],
        answer: None,
        undeclared_files: &[],
        reopened_findings: &[],
        feature_specs: &[],
    };
    adapter_for(harness).unwrap().implement_argv(&request).unwrap().last().unwrap().clone()
}

const RUNS_AGAIN: &str = "orq runs the whole gate again itself as soon as this session ends";

#[test]
fn a_fix_session_is_told_orq_runs_the_gate_again_once_it_ends_and_an_implement_session_is_not() {
    each_fix_prompt(|harness, attended, marker, prompt| {
        assert!(prompt.contains(RUNS_AGAIN), "{harness} attended={attended} marker={marker:?}: {prompt}");
        assert!(prompt.contains("as many times as its fix attempts allow"), "{harness} attended={attended} marker={marker:?}: {prompt}");
    });
    for (harness, model) in HARNESSES {
        let prompt = implement_prompt(harness, model);
        assert!(!prompt.contains(RUNS_AGAIN), "{harness}: {prompt}");
    }
}

#[test]
fn a_fix_session_is_told_it_need_not_run_the_whole_gate_and_never_asks_the_user_to_run_it() {
    each_fix_prompt(|harness, attended, marker, prompt| {
        let at = format!("{harness} attended={attended} marker={marker:?}: {prompt}");
        assert!(prompt.contains("You do not need to run the whole gate, or the full suite, to finish"), "{at}");
        assert!(prompt.contains("run the failing tests, or the tests for what you changed, the way you can (run_tests or your allowed commands)"), "{at}");
        assert!(prompt.contains("If you cannot run them, say so in your notes and finish anyway"), "{at}");
        assert!(prompt.contains("Never wait for the user, or ask them, to run the gate"), "{at}");
    });
}

#[test]
fn a_fix_session_with_a_marker_writes_it_once_the_fix_is_committed_without_waiting_for_a_gate_run() {
    each_fix_prompt(|harness, attended, marker, prompt| {
        let at = format!("{harness} attended={attended} marker={marker:?}: {prompt}");
        let sentence = "Final step, once the fix is committed and nothing is left to do or ask, without waiting for a gate run: create the empty file /logs/gate-fix.done and then stop.";
        assert_eq!(prompt.contains(sentence), marker.is_some(), "{at}");
    });
}

/// orq reads the note once the session ends and stops there: a note left behind blocks the run even
/// when the session went on to bring the fix in.
#[test]
fn a_fix_session_is_told_the_unrelated_failure_note_blocks_the_run_unless_it_deletes_it() {
    each_fix_prompt(|harness, attended, marker, prompt| {
        let at = format!("{harness} attended={attended} marker={marker:?}: {prompt}");
        assert!(prompt.contains("Writing the unrelated-failure note ends the run as blocked, with no further gate run"), "{at}");
        assert!(
            prompt.contains("If you go on to bring the fix in (for example by merging the target branch), delete the note before you finish, and orq then runs the gate again as usual"),
            "{at}"
        );
    });
}
