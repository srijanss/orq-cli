//! What a run's worktree needs so that the agent session, the MCP servers and a hand-resumed session
//! all see the run's environment: the git-ignored `.claude/settings.local.json`, the MCP servers it
//! approves, and the one-time `mcpctl` setup. Every problem is a note, never a failed run, and
//! environment values are never put into a note.

use std::collections::HashSet;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::local_files::{crosses_symlink, exclude_locally};

const SETTINGS: &str = ".claude/settings.local.json";
const WORKTREES_DIR: &str = ".claude/worktrees";
const MCPCTL_TOML: &str = ".mcpctl.toml";
/// What every command of the needs-input hook contains, however `orq` is reached.
const NEEDS_INPUT_COMMAND: &str = " run needs-input ";
const NEEDS_INPUT_TIMEOUT_SECS: u64 = 10;

/// Merges `env` into the `env` object of the worktree's `.claude/settings.local.json` and approves
/// MCP servers in `enabledMcpjsonServers`: those of `servers` that `.mcp.json` declares, or every
/// server it declares when `servers` is `None`. Other keys and earlier entries are kept. A chosen
/// `mcpctl` package also approves the server that runs it with `mcpctl`, the program orq sets MCP
/// servers up with (`None` when it is not installed): bare on PATH, or at that program's path.
pub fn write_session_settings(
    worktree: &Path,
    env: &[(String, String)],
    servers: Option<&[String]>,
    mcpctl: Option<&Path>,
) -> Vec<String> {
    let mut notes = Vec::new();
    let declared = declared_servers(worktree, mcpctl, &mut notes);
    let approved: Vec<String> = match servers {
        Some(wanted) => {
            let mut approved = Vec::new();
            for name in wanted {
                match resolve(&declared, name) {
                    Ok(server) if !approved.contains(server) => approved.push(server.clone()),
                    Ok(_) => {}
                    Err(note) => notes.push(note),
                }
            }
            approved
        }
        None => declared.iter().map(|server| server.name.clone()).collect(),
    };
    if approved.is_empty() && !declared.is_empty() || (approved.is_empty() && servers.is_some()) {
        notes.push("no MCP server of .mcp.json was approved for the session".to_string());
    }
    if env.is_empty() && approved.is_empty() {
        return notes;
    }
    if let Err(why) = merge_settings(worktree, env, &approved) {
        notes.push(format!("{SETTINGS} was not updated: {why}"));
    }
    notes
}

/// A server `.mcp.json` declares: its name there and, when it is started with
/// `mcpctl run <package>`, that package, the name the `mcpctl` catalog knows it by.
struct DeclaredServer {
    name: String,
    package: Option<String>,
}

/// The `.mcp.json` server name a chosen name approves: the server of that name, or else the one
/// server that runs it as an `mcpctl` package. Otherwise the note why nothing is approved.
fn resolve<'a>(declared: &'a [DeclaredServer], chosen: &str) -> Result<&'a String, String> {
    if let Some(server) = declared.iter().find(|server| server.name == chosen) {
        return Ok(&server.name);
    }
    let running: Vec<&String> = declared
        .iter()
        .filter(|server| server.package.as_deref() == Some(chosen))
        .map(|server| &server.name)
        .collect();
    match running.as_slice() {
        [only] => Ok(only),
        [] => Err(format!(
            "MCP server {chosen} is not declared in .mcp.json; it was not approved"
        )),
        many => Err(format!(
            "MCP server {chosen} is run by more than one .mcp.json server ({}); it was not approved",
            many.iter().map(|name| name.as_str()).collect::<Vec<_>>().join(", ")
        )),
    }
}

/// The `<package>` of a server entry started with `mcpctl run <package>`, where that `mcpctl` is
/// the trusted one: the bare name, looked up on PATH as orq's own is, or a path to the same file.
fn mcpctl_package(
    worktree: &Path,
    entry: &serde_json::Value,
    trusted: Option<&Path>,
) -> Option<String> {
    let trusted = trusted?;
    // An entry that sets its own PATH may find a different `mcpctl` than orq's.
    let sets_path = entry
        .get("env")
        .and_then(|env| env.as_object())
        .is_some_and(|env| env.keys().any(|key| key.eq_ignore_ascii_case("PATH")));
    if sets_path {
        return None;
    }
    let command = entry.get("command")?.as_str()?;
    if command != "mcpctl" {
        let same = worktree.join(command).canonicalize().ok()? == trusted.canonicalize().ok()?;
        if !same {
            return None;
        }
    }
    match entry.get("args")?.as_array()?.as_slice() {
        [run, package, ..] if run.as_str() == Some("run") => package.as_str().map(str::to_string),
        _ => None,
    }
}

