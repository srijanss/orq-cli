use orq_features::ledger_path;
use orq_git::resolve_project;
use orq_test_support::{write_ledger, GitFixture, LedgerEntry};

#[test]
fn a_local_ledger_that_cannot_be_inspected_does_not_fall_back_to_the_main_checkout() {
    let fixture = GitFixture::new();
    write_ledger(fixture.root(), &[LedgerEntry::new("a", "draft")]);
    let worktree = fixture.add_linked_worktree("wt");
    // A symlink loop: the entry exists but its metadata cannot be read.
    let local = worktree.join(".tdd-features.json");
    std::os::unix::fs::symlink(&local, &local).unwrap();

    let project = resolve_project(&worktree).unwrap();

    assert_eq!(ledger_path(&project), project.root.join(".tdd-features.json"));
}
