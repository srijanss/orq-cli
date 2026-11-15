//! A run's implementer has nobody to answer a permission prompt, so the worktree's session settings
//! say what it may do: accept its edits, run the local commands and MCP tools a run needs, never
//! publish to a remote, and write its exit marker into the run's logs.

use std::fs;
use std::path::Path;

use orq_orchestrator::{refresh_run_permissions, write_run_permissions};
use orq_test_support::GitFixture;

fn checkout(tag: &str) -> (GitFixture, std::path::PathBuf) {
    let repo = GitFixture::new();
    let worktree = repo.add_linked_worktree(tag);
    (repo, worktree)
}

fn read_json(path: &Path) -> serde_json::Value {
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

fn strings(value: &serde_json::Value) -> Vec<String> {
    serde_json::from_value(value.clone()).unwrap()
}

fn git(worktree: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(["-c", "user.name=Orq Test", "-c", "user.email=test@example.invalid"])
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

/// The commit the worktree is at: the run's base, until the run commits.
fn head(worktree: &Path) -> String {
    git(worktree, &["rev-parse", "HEAD"])
}

/// Writes `path` in the worktree and commits it.
fn commit_file(worktree: &Path, path: &str, contents: &str) {
    let file = worktree.join(path);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, contents).unwrap();
    git(worktree, &["add", path]);
    git(worktree, &["commit", "--quiet", "-m", path]);
}

fn allowed(worktree: &Path) -> Vec<String> {
    strings(&read_json(&worktree.join(".claude/settings.local.json"))["permissions"]["allow"])
}

/// An interpreter running a fixed script of the repository cannot be told to publish: the script is
/// what the run's base holds, and the run's own changes to it are seen.
#[test]
fn an_interpreter_rule_running_a_checked_in_script_is_kept() {
    let (_repo, worktree) = checkout("permissions-script");
    commit_file(&worktree, "a/build.mjs", "console.log('built')\n");
    let base = head(&worktree);
    let extra = vec!["Bash(node a/build.mjs:*)".to_string(), "Bash(python3 a/build.mjs)".to_string()];

    let notes = write_run_permissions(&worktree, &extra, Path::new("/logs/r"), &base);

    assert_eq!(notes, Vec::<String>::new());
    let allow = allowed(&worktree);
    for rule in &extra {
        assert!(allow.contains(rule), "{rule} not in {allow:?}");
    }
}

#[test]
fn the_permissions_join_the_settings_beside_what_is_already_there_and_are_not_repeated() {
    let (_repo, worktree) = checkout("permissions-merge");
    fs::create_dir_all(worktree.join(".claude")).unwrap();
    let path = worktree.join(".claude/settings.local.json");
    fs::write(&path, r#"{"env":{"A":"1"},"permissions":{"allow":["WebFetch"],"additionalDirectories":["/elsewhere"]}}"#).unwrap();
    let logs = Path::new("/state/orq/logs/run-1");

    let extra = vec!["Bash(make lint:*)".to_string()];
    assert_eq!(write_run_permissions(&worktree, &extra, logs, &head(&worktree)), Vec::<String>::new());
    assert_eq!(write_run_permissions(&worktree, &extra, logs, &head(&worktree)), Vec::<String>::new());

    let settings = read_json(&path);
    assert_eq!(settings["env"]["A"], "1", "other keys stay");
    let permissions = &settings["permissions"];
    assert_eq!(permissions["defaultMode"], "acceptEdits");
    let allow = strings(&permissions["allow"]);
    for wanted in ["WebFetch", "Bash(git add:*)", "Bash(git commit:*)", "Bash(git status:*)", "Bash(cargo test:*)", "Bash(npm ci)", "mcp__outside-in-tdd", "mcp__project-mcp", "Bash(make lint:*)"] {
        assert_eq!(allow.iter().filter(|rule| *rule == wanted).count(), 1, "{wanted} once in {allow:?}");
    }
    let deny = strings(&permissions["deny"]);
    for wanted in ["Bash(git push:*)", "Bash(cargo publish:*)", "Bash(npm publish:*)"] {
        assert_eq!(deny.iter().filter(|rule| *rule == wanted).count(), 1, "{wanted} once in {deny:?}");
    }
    assert_eq!(strings(&permissions["additionalDirectories"]), ["/elsewhere", "/state/orq/logs/run-1"]);
}

/// Publishing is orq's own step: a project cannot allow the session to do it.
#[test]
fn an_added_rule_that_would_publish_to_a_remote_is_left_out_with_a_note() {
    let (_repo, worktree) = checkout("permissions-publish");
    let extra = vec!["Bash(git push origin:*)".to_string(), "Bash(cargo publish)".to_string(), "Bash(make lint:*)".to_string()];

    let notes = write_run_permissions(&worktree, &extra, Path::new("/logs/r"), &head(&worktree));

    assert_eq!(notes.len(), 2, "{notes:?}");
    assert!(notes.iter().all(|note| note.contains("allowed_tools")), "{notes:?}");
    let allow = strings(&read_json(&worktree.join(".claude/settings.local.json"))["permissions"]["allow"]);
    assert!(allow.iter().any(|rule| rule == "Bash(make lint:*)"), "{allow:?}");
    assert!(!allow.iter().any(|rule| rule.contains("push") || rule.contains("publish")), "{allow:?}");
}

/// A rule broad enough to reach publishing another way, a whole program that can publish or a
/// shell that can run anything, is left out too; a narrow one stays.
#[test]
fn an_added_rule_broad_enough_to_publish_another_way_is_left_out() {
    let (_repo, worktree) = checkout("permissions-broad");
    let broad = ["Bash", "Bash(*)", "Bash(git:*)", "Bash(git *)", "Bash(git -C:*)", "Bash(cargo:*)", "Bash(npm *)", "Bash(gh:*)", "Bash(sh:*)", "Bash(bash -c:*)", "Bash(env:*)", "Bash(xargs:*)"];
    let narrow = ["Bash(make lint:*)", "Bash(cargo nextest:*)", "Bash(git fetch:*)", "WebFetch"];
    let extra: Vec<String> = broad.iter().chain(narrow.iter()).map(|rule| rule.to_string()).collect();

    let notes = write_run_permissions(&worktree, &extra, Path::new("/logs/r"), &head(&worktree));

    assert_eq!(notes.len(), broad.len(), "{notes:?}");
    let allow = strings(&read_json(&worktree.join(".claude/settings.local.json"))["permissions"]["allow"]);
    for rule in broad {
        assert!(!allow.iter().any(|allowed| allowed == rule), "{rule} in {allow:?}");
    }
    for rule in narrow {
        assert!(allow.iter().any(|allowed| allowed == rule), "{rule} not in {allow:?}");
    }
}

/// A program named by its path is the same program, and an interpreter runs any code it is given.
#[test]
fn a_rule_naming_a_program_by_its_path_or_an_interpreter_is_left_out() {
    let (_repo, worktree) = checkout("permissions-paths");
    let broad = ["Bash(/bin/bash:*)", "Bash(/usr/bin/git:*)", "Bash(./git -C:*)", "Bash(python3:*)", "Bash(node -e:*)", "Bash(perl:*)", "Bash(ruby:*)", "Bash(/usr/bin/env python3:*)"];
    let narrow = ["Bash(/usr/bin/make lint:*)", "Bash(/usr/bin/git fetch:*)"];
    let extra: Vec<String> = broad.iter().chain(narrow.iter()).map(|rule| rule.to_string()).collect();

    let notes = write_run_permissions(&worktree, &extra, Path::new("/logs/r"), &head(&worktree));

    assert_eq!(notes.len(), broad.len(), "{notes:?}");
    let allow = strings(&read_json(&worktree.join(".claude/settings.local.json"))["permissions"]["allow"]);
    for rule in narrow {
        assert!(allow.iter().any(|allowed| allowed == rule), "{rule} not in {allow:?}");
    }
}

/// Code given on the command line, flags before the script, a file outside the base commit or a
/// path that could name another file are not a checked-in script; nor is a shell running one.
#[test]
fn an_interpreter_rule_on_anything_but_a_checked_in_script_is_left_out_saying_why() {
    let (_repo, worktree) = checkout("permissions-not-script");
    commit_file(&worktree, "a/build.mjs", "console.log('built')\n");
    commit_file(&worktree, "a/x.sh", "echo hi\n");
    let base = head(&worktree);
    fs::write(worktree.join("a/untracked.mjs"), "console.log('new')\n").unwrap();
    std::os::unix::fs::symlink("/etc/passwd", worktree.join("a/link.mjs")).unwrap();
    git(&worktree, &["add", "a/link.mjs"]);
    git(&worktree, &["commit", "--quiet", "-m", "link"]);
    let absolute = format!("Bash(node {}:*)", worktree.join("a/build.mjs").display());
    let interpreters = [
        "Bash(node a/untracked.mjs:*)", "Bash(node a/missing.mjs)", absolute.as_str(), "Bash(node ../x.mjs)",
        "Bash(node a/../a/build.mjs)", "Bash(node a/*.mjs)", "Bash(node a/build.mjs*)", "Bash(node a/build.mjs *)",
        "Bash(node -e:*)", "Bash(python3 -c:*)", "Bash(node --require x a/build.mjs)", "Bash(node a)",
        "Bash(/usr/bin/node a/build.mjs)", "Bash(node a/link.mjs)",
    ];
    let extra: Vec<String> = interpreters.iter().map(|rule| rule.to_string()).chain(["Bash(bash a/x.sh)".to_string(), "Bash(git push:*)".to_string()]).collect();

    let notes = write_run_permissions(&worktree, &extra, Path::new("/logs/r"), &base);

    assert_eq!(notes.len(), extra.len(), "{notes:?}");
    for rule in interpreters {
        let wanted = format!("allowed_tools rule {rule} runs an interpreter on something other than a checked-in script; it was left out");
        assert!(notes.contains(&wanted), "{wanted} not in {notes:?}");
    }
    let allow = allowed(&worktree);
    for rule in &extra {
        assert!(!allow.contains(rule), "{rule} in {allow:?}");
    }
    let deny = strings(&read_json(&worktree.join(".claude/settings.local.json"))["permissions"]["deny"]);
    assert!(deny.iter().any(|rule| rule == "Bash(git push:*)"), "{deny:?}");
}

/// A symlink the base itself holds is not a checked-in script, even unchanged; nor is a script the
/// run's new base (after a rebase) holds as a symlink where an earlier base held a regular file.
#[test]
fn a_script_the_base_holds_as_a_symlink_is_left_out() {
    let (_repo, worktree) = checkout("permissions-base-link");
    commit_file(&worktree, "a/make.mjs", "console.log('built')\n");
    std::os::unix::fs::symlink("/etc/passwd", worktree.join("a/link.mjs")).unwrap();
    git(&worktree, &["add", "a/link.mjs"]);
    git(&worktree, &["commit", "--quiet", "-m", "link"]);
    let earlier = head(&worktree);
    let linked = "Bash(node a/link.mjs)".to_string();
    let replaced = "Bash(node a/make.mjs:*)".to_string();
    let extra = vec![linked.clone(), replaced.clone()];
    let not_script = |rule: &str| format!("allowed_tools rule {rule} runs an interpreter on something other than a checked-in script; it was left out");

    assert_eq!(write_run_permissions(&worktree, &extra, Path::new("/logs/r"), &earlier), [not_script(&linked)]);
    assert!(allowed(&worktree).contains(&replaced), "{:?}", allowed(&worktree));

    fs::remove_file(worktree.join("a/make.mjs")).unwrap();
    std::os::unix::fs::symlink("/etc/passwd", worktree.join("a/make.mjs")).unwrap();
    git(&worktree, &["add", "a/make.mjs"]);
    git(&worktree, &["commit", "--quiet", "-m", "make is a link"]);
    let rebased = head(&worktree);
    let notes = write_run_permissions(&worktree, &extra, Path::new("/logs/r"), &rebased);

    assert_eq!(notes, [not_script(&linked), not_script(&replaced)]);
    let allow = allowed(&worktree);
    assert!(!allow.contains(&linked) && !allow.contains(&replaced), "{allow:?}");
}

/// A session cannot write itself a push script and run it: a script the run changed, in a commit or
/// still in the worktree, is not run without asking by the next session, even if an earlier one could.
#[test]
fn a_checked_in_script_the_run_changed_is_left_out_for_the_next_session() {
    let (_repo, worktree) = checkout("permissions-changed");
    commit_file(&worktree, "a/make.mjs", "console.log('built')\n");
    commit_file(&worktree, "b/make.mjs", "console.log('built')\n");
    let base = head(&worktree);
    let committed = "Bash(node a/make.mjs:*)".to_string();
    let edited = "Bash(node b/make.mjs)".to_string();
    let extra = vec![committed.clone(), edited.clone()];
    assert_eq!(write_run_permissions(&worktree, &extra, Path::new("/logs/r"), &base), Vec::<String>::new());

    commit_file(&worktree, "a/make.mjs", "require('child_process')\n");
    fs::write(worktree.join("b/make.mjs"), "require('child_process')\n").unwrap();
    let notes = write_run_permissions(&worktree, &extra, Path::new("/logs/r"), &base);

    assert_eq!(notes, [
        format!("a/make.mjs was changed by this run, so allowed_tools rule {committed} was left out"),
        format!("b/make.mjs was changed by this run, so allowed_tools rule {edited} was left out"),
    ]);
    let allow = allowed(&worktree);
    assert!(!allow.contains(&committed) && !allow.contains(&edited), "{allow:?}");
    assert!(allow.iter().any(|rule| rule == "Bash(git status:*)"), "{allow:?}");
}

/// orq's own `Bash(node build.mjs)` runs whichever `build.mjs` is in the folder it is run from, so it
/// is kept only while every `build.mjs` in the worktree is one the base holds checked in, unchanged:
/// one the run changed or added, committed or not, takes the rule out of the settings.
#[test]
fn the_built_in_build_script_rule_is_kept_only_while_every_build_mjs_is_the_bases() {
    let builtin = "Bash(node build.mjs)".to_string();
    let (_repo, worktree) = checkout("permissions-builtin-none");
    assert_eq!(write_run_permissions(&worktree, &[], Path::new("/logs/r"), &head(&worktree)), Vec::<String>::new());
    assert!(!allowed(&worktree).contains(&builtin), "no build.mjs at the base: {:?}", allowed(&worktree));

    for (tag, change, path) in [("changed", "web/build.mjs", "web/build.mjs"), ("added", "other/build.mjs", "other/build.mjs"), ("committed", "x/build.mjs", "x/build.mjs")] {
        let (_repo, worktree) = checkout(&format!("permissions-builtin-{tag}"));
        commit_file(&worktree, "web/build.mjs", "console.log('built')\n");
        let base = head(&worktree);
        assert_eq!(write_run_permissions(&worktree, &[], Path::new("/logs/r"), &base), Vec::<String>::new());
        assert!(allowed(&worktree).contains(&builtin), "{:?}", allowed(&worktree));

        if tag == "committed" {
            commit_file(&worktree, change, "require('child_process')\n");
        } else {
            fs::create_dir_all(worktree.join(change).parent().unwrap()).unwrap();
            fs::write(worktree.join(change), "require('child_process')\n").unwrap();
        }
        let notes = write_run_permissions(&worktree, &[], Path::new("/logs/r"), &base);

        assert_eq!(notes, [format!("{path} was changed by this run, so the rule {builtin} was left out")], "{tag}");
        assert!(!allowed(&worktree).contains(&builtin), "{tag}: {:?}", allowed(&worktree));
    }
}

/// A repository inside the worktree, a submodule or not, keeps its files from the worktree's own git,
/// so a `build.mjs` in it cannot be checked: the built-in rule is left out, naming the repository.
#[test]
fn the_built_in_build_script_rule_is_left_out_beside_a_nested_repository() {
    let builtin = "Bash(node build.mjs)".to_string();
    for tag in ["untracked", "committed", "at-base"] {
        let (_repo, worktree) = checkout(&format!("permissions-nested-{tag}"));
        commit_file(&worktree, "web/build.mjs", "console.log('built')\n");
        let nested = worktree.join("sub");
        fs::create_dir_all(&nested).unwrap();
        git(&nested, &["init", "--quiet"]);
        commit_file(&nested, "build.mjs", "console.log('built')\n");
        if tag != "untracked" {
            git(&worktree, &["add", "sub"]);
            git(&worktree, &["commit", "--quiet", "-m", "sub"]);
        }
        // Committed by the run: the base is the commit before it.
        let base = git(&worktree, &["rev-parse", if tag == "committed" { "HEAD~1" } else { "HEAD" }]);
        fs::write(nested.join("build.mjs"), "require('child_process')\n").unwrap();

        let notes = write_run_permissions(&worktree, &[], Path::new("/logs/r"), &base);

        assert_eq!(notes, [format!("sub is a repository of its own, whose build.mjs orq cannot check, so the rule {builtin} was left out")], "{tag}");
        assert!(!allowed(&worktree).contains(&builtin), "{tag}: {:?}", allowed(&worktree));
    }
}

/// git lists a folder nothing of which is tracked as the folder alone, so a repository deeper in it
/// is found all the same: the built-in rule is left out, naming the repository, not the folder.
#[test]
fn the_built_in_build_script_rule_is_left_out_beside_a_repository_inside_an_untracked_folder() {
    let builtin = "Bash(node build.mjs)".to_string();
    let (_repo, worktree) = checkout("permissions-nested-deep");
    commit_file(&worktree, "web/build.mjs", "console.log('built')\n");
    let base = head(&worktree);
    let nested = worktree.join("vendor/sub");
    fs::create_dir_all(&nested).unwrap();
    git(&nested, &["init", "--quiet"]);
    fs::write(nested.join("build.mjs"), "require('child_process')\n").unwrap();
    fs::write(worktree.join("vendor/note"), "untracked\n").unwrap();

    let notes = write_run_permissions(&worktree, &[], Path::new("/logs/r"), &base);

    assert_eq!(notes, [format!("vendor/sub is a repository of its own, whose build.mjs orq cannot check, so the rule {builtin} was left out")]);
    assert!(!allowed(&worktree).contains(&builtin), "{:?}", allowed(&worktree));
}

/// The project naming orq's own rule in its allowed_tools does not bring it back: the root
/// `build.mjs` is the base's, but another one the run changed takes the rule out all the same.
#[test]
fn the_built_in_rule_left_out_stays_out_when_allowed_tools_names_it_too() {
    let (_repo, worktree) = checkout("permissions-builtin-extra");
    commit_file(&worktree, "build.mjs", "console.log('built')\n");
    commit_file(&worktree, "web/build.mjs", "console.log('built')\n");
    let base = head(&worktree);
    let builtin = "Bash(node build.mjs)".to_string();
    fs::write(worktree.join("web/build.mjs"), "require('child_process')\n").unwrap();

    let notes = write_run_permissions(&worktree, &[builtin.clone()], Path::new("/logs/r"), &base);

    assert_eq!(notes, [format!("web/build.mjs was changed by this run, so the rule {builtin} was left out")]);
    assert!(!allowed(&worktree).contains(&builtin), "{:?}", allowed(&worktree));
}

/// A script's relative path runs whichever file is at it below the folder the session is in, so an
/// allowed_tools rule on a script, with arguments or without, is kept only while every file the path
/// could name is the base's: `build.mjs:*` loses to a changed web/build.mjs, `a/make.mjs` to an added
/// web/a/make.mjs, though the root's own build.mjs and a/make.mjs are unchanged.
#[test]
fn an_allowed_tools_script_rule_is_left_out_when_the_script_it_could_run_elsewhere_was_changed() {
    let (_repo, worktree) = checkout("permissions-script-anywhere");
    commit_file(&worktree, "build.mjs", "console.log('built')\n");
    commit_file(&worktree, "web/build.mjs", "console.log('built')\n");
    commit_file(&worktree, "a/make.mjs", "console.log('built')\n");
    let base = head(&worktree);
    let prefix = "Bash(node build.mjs:*)".to_string();
    let relative = "Bash(node a/make.mjs)".to_string();
    let extra = vec![prefix.clone(), relative.clone()];
    assert_eq!(write_run_permissions(&worktree, &extra, Path::new("/logs/r"), &base), Vec::<String>::new());

    fs::write(worktree.join("web/build.mjs"), "require('child_process')\n").unwrap();
    fs::create_dir_all(worktree.join("web/a")).unwrap();
    fs::write(worktree.join("web/a/make.mjs"), "require('child_process')\n").unwrap();
    let notes = write_run_permissions(&worktree, &extra, Path::new("/logs/r"), &base);

    assert_eq!(notes, [
        "web/build.mjs was changed by this run, so the rule Bash(node build.mjs) was left out".to_string(),
        format!("web/build.mjs was changed by this run, so allowed_tools rule {prefix} was left out"),
        format!("web/a/make.mjs was changed by this run, so allowed_tools rule {relative} was left out"),
    ]);
    let allow = allowed(&worktree);
    assert!(!allow.contains(&prefix) && !allow.contains(&relative), "{allow:?}");
}

/// A link to a folder outside the worktree holds files git does not list: a script reached through
/// it, here web/a/make.mjs, is not the base's, so the rule that could run it is left out.
#[test]
fn an_allowed_tools_script_rule_is_left_out_beside_a_link_to_a_folder_outside_the_worktree() {
    let (repo, worktree) = checkout("permissions-script-linked");
    commit_file(&worktree, "a/make.mjs", "console.log('built')\n");
    let base = head(&worktree);
    let rule = "Bash(node a/make.mjs)".to_string();
    let outside = repo.root().join("outside");
    fs::create_dir_all(outside.join("a")).unwrap();
    fs::write(outside.join("a/make.mjs"), "require('child_process')\n").unwrap();
    fs::create_dir_all(worktree.join("web")).unwrap();
    std::os::unix::fs::symlink(&outside.join("a"), worktree.join("web/a")).unwrap();

    let notes = write_run_permissions(&worktree, std::slice::from_ref(&rule), Path::new("/logs/r"), &base);

    assert_eq!(notes, [format!("web/a is a link to a folder, whose a/make.mjs orq cannot check, so allowed_tools rule {rule} was left out")]);
    assert!(!allowed(&worktree).contains(&rule));
}

/// A link to a folder inside the worktree gives its files a second path: web/a/make.mjs is
/// payload/make.mjs, which no path ending in a/make.mjs names, so the rule is left out all the same.
#[test]
fn an_allowed_tools_script_rule_is_left_out_beside_a_link_to_a_folder_inside_the_worktree() {
    let (_repo, worktree) = checkout("permissions-script-aliased");
    commit_file(&worktree, "a/make.mjs", "console.log('built')\n");
    let base = head(&worktree);
    let rule = "Bash(node a/make.mjs)".to_string();
    fs::create_dir_all(worktree.join("payload")).unwrap();
    fs::write(worktree.join("payload/make.mjs"), "require('child_process')\n").unwrap();
    fs::create_dir_all(worktree.join("web")).unwrap();
    std::os::unix::fs::symlink("../payload", worktree.join("web/a")).unwrap();

    let notes = write_run_permissions(&worktree, std::slice::from_ref(&rule), Path::new("/logs/r"), &base);

    assert_eq!(notes, [format!("web/a is a link to a folder, whose a/make.mjs orq cannot check, so allowed_tools rule {rule} was left out")]);
    assert!(!allowed(&worktree).contains(&rule));
}

/// Each script rule is decided on its own script, the worktree read once for all of them: an
/// unrelated untracked or ignored file leaves every rule as it was.
#[test]
fn script_rules_are_each_decided_on_their_own_script_beside_unrelated_untracked_files() {
    let (_repo, worktree) = checkout("permissions-script-each");
    commit_file(&worktree, "build.mjs", "console.log('built')\n");
    commit_file(&worktree, "a/make.mjs", "console.log('built')\n");
    commit_file(&worktree, "b/run.py", "print('ran')\n");
    let base = head(&worktree);
    let kept = "Bash(node a/make.mjs)".to_string();
    let changed = "Bash(python b/run.py:*)".to_string();
    fs::create_dir_all(worktree.join("node_modules/x")).unwrap();
    fs::write(worktree.join("node_modules/x/index.js"), "module.exports = 1\n").unwrap();
    fs::create_dir_all(worktree.join("web/b")).unwrap();
    fs::write(worktree.join("web/b/run.py"), "import os\n").unwrap();

    let notes = write_run_permissions(&worktree, &[kept.clone(), changed.clone()], Path::new("/logs/r"), &base);

    assert_eq!(notes, [format!("web/b/run.py was changed by this run, so allowed_tools rule {changed} was left out")]);
    let allow = allowed(&worktree);
    assert!(allow.contains(&kept) && allow.contains(&"Bash(node build.mjs)".to_string()) && !allow.contains(&changed), "{allow:?}");
}

/// A rule that must go but cannot be taken out of the settings could still be allowed there: the
/// refresh before a session fails, naming the rule, rather than letting the session start.
#[test]
fn a_rule_the_settings_cannot_lose_fails_the_refresh_naming_it() {
    let (_repo, worktree) = checkout("permissions-unrevoked");
    commit_file(&worktree, "a/make.mjs", "console.log('built')\n");
    let base = head(&worktree);
    let rule = "Bash(node a/make.mjs)".to_string();
    assert_eq!(refresh_run_permissions(&worktree, std::slice::from_ref(&rule), Path::new("/logs/r"), &base), Ok(Vec::new()));

    fs::write(worktree.join("a/make.mjs"), "require('child_process')\n").unwrap();
    let path = worktree.join(".claude/settings.local.json");
    let mut kept = read_json(&path);
    kept["permissions"]["deny"] = serde_json::json!(1);
    fs::write(&path, kept.to_string()).unwrap();

    let refreshed = refresh_run_permissions(&worktree, std::slice::from_ref(&rule), Path::new("/logs/r"), &base);

    let why = refreshed.expect_err("the rule is still in the settings");
    assert!(why.contains(&rule) && why.contains("settings.local.json"), "{why}");
    let unchanged = write_run_permissions(&worktree, &[], Path::new("/logs/r"), &base);
    assert!(unchanged.iter().any(|note| note.contains("settings.local.json")), "nothing to take out is still a note: {unchanged:?}");
}

#[test]
fn settings_the_permissions_cannot_join_are_a_note_and_are_left_as_they_were() {
    let (_repo, worktree) = checkout("permissions-bad");
    fs::create_dir_all(worktree.join(".claude")).unwrap();
    let path = worktree.join(".claude/settings.local.json");
    fs::write(&path, r#"{"permissions":["nonsense"]}"#).unwrap();

    let notes = write_run_permissions(&worktree, &[], Path::new("/logs/r"), &head(&worktree));

    assert!(notes.iter().any(|note| note.contains("settings.local.json")), "{notes:?}");
    assert_eq!(fs::read_to_string(&path).unwrap(), r#"{"permissions":["nonsense"]}"#);
}

/// A session that refuses what it was not allowed still edits the worktree and writes its marker.
#[test]
fn the_session_may_edit_the_worktree_and_write_into_the_runs_logs() {
    let (_repo, worktree) = checkout("permissions-edits");

    assert_eq!(write_run_permissions(&worktree, &[], Path::new("/state/orq/logs/run-1"), &head(&worktree)), Vec::<String>::new());

    let allow = strings(&read_json(&worktree.join(".claude/settings.local.json"))["permissions"]["allow"]);
    for wanted in ["Edit(./**)", "Edit(//state/orq/logs/run-1/**)", "Read", "Skill"] {
        assert!(allow.iter().any(|rule| rule == wanted), "{wanted} in {allow:?}");
    }
    assert!(!allow.iter().any(|rule| rule == "Edit"), "no edit anywhere: {allow:?}");
}