/// The servers `.mcp.json` declares; a missing or unreadable file is a note.
fn declared_servers(
    worktree: &Path,
    mcpctl: Option<&Path>,
    notes: &mut Vec<String>,
) -> Vec<DeclaredServer> {
    let text = match std::fs::read_to_string(worktree.join(".mcp.json")) {
        Ok(text) => text,
        Err(_) => {
            notes.push(
                "there is no readable .mcp.json in the worktree; no MCP server was approved"
                    .to_string(),
            );
            return Vec::new();
        }
    };
    match serde_json::from_str::<serde_json::Value>(&text) {
        Ok(json) => json
            .get("mcpServers")
            .and_then(|v| v.as_object())
            .map(|servers| {
                servers
                    .iter()
                    .map(|(name, entry)| DeclaredServer {
                        name: name.clone(),
                        package: mcpctl_package(worktree, entry, mcpctl),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        Err(_) => {
            notes.push(".mcp.json is not valid JSON; no MCP server was approved".to_string());
            Vec::new()
        }
    }
}

fn merge_settings(
    worktree: &Path,
    env: &[(String, String)],
    approved: &[String],
) -> Result<(), String> {
    edit_settings(worktree, |root| {
        if !env.is_empty() {
            let table = root.entry("env").or_insert_with(|| serde_json::json!({}));
            let table = table
                .as_object_mut()
                .ok_or("its env is not a JSON object")?;
            for (key, value) in env {
                table.insert(key.clone(), serde_json::Value::String(value.clone()));
            }
        }
        if !approved.is_empty() {
            let list = root
                .entry("enabledMcpjsonServers")
                .or_insert_with(|| serde_json::json!([]));
            let list = list
                .as_array_mut()
                .ok_or("its enabledMcpjsonServers is not a JSON array")?;
            for name in approved {
                if !list.iter().any(|item| item.as_str() == Some(name)) {
                    list.push(serde_json::Value::String(name.clone()));
                }
            }
        }
        Ok(())
    })
}

/// When a session needs its person, and when a question has been answered and no longer does.
const NEEDS_INPUT_EVENTS: [(&str, Option<&str>); 3] = [
    ("PreToolUse", Some("AskUserQuestion")),
    ("Notification", Some("permission_prompt")),
    ("PostToolUse", Some("AskUserQuestion")),
];
/// For a session that leaves a marker when it is finished: when it has ended its turn or sits idle
/// without one, so waits for its person, and when that person types into it.
const WAITING_EVENTS: [(&str, Option<&str>); 3] = [
    ("Stop", None),
    ("Notification", Some("idle_prompt")),
    ("UserPromptSubmit", None),
];

/// Tells the worktree's Claude session to run `command` when it asks its person a question or waits
/// for a permission, and nothing else about the settings changes. A command that names the session's
/// marker (`--marker '<path>'`) is also run when the session stops or sits idle, and when its person
/// types into it. An earlier copy of the hook goes, so a resumed run, a later session with another
/// marker or a moved `orq` binary does not call it twice. The hook is held to a short timeout:
/// Claude waits for it before it shows the question.
pub fn write_needs_input_hook(worktree: &Path, command: &str) -> Vec<String> {
    let waits = needs_input_marker(command).is_some_and(|marker| !marker.is_empty());
    let edited = edit_settings(worktree, |root| {
        let hooks = root.entry("hooks").or_insert_with(|| serde_json::json!({}));
        let hooks = hooks
            .as_object_mut()
            .ok_or("its hooks is not a JSON object")?;
        // Every earlier copy goes before any is added: one event can hold more than one of ours.
        remove_own_hooks_from(hooks)?;
        let wanted = NEEDS_INPUT_EVENTS
            .iter()
            .chain(WAITING_EVENTS.iter().filter(|_| waits));
        for (event, matcher) in wanted {
            let entries = hooks.entry(*event).or_insert_with(|| serde_json::json!([]));
            let entries = entries
                .as_array_mut()
                .ok_or_else(|| format!("its hooks.{event} is not a JSON array"))?;
            let mut entry = serde_json::json!({
                "hooks": [{"type": "command", "command": command, "timeout": NEEDS_INPUT_TIMEOUT_SECS}],
            });
            if let Some(matcher) = matcher {
                entry["matcher"] = serde_json::Value::from(*matcher);
            }
            entries.push(entry);
        }
        Ok(())
    });
    match edited {
        Ok(()) => Vec::new(),
        Err(why) => vec![format!("{SETTINGS} was not updated: {why}")],
    }
}

/// Takes every copy of the hook out of the settings' `hooks`, whichever events hold one.
fn remove_own_hooks_from(
    hooks: &mut serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    for (event, _) in NEEDS_INPUT_EVENTS.iter().chain(WAITING_EVENTS.iter()) {
        if let Some(entries) = hooks.get_mut(*event) {
            let entries = entries
                .as_array_mut()
                .ok_or_else(|| format!("its hooks.{event} is not a JSON array"))?;
            remove_own_hooks(entries);
        }
    }
    Ok(())
}

/// Takes the hook `write_needs_input_hook` wrote back out of a worktree an attended session has used,
/// for a session with nobody to ask. A worktree with no settings gets none. Returns notes, like it.
pub fn remove_needs_input_hook(worktree: &Path) -> Vec<String> {
    if std::fs::symlink_metadata(worktree.join(SETTINGS)).is_err() {
        return Vec::new();
    }
    let edited = edit_settings(worktree, |root| {
        let Some(hooks) = root
            .get_mut("hooks")
            .and_then(|hooks| hooks.as_object_mut())
        else {
            return Ok(());
        };
        remove_own_hooks_from(hooks)
    });
    match edited {
        Ok(()) => Vec::new(),
        Err(why) => vec![format!("{SETTINGS} was not updated: {why}")],
    }
}

/// What a run's session may do without asking, beside editing the worktree: read, use its skills,
/// the local git commands, building and testing, and the MCP tools the implement-and-review skill
/// works through.
const ALLOWED: &[&str] = &[
    "Read",
    "Glob",
    "Grep",
    "Skill",
    "ToolSearch",
    "TodoWrite",
    "Bash(git status:*)",
    "Bash(git diff:*)",
    "Bash(git log:*)",
    "Bash(git show:*)",
    "Bash(git add:*)",
    "Bash(git commit:*)",
    "Bash(git rev-parse:*)",
    "Bash(git branch:*)",
    "Bash(git stash:*)",
    "Bash(git restore:*)",
    "Bash(git rm:*)",
    "Bash(git mv:*)",
    "Bash(git ls-files:*)",
    "Bash(git grep:*)",
    "Bash(git merge-base:*)",
    "Bash(cargo build:*)",
    "Bash(cargo test:*)",
    "Bash(cargo check:*)",
    "Bash(cargo fmt:*)",
    "Bash(cargo clippy:*)",
    "Bash(cargo run:*)",
    "Bash(cargo metadata:*)",
    "Bash(cargo tree:*)",
    "Bash(npm ci)",
    "Bash(ls:*)",
    "Bash(cat:*)",
    "Bash(head:*)",
    "Bash(wc:*)",
    "mcp__outside-in-tdd",
    "mcp__project-mcp",
];
/// Rules an earlier orq wrote and no longer does, taken out of the settings it rewrites. They allow
/// no more than what replaced them, so settings that still hold one never stop a session.
const RETIRED: [&str; 1] = ["Edit(./**)"];
/// Rules of orq's own that run a project's script from the folder that holds it, with the script's
/// file name: each is allowed only while every file of that name in the worktree is one the run's
/// base holds checked in and the run has not changed.
const ALLOWED_SCRIPTS: [(&str, &str); 1] = [("Bash(node build.mjs)", "build.mjs")];
/// Publishing to a remote is orq's own step, never the session's, whatever else is allowed.
const DENIED: [&str; 3] = [
    "Bash(git push:*)",
    "Bash(cargo publish:*)",
    "Bash(npm publish:*)",
];

/// Shells, wrappers and interpreters run any command or code they are given, publishing included.
const RUNS_ANYTHING: [&str; 21] = [
    "sh",
    "bash",
    "zsh",
    "dash",
    "fish",
    "env",
    "xargs",
    "eval",
    "exec",
    "sudo",
    "nohup",
    "command",
    "python",
    "node",
    "perl",
    "ruby",
    "php",
    "deno",
    "bun",
    "lua",
    "osascript",
];

/// Programs with a subcommand that publishes: a rule for one of them must name a subcommand.
const PUBLISHING_PROGRAMS: [&str; 7] = ["git", "cargo", "npm", "gh", "glab", "pnpm", "yarn"];

/// Whether a rule has `push` or `publish` among its words.
fn names_publishing(rule: &str) -> bool {
    let mut words = rule.split(|c: char| !c.is_ascii_alphanumeric() && c != '-');
    words.any(|word| word == "push" || word == "publish")
}

/// Whether a permission rule would let the session publish to a remote: it names publishing, or
/// is broad enough to reach it another way, any command, a shell or wrapper, or a whole program
/// that can publish without naming its subcommand.
fn publishes(rule: &str) -> bool {
    if names_publishing(rule) {
        return true;
    }
    let rule = rule.trim();
    if rule == "Bash" {
        return true;
    }
    let Some(command) = rule
        .strip_prefix("Bash(")
        .and_then(|rest| rest.strip_suffix(')'))
    else {
        return false;
    };
    let command = command.trim_end();
    let command = command
        .strip_suffix(":*")
        .or_else(|| command.strip_suffix('*'))
        .unwrap_or(command);
    let mut words = command.split_whitespace();
    let Some(program) = words.next() else {
        return true;
    };
    // A program named by its path is the same program; `python3.12` is `python`.
    let program = program.rsplit('/').next().unwrap_or(program);
    let family = program.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
    if program.is_empty() || program.contains('*') || RUNS_ANYTHING.contains(&family) {
        return true;
    }
    if PUBLISHING_PROGRAMS.contains(&program) {
        return match words.next() {
            None => true,
            Some(sub) => sub.contains('*') || sub.starts_with('-'),
        };
    }
    false
}

/// Interpreters that may run a script checked in to the repository, by their family name.
const SCRIPT_INTERPRETERS: [&str; 6] = ["node", "python", "ruby", "perl", "bun", "deno"];

/// For a rule that runs an interpreter, `Some` with the script it runs when it has the one form
/// allowed: `Bash(<interpreter> <path>)` or `Bash(<interpreter> <path>:*)`, the interpreter by its
/// bare name and the path relative, plain and in the worktree. `Some(None)` for any other
/// interpreter rule, and `None` for a rule that runs no interpreter.
fn interpreter_script(rule: &str) -> Option<Option<&str>> {
    let command = rule.trim().strip_prefix("Bash(")?.strip_suffix(')')?;
    let program = command.split_whitespace().next()?;
    let name = program.rsplit('/').next().unwrap_or(program);
    let family = name.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
    if !SCRIPT_INTERPRETERS.contains(&family) {
        return None;
    }
    let command = command.strip_suffix(":*").unwrap_or(command);
    let script = match command.split(' ').collect::<Vec<_>>().as_slice() {
        [interpreter, script] if *interpreter == program && !program.contains('/') => *script,
        _ => return Some(None),
    };
    let plain = script
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "._-+/".contains(c));
    let segments_plain = script
        .split('/')
        .all(|segment| !matches!(segment, "" | "." | ".."));
    Some((plain && segments_plain && !script.starts_with('-')).then_some(script))
}

/// What the worktree holds of the files at the relative path `script` below any folder (`script`
/// itself, `web/script`, ...), each of which a command running it from that folder runs, against
/// the commit `base`.
#[derive(Clone)]
enum NamedScripts {
    /// `base` holds no such file, or one that is not a regular file, or git cannot tell.
    Absent,
    /// Repositories inside the worktree (submodules or not), whose files the worktree's git does not see.
    Nested(Vec<String>),
    /// Links to folders, whose files a check of the script's path does not see.
    Linked(Vec<String>),
    /// The files that differ from what `base` holds: changed, added or removed in a commit of the
    /// run or in the worktree, untracked or ignored ones included. Empty when none does.
    Changed(Vec<String>),
}

/// What the worktree holds against the commit `base`, read once for every script a run's rules
/// could run, and only what concerns those scripts kept.
struct Inventory {
    /// The base's entries for files named like one of the scripts: the path, and whether it is a
    /// regular file.
    held: Vec<(String, bool)>,
    /// What hides files from the check: repositories of their own, else links to folders. `None` when nothing does.
    hidden: Option<NamedScripts>,
    /// The files named like one of the scripts that differ from the base's, untracked or ignored
    /// ones included.
    differing: Vec<String>,
}

impl Inventory {
    /// The worktree read for `scripts`, relative paths; `None` when `base` is no commit, git
    /// cannot tell, or a path in the worktree or git's listing is not UTF-8.
    fn read(worktree: &Path, base: &str, scripts: &[&str]) -> Option<Self> {
        if base.is_empty() || !base.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let git = |args: &[&str]| -> Option<String> {
            let output = Command::new("git")
                .arg("-C")
                .arg(worktree)
                .args(args)
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .output()
                .ok()?;
            // A path that is not UTF-8 could read as another one: git's listing must be exact.
            output
                .status
                .success()
                .then(|| String::from_utf8(output.stdout).ok())
                .flatten()
        };
        let file_name = |path: &str| path.rsplit('/').next().unwrap_or(path).to_string();
        let names: Vec<String> = scripts.iter().map(|script| file_name(script)).collect();
        let named = |path: &str| names.contains(&file_name(path));
        let entry = |line: &str| {
            line.split_once('\t')
                .map(|(meta, path)| (meta.to_string(), path.to_string()))
        };
        // ls-tree takes no glob: the base's files are listed and picked by name here.
        let tree = git(&["ls-tree", "-r", "-z", base])?;
        let staged = git(&["ls-files", "-s", "-z"])?;
        let changed = git(&["diff", "--name-only", "-z", base])?;
        let held = tree
            .split('\0')
            .filter_map(entry)
            .filter(|(_, path)| named(path))
            .map(|(meta, path)| {
                (
                    path,
                    meta.starts_with("100644 blob ") || meta.starts_with("100755 blob "),
                )
            })
            .collect();
        // A submodule at the base or in the index.
        let mut nested: Vec<String> = tree
            .split('\0')
            .chain(staged.split('\0'))
            .filter(|line| line.starts_with("160000 "))
            .filter_map(entry)
            .map(|(_, path)| path)
            .collect();
        let mut differing: Vec<String> = changed
            .split('\0')
            .filter(|path| !path.is_empty() && named(path))
            .map(str::to_string)
            .collect();
        let indexed: HashSet<&str> = staged
            .split('\0')
            .filter_map(|line| line.split_once('\t'))
            .map(|(_, path)| path)
            .filter(|path| named(path))
            .collect();
        let mut linked = Vec::new();
        let mut untracked = Vec::new();
        // Every file in the worktree, tracked, untracked or ignored, read folder by folder: each entry
        // comes with its type, so a plain file is passed over with no further look and only a link is
        // followed, to see whether it leads to a folder. A folder holding `.git` is a repository of its
        // own, however deep in an untracked or ignored folder it lies, and nothing in it is read; a link
        // to a folder, inside the worktree or out, gives what is below it a path no check sees.
        let mut folders = vec![(worktree.to_path_buf(), String::new())];
        while let Some((folder, relative)) = folders.pop() {
            let (mut inner, mut files, mut repository) = (Vec::new(), Vec::new(), false);
            for entry in std::fs::read_dir(&folder).ok()? {
                let entry = entry.ok()?;
                let name = entry.file_name();
                // A name that is not UTF-8 could be taken for a checked-in one: nothing is told then.
                let text = name.to_str()?;
                let path = if relative.is_empty() {
                    text.to_string()
                } else {
                    format!("{relative}/{text}")
                };
                if name == ".git" {
                    repository = !relative.is_empty();
                    continue;
                }
                let kind = entry.file_type().ok()?;
                if kind.is_dir() {
                    inner.push((entry.path(), path));
                } else if kind.is_symlink()
                    && std::fs::metadata(entry.path()).is_ok_and(|target| target.is_dir())
                {
                    linked.push(path);
                } else if named(&path) && !indexed.contains(&path.as_str()) {
                    files.push(path);
                }
            }
            if repository {
                nested.push(relative);
            } else {
                folders.extend(inner);
                untracked.extend(files);
            }
        }
        untracked.sort();
        let listed: HashSet<&String> = differing.iter().collect();
        untracked.retain(|path| !listed.contains(path));
        differing.extend(untracked);
        for found in [&mut nested, &mut linked] {
            found.sort();
            found.dedup();
        }
        let hidden = match (nested.is_empty(), linked.is_empty()) {
            (false, _) => Some(NamedScripts::Nested(nested)),
            (true, false) => Some(NamedScripts::Linked(linked)),
            (true, true) => None,
        };
        Some(Self {
            held,
            hidden,
            differing,
        })
    }

    /// Whether `script` is a regular file of the base.
    fn tracked(&self, script: &str) -> bool {
        self.held
            .iter()
            .any(|(path, regular)| path == script && *regular)
    }

    /// What the worktree holds of the files `script` names below any folder.
    fn named(&self, script: &str) -> NamedScripts {
        let below = format!("/{script}");
        let names = |path: &&String| *path == script || path.ends_with(&below);
        let mut held = self.held.iter().filter(|(path, _)| names(&path)).peekable();
        if held.peek().is_none() || !held.all(|(_, regular)| *regular) {
            return NamedScripts::Absent;
        }
        if let Some(hidden) = &self.hidden {
            return hidden.clone();
        }
        NamedScripts::Changed(self.differing.iter().filter(names).cloned().collect())
    }
}

fn linked_what(links: &[String]) -> &'static str {
    if links.len() == 1 {
        "is a link to a folder"
    } else {
        "are links to folders"
    }
}

/// Lets the worktree's session work with nobody to answer a prompt: its edits are accepted, the
/// commands and tools of `ALLOWED` and the project's `extra` (its `allowed_tools`) run without
/// asking, publishing is refused, and `logs` is writable so the session can leave its exit marker.
/// Anything else the session is refused. An added rule that would publish is left out; an
/// interpreter is kept only to run a script the run's `base` commit holds and the run has not
/// changed since, nor any other file its path names when run from another folder, with no repository
/// of its own nor link to a folder in the worktree to hide such a file; a rule left out is
/// also taken out of an allow list an earlier call wrote. Other
/// keys and earlier entries are kept, and nothing is listed twice. Returns notes, never fails the run.
pub fn write_run_permissions(
    worktree: &Path,
    extra: &[String],
    logs: &Path,
    base: &str,
) -> Vec<String> {
    let (mut notes, unrevoked, _) = apply_run_permissions(worktree, extra, logs, base);
    notes.extend(unrevoked);
    notes
}

/// What a refresh of a run's permissions wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunPermissions {
    /// What was left out and why, and settings that could not be updated.
    pub notes: Vec<String>,
    /// The `Bash(...)` rules the settings now allow, orq's, the project's and those the file already
    /// held, once each in order and none a deny rule there refuses: the command forms an unattended
    /// session is told it may run.
    pub allowed_commands: Vec<String>,
}

/// What [`write_run_permissions`] does: its notes, why a rule left out is still allowed by
/// settings that could not be updated, and the command rules the settings were given.
fn apply_run_permissions(
    worktree: &Path,
    extra: &[String],
    logs: &Path,
    base: &str,
) -> (Vec<String>, Option<String>, Vec<String>) {
    let mut notes = Vec::new();
    let logs = logs.to_string_lossy();
    // A path rule beginning with `//` is absolute: the session writes its exit marker there.
    let marker_rule = format!("Edit(/{logs}/**)");
    let worktree_rule = worktree_edit_rule(worktree);
    let mut allowed: Vec<&str> = worktree_rule.as_deref().into_iter().collect();
    let refused = worktree_rule.as_ref().err().cloned();
    allowed.extend(ALLOWED);
    allowed.push(&marker_rule);
    // A rule left out now goes from the settings too: an earlier session may have been allowed it.
    let mut withdrawn: Vec<&str> = Vec::new();
    let scripts: Vec<&str> = ALLOWED_SCRIPTS
        .iter()
        .map(|(_, name)| *name)
        .chain(
            extra
                .iter()
                .filter_map(|rule| interpreter_script(rule).flatten()),
        )
        .collect();
    let inventory = Inventory::read(worktree, base, &scripts);
    let named_scripts = |script: &str| {
        inventory
            .as_ref()
            .map_or(NamedScripts::Absent, |inventory| inventory.named(script))
    };
    let tracked_at = |script: &str| {
        inventory
            .as_ref()
            .is_some_and(|inventory| inventory.tracked(script))
    };
    for (rule, name) in ALLOWED_SCRIPTS {
        match named_scripts(name) {
            // A project without the script has no use for the rule: it goes without a note.
            NamedScripts::Absent => withdrawn.push(rule),
            NamedScripts::Nested(repositories) => {
                let what = if repositories.len() == 1 {
                    "is a repository of its own"
                } else {
                    "are repositories of their own"
                };
                notes.push(format!(
                    "{} {what}, whose {name} orq cannot check, so the rule {rule} was left out",
                    repositories.join(", ")
                ));
                withdrawn.push(rule);
            }
            NamedScripts::Linked(links) => {
                notes.push(format!(
                    "{} {}, whose {name} orq cannot check, so the rule {rule} was left out",
                    links.join(", "),
                    linked_what(&links)
                ));
                withdrawn.push(rule);
            }
            NamedScripts::Changed(changed) if changed.is_empty() => allowed.push(rule),
            NamedScripts::Changed(changed) => {
                notes.push(format!(
                    "{} was changed by this run, so the rule {rule} was left out",
                    changed.join(", ")
                ));
                withdrawn.push(rule);
            }
        }
    }
    for rule in extra {
        // orq's own rule, left out above, already has its note.
        if withdrawn.contains(&rule.as_str()) {
            continue;
        }
        let not_script = || {
            format!("allowed_tools rule {rule} runs an interpreter on something other than a checked-in script; it was left out")
        };
        let could_publish = || {
            format!("allowed_tools rule {rule} could publish to a remote, which orq does itself; it was left out, name the subcommands the run needs instead")
        };
        let left_out = match interpreter_script(rule) {
            _ if names_publishing(rule) => Some(could_publish()),
            None => publishes(rule).then(could_publish),
            // A worktree that could not be read is one whose scripts nothing checked.
            Some(Some(_)) if inventory.is_none() => Some(format!("orq could not read the worktree to check its scripts, so allowed_tools rule {rule} was left out")),
            // Run from another folder, the path names another file: each must be the base's too.
            Some(Some(script)) if tracked_at(script) => match named_scripts(script) {
                NamedScripts::Absent => Some(not_script()),
                NamedScripts::Nested(repositories) => {
                    let what = if repositories.len() == 1 { "is a repository of its own" } else { "are repositories of their own" };
                    Some(format!("{} {what}, whose {script} orq cannot check, so allowed_tools rule {rule} was left out", repositories.join(", ")))
                }
                NamedScripts::Linked(links) => Some(format!("{} {}, whose {script} orq cannot check, so allowed_tools rule {rule} was left out", links.join(", "), linked_what(&links))),
                NamedScripts::Changed(changed) => (!changed.is_empty()).then(|| format!("{} was changed by this run, so allowed_tools rule {rule} was left out", changed.join(", "))),
            },
            Some(_) => Some(not_script()),
        };
        match left_out {
            Some(note) => {
                notes.push(note);
                withdrawn.push(rule.as_str());
            }
            None => allowed.push(rule),
        }
    }
    // A rule left out for one reason stays out, however else it was allowed.
    allowed.retain(|rule| !withdrawn.contains(rule));
    let mut commands: Vec<String> = Vec::new();
    let edited = edit_settings(worktree, |root| {
        let permissions = root
            .entry("permissions")
            .or_insert_with(|| serde_json::json!({}));
        let permissions = permissions
            .as_object_mut()
            .ok_or("its permissions is not a JSON object")?;
        permissions.insert("defaultMode".to_string(), serde_json::json!("acceptEdits"));
        for (key, rules) in [
            ("allow", allowed.as_slice()),
            ("deny", DENIED.as_slice()),
            ("additionalDirectories", &[logs.as_ref()][..]),
        ] {
            let list = permissions
                .entry(key)
                .or_insert_with(|| serde_json::json!([]));
            let list = list
                .as_array_mut()
                .ok_or_else(|| format!("its permissions.{key} is not a JSON array"))?;
            if key == "allow" {
                list.retain(|item| {
                    !item
                        .as_str()
                        .is_some_and(|item| withdrawn.contains(&item) || RETIRED.contains(&item))
                });
            }
            for rule in rules {
                if !list.iter().any(|item| item.as_str() == Some(rule)) {
                    list.push(serde_json::json!(rule));
                }
            }
        }
        // The rules the file now holds, those it already held too, but for any a deny rule refuses.
        let rules = |key: &str| {
            permissions
                .get(key)
                .and_then(|list| list.as_array())
                .into_iter()
                .flatten()
                .filter_map(|item| item.as_str())
        };
        let denied: Vec<&str> = rules("deny").collect();
        let mut seen = std::collections::HashSet::new();
        commands = rules("allow")
            .filter(|rule| {
                rule.starts_with("Bash(")
                    && !denied.iter().any(|deny| deny_covers(deny, rule))
                    && seen.insert(*rule)
            })
            .map(str::to_string)
            .collect();
        Ok(())
    });
    let Err(why) = edited else {
        return (notes, refused, commands);
    };
    // Rules a file Claude Code still reads allows; one it cannot parse allows nothing.
    let held = std::fs::read_to_string(worktree.join(SETTINGS))
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok());
    let still: Vec<&str> = withdrawn
        .into_iter()
        .filter(|rule| {
            held.as_ref()
                .and_then(|held| held["permissions"]["allow"].as_array())
                .is_some_and(|list| list.iter().any(|item| item.as_str() == Some(rule)))
        })
        .collect();
    if still.is_empty() {
        // Settings that were not written allow none of the rules: the session is told of none.
        notes.push(format!("{SETTINGS} was not updated: {why}"));
        return (notes, refused, Vec::new());
    }
    let unrevoked = format!(
        "{SETTINGS} was not updated ({why}) and still allows {}, which must be left out",
        still.join(", ")
    );
    let unrevoked = match refused {
        Some(refused) => format!("{refused}; {unrevoked}"),
        None => unrevoked,
    };
    (notes, Some(unrevoked), Vec::new())
}

