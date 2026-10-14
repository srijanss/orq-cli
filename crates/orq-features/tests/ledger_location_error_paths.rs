use orq_features::{ledger_path, load_ledger, LedgerError};
use orq_git::resolve_project;
use orq_test_support::{write_ledger, GitFixture, LedgerEntry};

fn entry() -> Vec<LedgerEntry> {
    vec![LedgerEntry::new("a", "draft")]
}

#[test]
fn neither_ledger_existing_reports_missing_at_the_main_checkouts_path() {
    let fixture = GitFixture::new();
    let worktree = fixture.add_linked_worktree("wt");
    let project = resolve_project(&worktree).unwrap();
    let main = resolve_project(fixture.root()).unwrap();

    let path = ledger_path(&project);
    match load_ledger(&path) {
        Err(LedgerError::Missing { path: reported }) => {
            assert_eq!(reported, main.root.join(".tdd-features.json"));
        }
        other => panic!("expected Missing, got {other:?}"),
    }
}

#[test]
fn a_malformed_local_ledger_is_reported_even_when_main_has_a_valid_one() {
    let fixture = GitFixture::new();
    write_ledger(fixture.root(), &entry());
    let worktree = fixture.add_linked_worktree("wt");
    std::fs::write(worktree.join(".tdd-features.json"), "{ not json").unwrap();
    let project = resolve_project(&worktree).unwrap();

    let path = ledger_path(&project);
    assert_eq!(path, project.root.join(".tdd-features.json"));
    assert!(matches!(load_ledger(&path), Err(LedgerError::Malformed { .. })));
}

#[test]
fn an_unreadable_local_ledger_is_reported_even_when_main_has_a_valid_one() {
    let fixture = GitFixture::new();
    write_ledger(fixture.root(), &entry());
    let worktree = fixture.add_linked_worktree("wt");
    // A directory where the ledger file should be: it exists but cannot be read.
    std::fs::create_dir(worktree.join(".tdd-features.json")).unwrap();
    let project = resolve_project(&worktree).unwrap();

    let path = ledger_path(&project);
    assert_eq!(path, project.root.join(".tdd-features.json"));
    assert!(matches!(load_ledger(&path), Err(LedgerError::Unreadable { .. })));
}
