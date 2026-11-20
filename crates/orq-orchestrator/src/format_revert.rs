//! A session formats only its own files: after it, a Rust file outside its features' declared files
//! whose committed change is formatting only (rustfmt of the base version gives the new one) is put
//! back as the base held it, in a commit of its own, and `session.format_reverted {files}` recorded.

use std::collections::{BTreeSet, HashMap};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::implement::{next_event_id, now_rfc3339};
use crate::store::{Event, RunRecord, RunStore};

/// The message of the commit that puts the files back.
pub const FORMAT_REVERT_MESSAGE: &str = "chore: undo formatting outside the feature's files";
/// The event naming the files put back.
pub const FORMAT_REVERTED: &str = "session.format_reverted";

/// Puts back each Rust file the commits since `base` changed (modified, not added or removed) that
/// `declared` does not name and whose change is formatting only, in a commit
/// [`FORMAT_REVERT_MESSAGE`], and records [`FORMAT_REVERTED`] for the stage `stage`. A file with
/// uncommitted changes is left alone. Returns the files put back, none when nothing was; an error
/// says what git or the store could not do.
pub fn undo_formatting_outside(
    store: &RunStore,
    record: &RunRecord,
    base: &str,
    declared: &BTreeSet<String>,
    stage: &str,
) -> Result<Vec<String>, String> {
    let worktree = record.worktree.as_path();
    let from = text(&git(worktree, &["merge-base", base, "HEAD"])?)?
        .trim()
        .to_string();
    let modified = git(
        worktree,
        &["diff", "--name-only", "--no-renames", "--diff-filter=M", "-z", &from, "HEAD", "--"],
    )?;
    let candidates: Vec<String> = text(&modified)?
        .split('\0')
        .filter(|path| path.ends_with(".rs") && !declared.contains(*path))
        .map(str::to_string)
        .collect();
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let dirty = uncommitted(worktree, &candidates)?;
    let mut editions = HashMap::new();
    let mut reverted = Vec::new();
    for path in candidates.into_iter().filter(|path| !dirty.contains(path)) {
        let old = git(worktree, &["cat-file", "blob", &format!("{from}:{path}")])?;
        let new = git(worktree, &["cat-file", "blob", &format!("HEAD:{path}")])?;
        let edition = edition_of(worktree, &path, &mut editions);
        if rustfmt(&config_dir(worktree, &path), &edition, &old).is_some_and(|formatted| formatted == new) {
            reverted.push(path);
        }
    }
    if reverted.is_empty() {
        return Ok(reverted);
    }
    let mut checkout = vec!["checkout", from.as_str(), "--"];
    checkout.extend(reverted.iter().map(String::as_str));
    let mut commit = vec!["commit", "--quiet", "--no-verify", "-m", FORMAT_REVERT_MESSAGE, "--only", "--"];
    commit.extend(reverted.iter().map(String::as_str));
    if let Err(why) = git(worktree, &checkout).and_then(|_| git(worktree, &commit)) {
        // The files were clean before: they go back to what HEAD holds, in the index and the worktree.
        let mut restore = vec!["checkout", "HEAD", "--"];
        restore.extend(reverted.iter().map(String::as_str));
        return Err(match git(worktree, &restore) {
            Ok(_) => why,
            Err(left) => format!("{why}; the files could not be put back as HEAD holds them: {left}"),
        });
    }
    store
        .append_event(&Event {
            schema_version: 1,
            event_id: next_event_id(&record.run_id),
            run_id: record.run_id.clone(),
            project_id: record.project_id.clone(),
            feature_id: record.feature_id.clone(),
            kind: FORMAT_REVERTED.to_string(),
            stage: Some(stage.to_string()),
            attempt: 1,
            occurred_at: now_rfc3339(),
            payload: serde_json::json!({ "files": reverted, "stage": stage }),
        })
        .map_err(|err| format!("cannot record the files put back: {err:?}"))?;
    Ok(reverted)
}