/// Characters a permission rule's path reads as a pattern rather than as themselves; a backslash
/// escapes the character after it.
const PATTERN_CHARACTERS: [char; 9] = ['*', '?', '[', ']', '{', '}', '(', ')', '\\'];

/// The rule letting the session edit the worktree and nothing else: absolute, by the worktree's
/// canonical path, as a `./` rule is read from the session's current folder, which a `cd` moves.
/// Fails, saying why, for a path no rule names exactly: one a pattern or control character, or a
/// byte UTF-8 does not allow, would make the rule allow more or less than the worktree.
fn worktree_edit_rule(worktree: &Path) -> Result<String, String> {
    let cannot = |why: String| {
        format!(
            "orq cannot allow editing the worktree {}: {why}, so no session was given it",
            worktree.display()
        )
    };
    let canonical = worktree
        .canonicalize()
        .map_err(|error| cannot(format!("its path cannot be resolved ({error})")))?;
    let path = canonical
        .to_str()
        .ok_or_else(|| cannot("its path is not UTF-8".to_string()))?;
    if let Some(found) = path
        .chars()
        .find(|c| PATTERN_CHARACTERS.contains(c) || c.is_control())
    {
        return Err(cannot(format!("its path holds {found:?}, which a permission rule does not read as itself; move the worktree to a path without it")));
    }
    Ok(format!("Edit(/{path}/**)"))
}

