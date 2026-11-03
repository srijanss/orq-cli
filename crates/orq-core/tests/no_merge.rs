use std::path::{Path, PathBuf};

use orq_core::no_merge::{forbidden_git_push, forbidden_provider_call};

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

#[test]
fn provider_calls_that_merge_approve_or_automate_a_merge_are_forbidden() {
    for call in [
        ("gh", vec!["pr", "merge", "12"]),
        ("gh", vec!["pr", "merge", "12", "--auto"]),
        ("/usr/bin/gh", vec!["--repo", "o/r", "pr", "merge", "12"]),
        ("glab", vec!["mr", "merge", "3"]),
        ("glab", vec!["mr", "approve", "3"]),
        ("gh", vec!["pr", "review", "12", "--approve"]),
        ("gh", vec!["api", "-X", "PUT", "repos/o/r/pulls/12/merge"]),
        ("gh", vec!["api", "--method=PUT", "repos/o/r/pulls/12/merge"]),
        ("glab", vec!["api", "--method", "PUT", "projects/1/merge_requests/3/merge"]),
        ("glab", vec!["api", "projects/1/merge_requests/3/approve", "-X", "POST"]),
        ("gh", vec!["pr", "edit", "12", "--enable-auto-merge"]),
    ] {
        let verdict = forbidden_provider_call(call.0, &args(&call.1));
        assert!(verdict.is_some(), "{} {:?} must be forbidden", call.0, call.1);
    }
}

#[test]
fn creating_listing_and_viewing_reviews_stays_allowed() {
    for call in [
        ("gh", vec!["pr", "create", "--title", "merge strategy notes", "--head", "feat"]),
        ("gh", vec!["pr", "list", "--state", "all"]),
        ("gh", vec!["pr", "view", "feat", "--json", "number,url"]),
        ("glab", vec!["mr", "view", "feat", "--output", "json"]),
        ("glab", vec!["mr", "create", "--title", "approve the plan"]),
        ("git", vec!["merge-base", "--is-ancestor", "a", "b"]),
    ] {
        assert_eq!(forbidden_provider_call(call.0, &args(&call.1)), None, "{} {:?}", call.0, call.1);
    }
}

#[test]
fn pushing_to_a_protected_target_branch_is_forbidden_but_a_feature_branch_is_not() {
    let protected = args(&["main", "release"]);
    assert_eq!(forbidden_git_push(&args(&["push", "origin", "feat/x"]), &protected), None);
    assert_eq!(forbidden_git_push(&args(&["push", "-u", "origin", "HEAD:feat/x"]), &protected), None);
    assert_eq!(forbidden_git_push(&args(&["fetch", "origin", "main"]), &protected), None);
    for bad in [
        vec!["push", "origin", "main"],
        vec!["push", "origin", "HEAD:main"],
        vec!["push", "origin", "feat:refs/heads/main"],
        vec!["push", "--force", "origin", "+HEAD:release"],
        vec!["push", "origin", "--delete", "main"],
        vec!["push", "origin", ":main"],
        vec!["push", "--mirror", "origin"],
        vec!["push", "--all", "origin"],
    ] {
        assert!(forbidden_git_push(&args(&bad), &protected).is_some(), "{bad:?}");
    }
}

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

fn crate_sources(krate: &str) -> Vec<(PathBuf, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(krate).join("src");
    let mut files = Vec::new();
    if dir.exists() {
        sources(&dir, &mut files);
    }
    files.into_iter().map(|p| { let text = std::fs::read_to_string(&p).unwrap(); (p, text) }).collect()
}

#[test]
fn no_production_code_path_can_invoke_a_merge() {
    let forbidden = [
        "\"merge\"",
        "\"approve\"",
        "\"--auto\"",
        "\"--admin\"",
        "merge_when_pipeline_succeeds",
        "pr merge",
        "mr merge",
        "/merge\"",
        "/approve\"",
        "enable-auto-merge",
    ];
    let mut seen = 0;
    for krate in ["orq-agents", "orq-cli", "orq-core", "orq-features", "orq-git", "orq-herdr", "orq-orchestrator", "orq-tui"] {
        for (path, text) in crate_sources(krate) {
            if path.ends_with("no_merge.rs") {
                continue; // the guard itself has to name what it forbids
            }
            seen += 1;
            for needle in forbidden {
                assert!(!text.contains(needle), "{} contains the merge-capable text {needle}", path.display());
            }
        }
    }
    assert!(seen > 10, "the scan found only {seen} source files, so it is not looking where the code is");
}

#[test]
fn the_terminal_ui_exposes_no_approve_or_merge_control() {
    let mut seen = 0;
    for krate in ["orq-tui", "orq-herdr"] {
        for (path, text) in crate_sources(krate) {
            seen += 1;
            let lower = text.to_lowercase();
            for needle in ["approve", "auto-merge", "automerge", "\"merge\"", "\"merge ", "merge now", "merge it"] {
                assert!(!lower.contains(needle), "{} mentions the control {needle}", path.display());
            }
        }
    }
    assert!(seen > 0, "no UI source files were scanned");
}

