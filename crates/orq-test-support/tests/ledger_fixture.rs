use orq_core::FeatureStatus;
use orq_features::parse_ledger;
use orq_test_support::{write_ledger, write_ledger_bytes, GitFixture, LedgerEntry};

#[test]
fn written_entries_parse_back_from_a_repo_and_a_linked_worktree() {
    let repo = GitFixture::new();
    let worktree = repo.add_linked_worktree("feature");
    let entries = [
        LedgerEntry::new("alpha", "pending").description("first thing"),
        LedgerEntry::new("beta", "draft").depends_on(&["alpha"]),
    ];

    for dir in [repo.root(), worktree.as_path()] {
        let path = write_ledger(dir, &entries);

        assert_eq!(path, dir.join(".tdd-features.json"));
        let parsed = parse_ledger(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].feature_name, "alpha");
        assert_eq!(parsed[0].description.as_deref(), Some("first thing"));
        assert_eq!(parsed[0].status.known(), Some(FeatureStatus::Pending));
        assert_eq!(parsed[1].depends_on, vec!["alpha".to_string()]);
        assert_eq!(parsed[1].status.known(), Some(FeatureStatus::Draft));
    }
}

#[test]
fn raw_bytes_are_written_verbatim_even_when_not_valid_json_or_utf8() {
    let repo = GitFixture::new();
    let worktree = repo.add_linked_worktree("raw");
    let bytes: &[u8] = b"[{\"featureName\": \xff\xfe not json";

    for dir in [repo.root(), worktree.as_path()] {
        let path = write_ledger_bytes(dir, bytes);

        assert_eq!(path, dir.join(".tdd-features.json"));
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert!(parse_ledger(&String::from_utf8_lossy(bytes)).is_err());
    }
}