/// Whether the deny rule `deny` refuses every command the allow rule `allow` lets run. Checked on
/// commands standing for the allowed ones: the rule's own words, with each `*` empty and with each
/// filled, and for a prefix rule those followed by more words.
fn deny_covers(deny: &str, allow: &str) -> bool {
    if deny == "Bash" {
        return true;
    }
    let inner = |rule: &str| {
        rule.strip_prefix("Bash(")?
            .strip_suffix(')')
            .map(str::to_string)
    };
    let (Some(denied), Some(allowed)) = (inner(deny), inner(allow)) else {
        return false;
    };
    let (words, more) = command_pattern(&allowed);
    // A character no configured command holds, so a sample matches only through a `*`.
    let filled = words.replace('*', "\u{1}");
    let mut samples = vec![words.replace('*', ""), filled.clone()];
    if more {
        samples.push(format!("{filled} \u{1}"));
    }
    samples
        .iter()
        .all(|command| pattern_matches(&denied, command))
}

/// A rule's command pattern and whether it lets more words follow: `prefix:*` and `prefix *` do.
fn command_pattern(rule: &str) -> (&str, bool) {
    match rule.strip_suffix(":*").or_else(|| rule.strip_suffix(" *")) {
        Some(words) => (words, true),
        None => (rule, false),
    }
}

