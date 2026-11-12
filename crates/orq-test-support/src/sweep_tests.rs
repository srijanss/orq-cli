use super::*;
use crate::TempDir;
use std::fs;

fn staged_and_taken(temp: &TempDir) -> (std::path::PathBuf, std::path::PathBuf) {
    let staged = temp.join("staged");
    fs::create_dir(&staged).unwrap();
    fs::write(staged.join("mine"), "x").unwrap();
    // Another process published its empty root under this name a moment ago.
    let taken = temp.join("orq-tests-7-0");
    fs::create_dir(&taken).unwrap();
    (staged, taken)
}

#[test]
fn publishing_never_replaces_a_directory_already_at_the_name() {
    let temp = TempDir::new("orq-publish-fixture");
    let (staged, taken) = staged_and_taken(&temp);

    assert!(matches!(publish(&staged, &taken), Published::Taken), "an existing root was replaced");
    assert!(staged.join("mine").is_file(), "the staged root moved");
    assert_eq!(fs::read_dir(&taken).unwrap().count(), 0, "the other root changed");

    let free = temp.join("orq-tests-7-1");
    assert!(matches!(publish(&staged, &free), Published::Done));
    assert!(free.join("mine").is_file() && !staged.exists());
}

#[test]
fn the_fallback_for_systems_without_a_no_replace_rename_never_replaces_either() {
    let temp = TempDir::new("orq-publish-fixture");
    let (staged, taken) = staged_and_taken(&temp);

    assert!(matches!(publish_by_claiming_the_name(&staged, &taken), Published::Taken), "an existing root was replaced");
    assert!(staged.join("mine").is_file(), "the staged root moved");
    assert_eq!(fs::read_dir(&taken).unwrap().count(), 0, "the other root changed");

    let free = temp.join("orq-tests-7-1");
    assert!(matches!(publish_by_claiming_the_name(&staged, &free), Published::Done));
    assert!(free.join("mine").is_file() && !staged.exists());
}

#[test]
fn a_failure_other_than_a_taken_name_is_reported_as_one_not_retried_as_a_collision() {
    let temp = TempDir::new("orq-publish-fixture");
    let (staged, _) = staged_and_taken(&temp);
    let nowhere = temp.join("missing-parent").join("orq-tests-7-2");

    for published in [publish(&staged, &nowhere), publish_by_claiming_the_name(&staged, &nowhere)] {
        match published {
            Published::Failed(error) => assert_eq!(error.kind(), std::io::ErrorKind::NotFound),
            _ => panic!("a missing parent was not reported as a failure"),
        }
    }
    assert!(staged.join("mine").is_file(), "the staged root moved");
}

#[test]
fn a_root_is_swept_only_when_well_named_a_real_directory_this_users_and_its_process_is_gone() {
    let me = 1000;
    let gone = |_| true;
    let dir_of = |owner| move || Some((true, owner));
    assert!(sweepable("orq-tests-42-0", dir_of(me), me, gone));
    assert!(!sweepable("orq-tests-42-0", dir_of(0), me, gone), "another user's root");
    assert!(!sweepable("orq-tests-42-0", || Some((false, me)), me, gone), "a symlink or file");
    assert!(!sweepable("orq-tests-42-0", || None, me, gone), "an entry already gone");
    assert!(!sweepable("orq-tests-42-0", dir_of(me), me, |_| false), "its process exists");
}

#[test]
fn an_entry_not_named_like_a_root_is_never_looked_up() {
    for name in ["orq-tests-x-0", "orq-tests-0-0", "orq-tests--1-0", "orq-tests-42-x", "orq-tests-42", "orq-other-42-0", "unrelated.sock"] {
        let looked_up = std::cell::Cell::new(false);
        let metadata = || {
            looked_up.set(true);
            Some((true, 1000))
        };
        assert!(!sweepable(name, metadata, 1000, |_| true), "{name}");
        assert!(!looked_up.get(), "{name} was looked up");
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
