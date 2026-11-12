use super::*;
use crate::TempDir;
use std::fs;

#[test]
fn publishing_never_replaces_a_directory_already_at_the_name() {
    let temp = TempDir::new("orq-publish-fixture");
    let staged = temp.join("staged");
    fs::create_dir(&staged).unwrap();
    fs::write(staged.join("mine"), "x").unwrap();
    // Another process published its empty root under this name a moment ago.
    let taken = temp.join("orq-tests-7-0");
    fs::create_dir(&taken).unwrap();

    assert!(!publish(&staged, &taken), "an existing root was replaced");
    assert!(staged.join("mine").is_file(), "the staged root moved");
    assert_eq!(fs::read_dir(&taken).unwrap().count(), 0, "the other root changed");

    let free = temp.join("orq-tests-7-1");
    assert!(publish(&staged, &free));
    assert!(free.join("mine").is_file() && !staged.exists());
}

#[test]
fn a_root_is_swept_only_when_well_named_a_real_directory_this_users_and_its_process_is_gone() {
    let me = 1000;
    let gone = |_| true;
    assert!(sweepable("orq-tests-42-0", true, me, me, gone));
    assert!(!sweepable("orq-tests-42-0", true, 0, me, gone), "another user's root");
    assert!(!sweepable("orq-tests-42-0", false, me, me, gone), "a symlink or file");
    assert!(!sweepable("orq-tests-42-0", true, me, me, |_| false), "its process exists");
    for name in ["orq-tests-x-0", "orq-tests-0-0", "orq-tests--1-0", "orq-tests-42-x", "orq-tests-42", "orq-other-42-0"] {
        assert!(!sweepable(name, true, me, me, gone), "{name}");
    }
}

#[test]
fn a_process_counts_as_gone_only_when_signal_zero_answers_no_such_process() {
    const EPERM: i32 = 1;
    const ESRCH: i32 = 3;
    assert!(gone_after_probe(-1, Some(ESRCH)));
    assert!(!gone_after_probe(-1, Some(EPERM)), "another user's process exists");
    assert!(!gone_after_probe(0, None), "a process this user may signal exists");
    assert!(!gone_after_probe(-1, None), "an unknown failure is not proof it is gone");
}