/// Whether the command `command` is one the rule pattern `rule` names, where `*` stands for any text.
fn pattern_matches(rule: &str, command: &str) -> bool {
    let (words, more) = command_pattern(rule);
    glob_matches(words, command) || (more && glob_matches(&format!("{words} *"), command))
}

/// Whether `text` is `pattern` with each `*` in it standing for any text, none included.
fn glob_matches(pattern: &str, text: &str) -> bool {
    let (pattern, text): (Vec<char>, Vec<char>) =
        (pattern.chars().collect(), text.chars().collect());
    let (mut p, mut t) = (0, 0);
    // The last `*` seen and the text position it was last tried to end at.
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        if p < pattern.len() && pattern[p] == '*' {
            star = Some((p, t));
            p += 1;
        } else if p < pattern.len() && pattern[p] == text[t] {
            p += 1;
            t += 1;
        } else if let Some((at, from)) = star {
            star = Some((at, from + 1));
            p = at + 1;
            t = from + 1;
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|c| *c == '*')
}

/// Like [`write_run_permissions`], for a session that must not start with a rule it was allowed
/// before and must lose now: fails, saying which, when the settings still allow such a rule. Also
/// returns the command rules the session is allowed.
pub fn refresh_run_permissions(
    worktree: &Path,
    extra: &[String],
    logs: &Path,
    base: &str,
) -> Result<RunPermissions, String> {
    match apply_run_permissions(worktree, extra, logs, base) {
        (_, Some(unrevoked), _) => Err(unrevoked),
        (notes, None, allowed_commands) => Ok(RunPermissions {
            notes,
            allowed_commands,
        }),
    }
}

