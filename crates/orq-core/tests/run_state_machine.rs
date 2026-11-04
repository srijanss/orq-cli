use orq_core::run::{check_run_transition, IllegalRunTransition, RunState};

const ALL: [RunState; 8] = [
    RunState::Queued,
    RunState::Preparing,
    RunState::Running,
    RunState::Blocked,
    RunState::ReviewReady,
    RunState::Completed,
    RunState::Failed,
    RunState::Cancelled,
];

fn legal() -> Vec<(RunState, RunState)> {
    use RunState::*;
    vec![
        (Queued, Preparing),
        (Queued, Cancelled),
        (Preparing, Running),
        (Preparing, Blocked),
        (Preparing, Failed),
        (Preparing, Cancelled),
        (Running, Blocked),
        (Running, ReviewReady),
        (Running, Failed),
        (Running, Cancelled),
        (Blocked, Running),
        (Blocked, Failed),
        (Blocked, Cancelled),
        (ReviewReady, Completed),
    ]
}

#[test]
fn only_the_documented_run_transitions_are_allowed() {
    let legal = legal();
    for from in ALL {
        for to in ALL {
            let result = check_run_transition(from, to);
            if legal.contains(&(from, to)) {
                assert_eq!(result, Ok(()), "{from:?} -> {to:?} should be allowed");
            } else {
                assert_eq!(
                    result,
                    Err(IllegalRunTransition { from, to }),
                    "{from:?} -> {to:?} should be rejected"
                );
            }
        }
    }
}

#[test]
fn completed_failed_and_cancelled_are_terminal() {
    for state in ALL {
        let terminal = matches!(
            state,
            RunState::Completed | RunState::Failed | RunState::Cancelled
        );
        assert_eq!(state.is_terminal(), terminal, "{state:?}");
        if terminal {
            assert!(
                ALL.iter().all(|to| check_run_transition(state, *to).is_err()),
                "{state:?} must not leave its terminal state"
            );
        }
    }
}

#[test]
fn run_states_have_stable_snake_case_names() {
    let names: Vec<&str> = ALL.iter().map(|s| s.as_str()).collect();
    assert_eq!(
        names,
        [
            "queued",
            "preparing",
            "running",
            "blocked",
            "review_ready",
            "completed",
            "failed",
            "cancelled"
        ]
    );
    for state in ALL {
        assert_eq!(RunState::parse(state.as_str()), Some(state));
    }
    assert_eq!(RunState::parse("paused"), Some(RunState::Paused));
    assert_eq!(RunState::parse("sleeping"), None);
}
