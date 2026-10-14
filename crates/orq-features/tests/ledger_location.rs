use orq_features::ledger_path;
use orq_git::resolve_project;
use orq_test_support::{write_ledger, GitFixture, LedgerEntry};

fn entry() -> Vec<LedgerEntry> {
    vec![LedgerEntry::new("a", "draft")]
}

#[test]
fn a_main_checkout_uses_its_own_ledger_path() {
    let fixture = GitFixture::new();
    let project = resolve_project(fixture.root()).unwrap();
    assert_eq!(ledger_path(&project), project.root.join(".tdd-features.json"));
}

#[test]
fn a_worktree_with_its_own_ledger_uses_it() {
    let fixture = GitFixture::new();
    write_ledger(fixture.root(), &entry());
    let worktree = fixture.add_linked_worktree("wt");
    write_ledger(&worktree, &entry());
    let project = resolve_project(&worktree).unwrap();
    assert_eq!(ledger_path(&project), project.root.join(".tdd-features.json"));
}

#[test]
fn a_worktree_without_a_ledger_falls_back_to_the_main_checkouts() {
    let fixture = GitFixture::new();
    write_ledger(fixture.root(), &entry());
    let worktree = fixture.add_linked_worktree("wt");
    let project = resolve_project(&worktree).unwrap();
    let main = resolve_project(fixture.root()).unwrap();
    assert_eq!(ledger_path(&project), main.root.join(".tdd-features.json"));
}

#[test]
fn an_inherited_tdd_features_path_is_ignored() {
    let fixture = GitFixture::new();
    let other = GitFixture::new();
    let foreign = write_ledger(other.root(), &entry());
    std::env::set_var("TDD_FEATURES_PATH", &foreign);
    let project = resolve_project(fixture.root()).unwrap();
    assert_eq!(ledger_path(&project), project.root.join(".tdd-features.json"));
}