/// Takes out of `entries` the commands `write_needs_input_hook` wrote and nothing beside them: an
/// entry goes only when one of ours was all it held.
fn remove_own_hooks(entries: &mut Vec<serde_json::Value>) {
    entries.retain_mut(|entry| {
        let Some(hooks) = entry
            .get_mut("hooks")
            .and_then(|hooks| hooks.as_array_mut())
        else {
            return true;
        };
        let held = hooks.len();
        hooks.retain(|hook| {
            !hook
                .get("command")
                .and_then(|c| c.as_str())
                .is_some_and(is_needs_input_command)
        });
        hooks.len() == held || !hooks.is_empty()
    });
}

/// Whether `command` is exactly what Orq writes, `'<orq>' run needs-input '<run>' --repo '<root>'`,
/// with or without a leading `XDG_STATE_HOME='<dir>' ` and a trailing ` --marker '<path>'`, one
/// quoted word in each place: someone's own command, compound or merely with these words in it, stays.
fn is_needs_input_command(command: &str) -> bool {
    needs_input_marker(command).is_some()
}

/// For a command `is_needs_input_command` takes for Orq's, its quoted ` --marker '<path>'`, or ""
/// when it names none; `None` for any other command.
fn needs_input_marker(command: &str) -> Option<&str> {
    let command = match command.strip_prefix("XDG_STATE_HOME=") {
        Some(state) => after_quoted_word(state)?.strip_prefix(' ')?,
        None => command,
    };
    let rest = after_quoted_word(command)?.strip_prefix(NEEDS_INPUT_COMMAND)?;
    let rest = after_quoted_word(rest)?.strip_prefix(" --repo ")?;
    let rest = after_quoted_word(rest)?;
    if rest.is_empty() {
        return Some(rest);
    }
    let marker = rest.strip_prefix(" --marker ")?;
    after_quoted_word(marker)?.is_empty().then_some(marker)
}

