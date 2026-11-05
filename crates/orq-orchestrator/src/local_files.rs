//! Carrying the main checkout's git-ignored local files into a run's worktree. A fresh worktree has
//! none of them: no dotenv file, no MCP configuration. Anything created here is added to the
//! repository's local exclude so it never shows up as untracked. File contents and values are never
//! logged; every problem is a note, never a failed run.

use std::path::{Component, Path, PathBuf};
use std::process::Command;

/// Carried when the repository does not choose its own list.
const DEFAULT_FILES: [&str; 3] = [".env", ".mcp.json", ".mcpctl.toml"];
/// The dotenv file is linked, so a later change in the main checkout is seen at once.
const DOTENV: &str = ".env";
const MCP_JSON: &str = ".mcp.json";
const MCPCTL_TOML: &str = ".mcpctl.toml";
const MCPCTL_LOCK: &str = ".mcpctl.lock";
/// The outside-in-tdd server's files, and the variable that points it at each.
const TDD_FILES: [(&str, &str); 2] = [
    (".tdd-config.json", "TDD_CONFIG_PATH"),
    (".tdd-features.json", "TDD_FEATURES_PATH"),
];

/// What the worktree was given besides files: environment for the agent session, and anything worth
/// telling the user.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocalFiles {
    pub env: Vec<(String, String)>,
    pub notes: Vec<String>,
}

/// Brings `files` (default: the dotenv file, `.mcp.json` and `.mcpctl.toml`) from `main_checkout` into
/// `worktree` where the worktree has none, and points the TDD server at the main checkout's own
/// files through the returned environment.
pub fn carry_local_files(
    main_checkout: &Path,
    worktree: &Path,
    files: Option<&[String]>,
) -> LocalFiles {
    let mut found = LocalFiles::default();
    let default: Vec<String> = DEFAULT_FILES.iter().map(|name| name.to_string()).collect();
    let chosen = files.is_some();
    for name in files.unwrap_or(&default) {
        if !plain_relative(name) {
            found.notes.push(format!(
                "{name} is not a plain relative path; it is not carried into the worktree"
            ));
            continue;
        }
        carry_one(main_checkout, worktree, name, chosen, &mut found);
    }
    for (name, variable) in TDD_FILES {
        let main = main_checkout.join(name);
        if worktree.join(name).exists() {
            continue;
        }
        if main.exists() {
            found
                .env
                .push((variable.to_string(), main.display().to_string()));
        } else {
            found.notes.push(format!(
                "{name} does not exist in the main checkout; the TDD server will find none"
            ));
        }
    }
    found
}

fn carry_one(main: &Path, worktree: &Path, name: &str, chosen: bool, found: &mut LocalFiles) {
    let (source, target) = (main.join(name), worktree.join(name));
    // A symbolic link in a parent directory could lead a read or a write outside either checkout.
    if let Some(root) = [main, worktree]
        .into_iter()
        .find(|root| crosses_symlink(root, name))
    {
        let which = if root == main {
            "main checkout"
        } else {
            "worktree"
        };
        found.notes.push(format!("{name} is not carried into the worktree: a directory on its path in the {which} is a symbolic link"));
        return;
    }
    if std::fs::symlink_metadata(&source).is_err() {
        if chosen || name == DOTENV {
            found.notes.push(format!(
                "{name} does not exist in the main checkout; nothing to carry over"
            ));
        }
        return;
    }
    // Copying follows a symbolic link, so a linked source would carry a file from outside the checkout.
    // (The dotenv file is itself linked, never copied.)
    if name != DOTENV
        && std::fs::symlink_metadata(&source).is_ok_and(|meta| meta.file_type().is_symlink())
    {
        found.notes.push(format!(
            "{name} is a symbolic link in the main checkout; it is not carried into the worktree"
        ));
        return;
    }
    if let Ok(meta) = std::fs::symlink_metadata(&target) {
        if name == MCP_JSON && meta.file_type().is_symlink() {
            found.notes.push(format!("{name} is a symbolic link in the worktree; the main checkout's values were not merged into it"));
        } else if name == MCP_JSON {
            // Ignore rules cannot keep a tracked file out of a commit, so values from the main
            // checkout are never written into one.
            let tracked = is_tracked(worktree, name);
            if tracked == Some(true) {
                found.notes.push(format!("{name} is tracked by git in the worktree; the main checkout's values were not merged into it"));
            } else if tracked.is_none() {
                found.notes.push(format!("whether {name} is tracked by git could not be checked; the main checkout's values were not merged into it"));
            } else if let Err(why) = exclude_locally(worktree, name) {
                // Credentials must not land in a file git could offer for staging.
                found.notes.push(format!("{name} was not merged: {why}"));
            } else if let Err(note) = merge_mcp_servers(&source, &target) {
                found.notes.push(note);
            }
        }
        return;
    }
    // A copy git could offer for staging is worse than none: keep it out of git status first.
    if let Err(why) = exclude_locally(worktree, name) {
        found
            .notes
            .push(format!("{name} is not carried into the worktree: {why}"));
        return;
    }
    if let Some(parent) = target.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let done = if name == DOTENV {
        std::os::unix::fs::symlink(&source, &target)
    } else {
        std::fs::copy(&source, &target).map(|_| ())
    };
    if let Err(err) = done {
        found.notes.push(format!(
            "{name} could not be carried into the worktree ({:?})",
            err.kind()
        ));
        return;
    }
    if name == MCPCTL_TOML {
        if let Err(why) = exclude_locally(worktree, MCPCTL_LOCK) {
            found.notes.push(format!(
                "{MCPCTL_LOCK} could not be kept out of git status: {why}"
            ));
        }
    }
}

