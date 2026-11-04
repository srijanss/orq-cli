use std::process::{Command, Output};

use orq_test_support::{write_ledger, GitFixture, LedgerEntry};

fn run_args(repo: &std::path::Path, extra: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["run", "--repo"])
        .arg(repo)
        .args(["--remote", "origin", "--target", "main"])
        .args(extra)
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .output()
        .unwrap()
}

fn repo_with(ids: &[&str]) -> GitFixture {
    let repo = GitFixture::new();
    let entries: Vec<LedgerEntry> = ids.iter().map(|id| LedgerEntry::new(id, "pending")).collect();
    write_ledger(repo.root(), &entries);
    repo
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn a_group_is_a_comma_separated_list_of_distinct_plain_feature_ids() {
    let repo = repo_with(&["COM-01", "COM-02"]);
    for bad in ["", ",", "COM-01,", ",COM-01", "COM-01,,COM-02", "COM-01,COM-01", "-x,COM-01", "COM-01,--help", "a b,COM-01", "a;b,COM-01", "a\nb,COM-01"] {
        let output = run_args(repo.root(), &["--group", bad]);
        assert_eq!(output.status.code(), Some(2), "{bad:?}: {}", stderr(&output));
        assert!(stderr(&output).contains("--group"), "{bad:?}: {}", stderr(&output));
    }
}

#[test]
fn a_group_and_a_single_feature_cannot_be_given_together() {
    let repo = repo_with(&["COM-01"]);
    let output = run_args(repo.root(), &["--group", "COM-01", "--feature", "COM-01"]);
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(stderr(&output).contains("--group") && stderr(&output).contains("--feature"), "{}", stderr(&output));
}

#[test]
fn a_group_of_one_is_checked_exactly_like_a_single_feature() {
    let repo = repo_with(&["COM-01"]);
    // A feature the ledger does not know is ineligible, by --group as by --feature.
    let by_feature = run_args(repo.root(), &["--feature", "NOPE"]);
    let by_group = run_args(repo.root(), &["--group", "NOPE"]);
    assert_eq!(by_feature.status.code(), Some(20), "{}", stderr(&by_feature));
    assert_eq!(by_group.status.code(), Some(20), "{}", stderr(&by_group));
    assert!(stderr(&by_group).contains("NOPE"), "{}", stderr(&by_group));
}

#[test]
fn a_group_that_cannot_run_together_is_refused_by_member_before_anything_is_created() {
    let repo = GitFixture::new();
    write_ledger(
        repo.root(),
        &[
            LedgerEntry::new("base", "pending"),
            LedgerEntry::new("a", "pending").depends_on(&["base"]),
            LedgerEntry::new("done", "completed"),
            LedgerEntry::new("c", "pending"),
            LedgerEntry::new("x", "pending").depends_on(&["y"]),
            LedgerEntry::new("y", "pending").depends_on(&["x"]),
        ],
    );
    for (group, mentions) in [("c,done", "done"), ("c,ghost", "ghost"), ("c,a", "unmet dependencies: base"), ("x,y", "depend on each other")] {
        let output = run_args(repo.root(), &["--group", group]);
        assert_eq!(output.status.code(), Some(20), "{group}: {}", stderr(&output));
        assert!(stderr(&output).contains(mentions), "{group}: {}", stderr(&output));
    }
}