/// What follows the single-quoted shell word `text` starts with, where `'\''` puts a quote inside it.
fn after_quoted_word(text: &str) -> Option<&str> {
    let mut rest = text.strip_prefix('\'')?;
    loop {
        rest = &rest[rest.find('\'')? + 1..];
        match rest.strip_prefix("\\''") {
            Some(more) => rest = more,
            None => return Some(rest),
        }
    }
}

/// Applies `edit` to the object in the worktree's settings file, or a new one, and writes it back.
/// An edit that fails leaves the file as it was.
fn edit_settings(
    worktree: &Path,
    edit: impl FnOnce(&mut serde_json::Map<String, serde_json::Value>) -> Result<(), String>,
) -> Result<(), String> {
    let path = worktree.join(SETTINGS);
    if crosses_symlink(worktree, SETTINGS)
        || std::fs::symlink_metadata(&path).is_ok_and(|meta| meta.file_type().is_symlink())
    {
        return Err("it is, or lies behind, a symbolic link".to_string());
    }
    // The file holds the run's environment: git must not offer it for staging.
    exclude_locally(worktree, SETTINGS)?;
    let mut settings = match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str::<serde_json::Value>(&text)
            .map_err(|_| "it is not valid JSON".to_string())?,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(err) => return Err(format!("it could not be read ({:?})", err.kind())),
    };
    let root = settings.as_object_mut().ok_or("it is not a JSON object")?;
    edit(root)?;
    // Only a file orq still owns is recorded again: an edit made before this write stays the
    // session's work, and adopting it would let cleanup remove the worktree that holds it.
    let orqs = !path.exists() || crate::orq_written::is_unchanged_orq_file(worktree, SETTINGS);
    std::fs::create_dir_all(worktree.join(".claude"))
        .map_err(|err| format!("its directory could not be created ({:?})", err.kind()))?;
    let text = serde_json::to_string_pretty(&settings)
        .map_err(|_| "it could not be serialised".to_string())?;
    // The file may hold credentials: owner-only, whether it is new or already existed.
    let write = || -> std::io::Result<()> {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        file.write_all((text + "\n").as_bytes())
    };
    write().map_err(|err| format!("it could not be written ({:?})", err.kind()))?;
    // What orq wrote is orq's, not the session's work: cleanup must still recognise it. A failed
    // record only keeps the worktree, the safe side.
    if orqs {
        let _ = crate::orq_written::record_orq_written_file(worktree, SETTINGS);
    }
    Ok(())
}