#[test]
fn approval_and_auto_merge_flags_are_caught_in_short_and_assigned_forms() {
    for call in [
        ("gh", vec!["pr", "review", "12", "-a"]),
        ("gh", vec!["pr", "review", "-a", "12"]),
        ("gh", vec!["pr", "review", "12", "--approve=true"]),
        ("gh", vec!["pr", "edit", "12", "--enable-auto-merge=true"]),
        ("gh", vec!["pr", "view", "12", "--admin=true"]),
    ] {
        assert!(forbidden_provider_call(call.0, &args(&call.1)).is_some(), "{} {:?} must be forbidden", call.0, call.1);
    }
    // Commenting through the same subcommand stays allowed.
    assert_eq!(forbidden_provider_call("gh", &args(&["pr", "review", "12", "--comment", "-b", "looks fine"])), None);
}

#[test]
fn ordinary_text_that_mentions_automerge_is_not_a_graphql_mutation() {
    for call in [
        ("gh", vec!["pr", "create", "--title", "Fix automerge documentation", "--body", "mergePullRequest is a mutation"]),
        ("gh", vec!["repo", "view", "o/automerge"]),
        ("gh", vec!["api", "graphql", "-f", "query={ repository(name: \"automerge\") { id } }"]),
    ] {
        assert_eq!(forbidden_provider_call(call.0, &args(&call.1)), None, "{} {:?}", call.0, call.1);
    }
    // A real mutation through `api graphql` is still refused.
    let mutation = args(&["api", "graphql", "-f", "query=mutation { mergePullRequest(input: {pullRequestId: \"x\"}) { clientMutationId } }"]);
    assert!(forbidden_provider_call("gh", &mutation).is_some());
}

#[test]
fn implicit_matching_and_wildcard_pushes_are_forbidden_because_their_destinations_are_unknown() {
    let protected = args(&["main"]);
    for push in [
        vec!["push", "origin"],
        vec!["push"],
        vec!["push", "origin", ":"],
        vec!["push", "origin", "refs/heads/*:refs/heads/*"],
        vec!["push", "origin", "HEAD"],
        vec!["push", "origin", "feat:"],
        vec!["push", "--tags", "origin"],
    ] {
        assert!(forbidden_git_push(&args(&push), &protected).is_some(), "{push:?} must be forbidden");
    }
    // An explicit single-branch destination stays allowed.
    assert_eq!(forbidden_git_push(&args(&["push", "--no-follow-tags", "--", "origin", "abc123:refs/heads/fix--a"]), &protected), None);
}

#[test]
fn a_graphql_mutation_is_caught_wherever_the_endpoint_sits_among_the_options() {
    let mutation = "query=mutation { mergePullRequest(input: {pullRequestId: \"x\"}) { clientMutationId } }";
    for call in [
        vec!["api", "-f", mutation, "graphql"],
        vec!["api", "--raw-field", mutation, "graphql"],
        vec!["api", "--field", mutation, "graphql"],
        vec!["api", "graphql", "--field", mutation],
    ] {
        assert!(forbidden_provider_call("gh", &args(&call)).is_some(), "{call:?} must be forbidden");
    }
}

#[test]
fn an_attached_short_option_value_is_not_mistaken_for_an_approval_flag() {
    for call in [
        vec!["pr", "review", "12", "--comment", "-bThanks"],
        vec!["-Rowner/repo-a", "pr", "review", "12", "--comment"],
        vec!["pr", "review", "12", "--comment", "-F", "notes.md"],
    ] {
        assert_eq!(forbidden_provider_call("gh", &args(&call)), None, "{call:?} must stay allowed");
    }
    for call in [vec!["pr", "review", "12", "-a"], vec!["pr", "review", "12", "-ab", "x"], vec!["pr", "review", "-ca", "12"]] {
        assert!(forbidden_provider_call("gh", &args(&call)).is_some(), "{call:?} must be forbidden");
    }
}

#[test]
fn a_push_option_value_is_not_counted_as_the_remote_or_a_branch() {
    let protected = args(&["main"]);
    for push in [
        vec!["push", "-o", "ci.skip", "origin"],
        vec!["push", "--push-option", "ci.skip", "origin"],
        vec!["push", "--receive-pack", "git-receive-pack", "origin"],
        vec!["push", "--repo", "origin"],
        vec!["push", "-o", "ci.skip", "origin", "main"],
    ] {
        assert!(forbidden_git_push(&args(&push), &protected).is_some(), "{push:?} must be forbidden");
    }
    for push in [
        vec!["push", "-o", "ci.skip", "origin", "feat/x"],
        vec!["push", "--push-option=ci.skip", "origin", "feat/x"],
    ] {
        assert_eq!(forbidden_git_push(&args(&push), &protected), None, "{push:?} must stay allowed");
    }
}

#[test]
fn an_unrecognised_push_option_spelling_fails_closed() {
    let protected = args(&["main"]);
    for push in [
        vec!["push", "--push-opt", "ci.skip", "origin"],
        vec!["push", "--push-opt=ci.skip", "origin", "feat/x"],
        vec!["push", "-vo", "ci.skip", "origin"],
        vec!["push", "-ovalue", "origin", "feat/x"],
        vec!["push", "--rece", "x", "origin"],
        vec!["push", "--no-such-option", "origin", "feat/x"],
    ] {
        assert!(forbidden_git_push(&args(&push), &protected).is_some(), "{push:?} must be forbidden");
    }
    for push in [
        vec!["push", "-u", "origin", "feat/x"],
        vec!["push", "-fu", "origin", "feat/x"],
        vec!["push", "--force-with-lease=feat/x:abc", "origin", "feat/x"],
        vec!["push", "--verbose", "--no-verify", "origin", "feat/x"],
    ] {
        assert_eq!(forbidden_git_push(&args(&push), &protected), None, "{push:?} must stay allowed");
    }
}
