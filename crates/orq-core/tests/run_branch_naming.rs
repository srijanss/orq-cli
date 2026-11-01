use std::collections::HashSet;
use std::path::PathBuf;

use orq_core::naming::{run_naming, NamingError, RunNaming};

fn free(_: &RunNaming) -> bool {
    false
}

#[test]
fn branch_and_worktree_follow_the_bash_orchestrator_layout() {
    let naming = run_naming("fix--", "run-state-machine", free).unwrap();
    assert_eq!(naming.branch, "fix--run-state-machine");
    assert_eq!(
        naming.worktree_dir,
        PathBuf::from(".claude/worktrees/fix--run-state-machine")
    );
    let custom = run_naming("feat/", "COM-02.v2_x", free).unwrap();
    assert_eq!(custom.branch, "feat/COM-02.v2_x");
    assert_eq!(custom.worktree_dir, PathBuf::from(".claude/worktrees/feat/COM-02.v2_x"));
}

#[test]
fn a_taken_branch_or_worktree_gets_the_next_numbered_suffix() {
    let taken: HashSet<&str> = ["fix--a", "fix--a-2"].into_iter().collect();
    let naming = run_naming("fix--", "a", |n| taken.contains(n.branch.as_str())).unwrap();
    assert_eq!(naming.branch, "fix--a-3");
    assert_eq!(naming.worktree_dir, PathBuf::from(".claude/worktrees/fix--a-3"));
}

#[test]
fn feature_names_that_are_not_slugs_are_rejected() {
    for bad in ["", "-x", ".x", "a b", "a/b", "a..b", "a.lock", "a.", "ä", "a\nb", "a~1", "a:b"] {
        assert_eq!(
            run_naming("fix--", bad, free),
            Err(NamingError::InvalidFeatureName(bad.to_string())),
            "{bad:?}"
        );
    }
}

#[test]
fn prefixes_that_would_make_an_unsafe_ref_or_path_are_rejected() {
    for bad in ["/x", "-x", ".x", "a//b", "a/../b", "a/./b", "a b", "a\\b", "a~", "x/.", "a@{b"] {
        assert_eq!(
            run_naming(bad, "f", free),
            Err(NamingError::InvalidPrefix(bad.to_string())),
            "{bad:?}"
        );
    }
    assert_eq!(run_naming("", "f", free).unwrap().branch, "f");
}

#[test]
fn giving_up_after_many_collisions_is_a_typed_error() {
    assert_eq!(
        run_naming("fix--", "a", |_| true),
        Err(NamingError::NoFreeName("fix--a".to_string()))
    );
}

#[test]
fn an_assembled_branch_named_head_is_rejected_before_asking_if_it_is_taken() {
    let asked = std::cell::Cell::new(false);
    for (prefix, feature) in [("", "HEAD"), ("HE", "AD"), ("", "HEAD")] {
        let result = run_naming(prefix, feature, |_| {
            asked.set(true);
            false
        });
        assert_eq!(
            result,
            Err(NamingError::InvalidFeatureName(feature.to_string())),
            "{prefix:?}+{feature:?}"
        );
    }
    assert!(!asked.get(), "HEAD must be refused before any collision check");
    assert_eq!(run_naming("", "HEADS", free).unwrap().branch, "HEADS");
    assert_eq!(run_naming("fix--", "HEAD", free).unwrap().branch, "fix--HEAD");
}