/// Sets up the run's MCP servers and then approves them, so what `mcpctl` generates in `.mcp.json`
/// is approved too. The servers are `chosen`, or with no such setting those the worktree's
/// `.mcpctl.toml` selects: without a setup they would run at the machine's active version, not
/// their latest. `env` is added to the setup's environment and merged into the session's settings.
pub fn prepare_mcp_servers(
    worktree: &Path,
    env: &[(String, String)],
    chosen: Option<&[String]>,
    mcpctl: Option<&Path>,
) -> Vec<String> {
    let mut notes = Vec::new();
    // A tracked link is in the worktree whatever carrying refused: reading it would let a file
    // from outside select servers, and setup, which rewrites the file, would write through it.
    let linked = std::fs::symlink_metadata(worktree.join(MCPCTL_TOML))
        .is_ok_and(|meta| meta.file_type().is_symlink());
    if linked {
        notes.push(format!(
            "{MCPCTL_TOML} is a symbolic link in the worktree; no MCP server was set up"
        ));
    } else {
        let selected = match chosen {
            Some(names) => names.to_vec(),
            None => selected_in_mcpctl_toml(worktree, &mut notes),
        };
        notes.extend(setup_mcp_servers(worktree, &selected, env, mcpctl));
    }
    notes.extend(write_session_settings(worktree, env, chosen, mcpctl));
    notes
}

/// Sets the run's MCP servers up in `worktree` through [`run_mcp_setup`](crate::run_mcp_setup),
/// taking a lock from `main_checkout` and logging mcpctl to `log`, then writes the session
/// settings. The report holds each server's version and source, and every note; nothing here
/// fails the run.
pub fn prepare_project_mcp(
    main_checkout: &Path,
    worktree: &Path,
    env: &[(String, String)],
    chosen: Option<&[String]>,
    mcpctl: Option<&Path>,
    log: &Path,
) -> crate::McpSetupReport {
    let mut report = crate::McpSetupReport::default();
    let linked = std::fs::symlink_metadata(worktree.join(MCPCTL_TOML))
        .is_ok_and(|meta| meta.file_type().is_symlink());
    if linked {
        report.notes.push(format!(
            "{MCPCTL_TOML} is a symbolic link in the worktree; no MCP server was set up"
        ));
    } else {
        let selected = match chosen {
            Some(names) => names.to_vec(),
            None => selected_in_mcpctl_toml(worktree, &mut report.notes),
        };
        if !selected.is_empty() {
            let done = crate::run_mcp_setup(&crate::McpSetupRequest {
                main_checkout,
                worktree,
                servers: &selected,
                env,
                mcpctl,
                log,
                timeouts: crate::McpTimeouts::default(),
            });
            report.versions = done.versions;
            report.notes.extend(done.notes);
        }
    }
    report
        .notes
        .extend(write_session_settings(worktree, env, chosen, mcpctl));
    report
}

/// The servers the `[[mcp]]` tables of the worktree's `.mcpctl.toml` name. A file that cannot be
/// read selects none and a name that would read as an option is left out, each with a note; a
/// missing file is simply no selection.
fn selected_in_mcpctl_toml(worktree: &Path, notes: &mut Vec<String>) -> Vec<String> {
    let text = match std::fs::read_to_string(worktree.join(MCPCTL_TOML)) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(err) => {
            notes.push(format!(
                "{MCPCTL_TOML} could not be read ({:?}); no MCP server was set up",
                err.kind()
            ));
            return Vec::new();
        }
    };
    let Ok(doc) = text.parse::<toml_edit::DocumentMut>() else {
        notes.push(format!(
            "{MCPCTL_TOML} is not valid TOML; no MCP server was set up"
        ));
        return Vec::new();
    };
    let tables = doc.get("mcp").and_then(|item| item.as_array_of_tables());
    let names = tables.into_iter().flat_map(|tables| tables.iter());
    let names = names.filter_map(|table| table.get("name")?.as_str());
    let (options, names): (Vec<&str>, Vec<&str>) = names.partition(|name| name.starts_with('-'));
    if !options.is_empty() {
        notes.push(format!(
            "{MCPCTL_TOML} names MCP servers that look like options; they were left out"
        ));
    }
    names.into_iter().map(str::to_string).collect()
}

/// Runs one `mcpctl setup --mcp <a> --mcp <b>` in `worktree` for all of `servers`, with `env` added
/// to the environment, so each is locked to its latest version. One call, because `setup` replaces
/// the project's selection: with a call per server only the last would be locked, and the others
/// would run at the machine's active version. `mcpctl` is the program, `None` when it is not installed.
pub fn setup_mcp_servers(
    worktree: &Path,
    servers: &[String],
    env: &[(String, String)],
    mcpctl: Option<&Path>,
) -> Vec<String> {
    if servers.is_empty() {
        return Vec::new();
    }
    let Some(program) = mcpctl else {
        return vec![
            "mcpctl is not on PATH; the configured MCP servers were not set up".to_string(),
        ];
    };
    let mut seen = HashSet::new();
    let names: Vec<&str> = servers
        .iter()
        .map(String::as_str)
        .filter(|name| seen.insert(*name))
        .collect();
    let done = Command::new(program)
        .arg("setup")
        .args(names.iter().flat_map(|name| ["--mcp", *name]))
        .current_dir(worktree)
        .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let names = names.join(", ");
    match done {
        Ok(status) if status.success() => Vec::new(),
        Ok(status) => vec![format!(
            "mcpctl setup for MCP servers {names} failed ({status}); run it in the worktree to see why"
        )],
        Err(err) => vec![format!(
            "mcpctl setup for MCP servers {names} could not be started ({:?})",
            err.kind()
        )],
    }
}

/// Keeps the directory run worktrees live in out of the main checkout's git status, through the local
/// exclude only. `None` when done (or already ignored); otherwise the note.
pub fn exclude_worktrees_dir(main_checkout: &Path) -> Option<String> {
    exclude_locally(main_checkout, WORKTREES_DIR)
        .err()
        .map(|why| format!("{WORKTREES_DIR} could not be kept out of git status: {why}"))
}