/// Runs git in `worktree` with pathspecs read literally; its output, or what went wrong.
fn git(worktree: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(worktree)
        .arg("--literal-pathspecs")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|err| format!("cannot run git: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "git {}: {}",
            args.first().copied().unwrap_or_default(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(output.stdout)
}

fn text(bytes: &[u8]) -> Result<&str, String> {
    std::str::from_utf8(bytes).map_err(|_| "git listed a path that is not UTF-8".to_string())
}

/// Which of `paths` have changes not committed, staged or not.
fn uncommitted(worktree: &Path, paths: &[String]) -> Result<BTreeSet<String>, String> {
    let mut args = vec!["status", "--porcelain", "-z", "--untracked-files=no", "--"];
    args.extend(paths.iter().map(String::as_str));
    let status = git(worktree, &args)?;
    Ok(text(&status)?
        .split('\0')
        .filter_map(|entry| entry.get(3..))
        .filter(|path| !path.is_empty())
        .map(str::to_string)
        .collect())
}

/// The Rust edition of the crate holding `path`: the `edition` of the nearest `Cargo.toml` at HEAD
/// that names one, the workspace's for a crate that takes it from there; 2021 when none does.
fn edition_of(worktree: &Path, path: &str, known: &mut HashMap<String, Option<String>>) -> String {
    let mut dir = path;
    while let Some((parent, _)) = dir.rsplit_once('/') {
        dir = parent;
        if let Some(edition) = manifest_edition(worktree, dir, known) {
            return edition;
        }
    }
    manifest_edition(worktree, "", known).unwrap_or_else(|| "2021".to_string())
}

fn manifest_edition(
    worktree: &Path,
    dir: &str,
    known: &mut HashMap<String, Option<String>>,
) -> Option<String> {
    known
        .entry(dir.to_string())
        .or_insert_with(|| {
            let manifest = if dir.is_empty() {
                "HEAD:Cargo.toml".to_string()
            } else {
                format!("HEAD:{dir}/Cargo.toml")
            };
            let contents = git(worktree, &["cat-file", "blob", &manifest]).ok()?;
            String::from_utf8_lossy(&contents).lines().find_map(|line| {
                let value = line.trim().strip_prefix("edition")?.trim_start().strip_prefix('=')?;
                let value = value.trim().trim_matches('"');
                (!value.is_empty() && value.chars().all(|c| c.is_ascii_digit()))
                    .then(|| value.to_string())
            })
        })
        .clone()
}

/// The folder rustfmt looks for its configuration from when formatting `path`: the file's own folder
/// in the worktree, so a crate's own `rustfmt.toml` applies; the worktree when that folder is not
/// one inside it (gone, or reached through a link that leads out).
fn config_dir(worktree: &Path, path: &str) -> std::path::PathBuf {
    let root = worktree.canonicalize().unwrap_or_else(|_| worktree.to_path_buf());
    let Some((parent, _)) = path.rsplit_once('/') else { return root };
    match worktree.join(parent).canonicalize() {
        Ok(dir) if dir.is_dir() && dir.starts_with(&root) => dir,
        _ => root,
    }
}

/// `source` as rustfmt formats it for `edition`, with the configuration found from `dir`; `None`
/// when rustfmt cannot be run or does not format it.
fn rustfmt(dir: &Path, edition: &str, source: &[u8]) -> Option<Vec<u8>> {
    let mut child = Command::new("rustfmt")
        .args(["--edition", edition, "--emit", "stdout", "--quiet"])
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdin = child.stdin.take()?;
    let source = source.to_vec();
    // Written apart from the read, so a large file fills neither pipe while the other waits.
    let writer = std::thread::spawn(move || stdin.write_all(&source));
    let output = child.wait_with_output().ok()?;
    writer.join().ok()?.ok()?;
    output.status.success().then_some(output.stdout)
}