/// Whether any directory leading to `name` under `root` is a symbolic link.
pub(crate) fn crosses_symlink(root: &Path, name: &str) -> bool {
    let mut path = root.to_path_buf();
    let mut parts = Path::new(name).components().peekable();
    while let Some(part) = parts.next() {
        if parts.peek().is_none() {
            break;
        }
        path.push(part);
        if std::fs::symlink_metadata(&path).is_ok_and(|meta| meta.file_type().is_symlink()) {
            return true;
        }
    }
    false
}

/// Whether git tracks `name` in `worktree`: `Some(false)` only when git confirms that no such path
/// is tracked (exit code 1); any other failure leaves it unknown.
fn is_tracked(worktree: &Path, name: &str) -> Option<bool> {
    let out = Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(["ls-files", "--error-unmatch", "--", name])
        .output()
        .ok()?;
    match out.status.code() {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    }
}

/// No `..`, no absolute path: only files inside the checkout can be carried.
fn plain_relative(name: &str) -> bool {
    let path = Path::new(name);
    !name.is_empty()
        && !name.contains(['\n', '\r'])
        && path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}

/// Adds `name` to the repository's `info/exclude` unless git already ignores it.
/// `name` as a gitignore pattern that matches only that file.
fn escape_pattern(name: &str) -> String {
    let mut pattern = String::new();
    for c in name.chars() {
        if matches!(c, '\\' | '*' | '?' | '[' | ']') {
            pattern.push('\\');
        }
        pattern.push(c);
    }
    // A trailing space is dropped by git unless escaped.
    if pattern.ends_with(' ') {
        pattern.insert(pattern.len() - 1, '\\');
    }
    pattern
}

/// It succeeds only once git reports `name` as ignored.
pub(crate) fn exclude_locally(worktree: &Path, name: &str) -> Result<(), String> {
    let git = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(worktree)
            .args(args)
            .output()
    };
    let ignored = || git(&["check-ignore", "-q", "--", name]).is_ok_and(|out| out.status.success());
    if ignored() {
        return Ok(());
    }
    let path = git(&["rev-parse", "--git-path", "info/exclude"])
        .ok()
        .filter(|out| out.status.success())
        .map(|out| {
            let printed = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
            if printed.is_absolute() {
                printed
            } else {
                worktree.join(printed)
            }
        })
        .ok_or("the local exclude was not found")?;
    let mut text = std::fs::read_to_string(&path).unwrap_or_default();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&format!("/{}\n", escape_pattern(name)));
    path.parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(&path, text))
        .map_err(|err| format!("the local exclude could not be written ({:?})", err.kind()))?;
    if ignored() {
        Ok(())
    } else {
        Err("git does not ignore it after the local exclude was updated".to_string())
    }
}

/// For each server both files describe, copies the main file's keys (its `env` entries among them)
/// over the worktree's, but keeps the worktree's own `command` and `args`.
fn merge_mcp_servers(source: &Path, target: &Path) -> Result<(), String> {
    let read = |path: &Path| -> Result<serde_json::Value, String> {
        let text = std::fs::read_to_string(path).map_err(|e| {
            format!(
                "{MCP_JSON} could not be read ({:?}); its servers were not merged",
                e.kind()
            )
        })?;
        serde_json::from_str(&text)
            .map_err(|_| format!("{MCP_JSON} is not valid JSON; its servers were not merged"))
    };
    let (main, mut own) = (read(source)?, read(target)?);
    let (Some(main_servers), Some(own_servers)) = (
        main.get("mcpServers").and_then(|v| v.as_object()),
        own.get_mut("mcpServers").and_then(|v| v.as_object_mut()),
    ) else {
        return Ok(());
    };
    for (server, from) in main_servers {
        let (Some(from), Some(into)) = (
            from.as_object(),
            own_servers.get_mut(server).and_then(|v| v.as_object_mut()),
        ) else {
            continue;
        };
        for (key, value) in from {
            if key == "command" || key == "args" {
                continue;
            }
            match (
                key.as_str(),
                value.as_object(),
                into.get_mut(key).and_then(|v| v.as_object_mut()),
            ) {
                ("env", Some(extra), Some(existing)) => {
                    existing.extend(extra.iter().map(|(k, v)| (k.clone(), v.clone())))
                }
                _ => {
                    into.insert(key.clone(), value.clone());
                }
            }
        }
    }
    let text = serde_json::to_string_pretty(&own)
        .map_err(|_| format!("{MCP_JSON} could not be written; its servers were not merged"))?;
    std::fs::write(target, text + "\n").map_err(|e| {
        format!(
            "{MCP_JSON} could not be written ({:?}); its servers were not merged",
            e.kind()
        )
    })
}
