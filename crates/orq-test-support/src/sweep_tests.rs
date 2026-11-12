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
