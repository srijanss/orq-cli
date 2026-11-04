use orq_core::run::{check_run_transition, RunState};
use RunState::*;

#[test]
fn pause_states_have_stable_names_and_round_trip() {
    for (state, name) in [(PauseRequested, "pause_requested"), (Pausing, "pausing"), (Paused, "paused")] {
        assert_eq!(state.as_str(), name);
        assert_eq!(RunState::parse(name), Some(state));
        assert!(RunState::ALL.contains(&state));
        assert!(!state.is_terminal(), "{name} is not an end state");
    }
    for state in RunState::ALL {
        assert_eq!(RunState::parse(state.as_str()), Some(state));
    }
}

#[test]
fn a_run_pauses_through_the_requested_and_pausing_states_and_resumes_to_running() {
    for (from, to) in [(Running, PauseRequested), (PauseRequested, Pausing), (Pausing, Paused), (Paused, Running)] {
        assert_eq!(check_run_transition(from, to), Ok(()), "{from:?} -> {to:?}");
    }
}

#[test]
fn a_pausing_run_can_still_stop_for_any_reason_a_running_one_can() {
    for from in [PauseRequested, Pausing, Paused] {
        for to in [Failed, Cancelled] {
            assert_eq!(check_run_transition(from, to), Ok(()), "{from:?} -> {to:?}");
        }
    }
    // The run may reach its end or block before the next stage boundary.
    assert_eq!(check_run_transition(PauseRequested, Blocked), Ok(()));
    assert_eq!(check_run_transition(PauseRequested, ReviewReady), Ok(()));
}

#[test]
fn nothing_else_enters_or_skips_the_pause_states() {
    for (from, to) in [
        (Queued, PauseRequested),
        (Preparing, PauseRequested),
        (Blocked, PauseRequested),
        (ReviewReady, PauseRequested),
        (Running, Pausing),
        (Running, Paused),
        (PauseRequested, Paused),
        (PauseRequested, Running),
        (Pausing, Running),
        (Paused, Pausing),
        (Paused, PauseRequested),
        (Paused, Blocked),
        (Completed, Paused),
        (Paused, Completed),
    ] {
        assert!(check_run_transition(from, to).is_err(), "{from:?} -> {to:?} should be rejected");
    }
}
