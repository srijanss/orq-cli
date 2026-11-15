//! What a run's worktree needs so that the agent session, the MCP servers and a hand-resumed session
//! all see the run's environment: the git-ignored `.claude/settings.local.json`, the MCP servers it
//! approves, and the one-time `mcpctl` setup. Every problem is a note, never a failed run, and
//! environment values are never put into a note.

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
const NEEDS_INPUT_EVENTS: [(&str, &str); 3] =
    [("PreToolUse", "AskUserQuestion"), ("Notification", "permission_prompt"), ("PostToolUse", "AskUserQuestion")];

/// Tells the worktree's Claude session to run `command` when it asks its person a question or waits
/// for a permission, and nothing else about the settings changes. An earlier copy of the hook goes,
/// so a resumed run or a moved `orq` binary does not call it twice. The hook is held to a short
/// timeout: Claude waits for it before it shows the question.
pub fn write_needs_input_hook(worktree: &Path, command: &str) -> Vec<String> {
    let edited = edit_settings(worktree, |root| {
        let hooks = root.entry("hooks").or_insert_with(|| serde_json::json!({}));
        let hooks = hooks.as_object_mut().ok_or("its hooks is not a JSON object")?;
        for (event, matcher) in NEEDS_INPUT_EVENTS {
            let entries = hooks.entry(event).or_insert_with(|| serde_json::json!([]));
            let entries = entries.as_array_mut().ok_or_else(|| format!("its hooks.{event} is not a JSON array"))?;
            remove_own_hooks(entries);
            entries.push(serde_json::json!({
                "matcher": matcher,
                "hooks": [{"type": "command", "command": command, "timeout": NEEDS_INPUT_TIMEOUT_SECS}],
            }));
        }
        Ok(())
    });
    match edited {
        Ok(()) => Vec::new(),
        Err(why) => vec![format!("{SETTINGS} was not updated: {why}")],
    }
}

/// Takes the hook `write_needs_input_hook` wrote back out of a worktree an attended session has used,
/// for a session with nobody to ask. A worktree with no settings gets none. Returns notes, like it.
pub fn remove_needs_input_hook(worktree: &Path) -> Vec<String> {
    if std::fs::symlink_metadata(worktree.join(SETTINGS)).is_err() {
        return Vec::new();
    }
    let edited = edit_settings(worktree, |root| {
        let Some(hooks) = root.get_mut("hooks").and_then(|hooks| hooks.as_object_mut()) else { return Ok(()) };
        for (event, _) in NEEDS_INPUT_EVENTS {
            if let Some(entries) = hooks.get_mut(event).and_then(|entries| entries.as_array_mut()) {
                remove_own_hooks(entries);
            }
        }
        Ok(())
    });
    match edited {
        Ok(()) => Vec::new(),
        Err(why) => vec![format!("{SETTINGS} was not updated: {why}")],
    }
}

/// What a run's session may do without asking: edit the worktree, read, use its skills, the local
/// git commands, building and testing, and the MCP tools the implement-and-review skill works through.
const ALLOWED: &[&str] = &[
    "Edit(./**)", "Read", "Glob", "Grep", "Skill", "ToolSearch", "TodoWrite",
    "Bash(git status:*)", "Bash(git diff:*)", "Bash(git log:*)", "Bash(git show:*)", "Bash(git add:*)",
    "Bash(git commit:*)", "Bash(git rev-parse:*)", "Bash(git branch:*)", "Bash(git stash:*)",
    "Bash(git restore:*)", "Bash(git rm:*)", "Bash(git mv:*)", "Bash(git ls-files:*)", "Bash(git grep:*)",
    "Bash(git merge-base:*)", "Bash(cargo build:*)", "Bash(cargo test:*)", "Bash(cargo check:*)",
    "Bash(cargo fmt:*)", "Bash(cargo clippy:*)", "Bash(cargo run:*)", "Bash(cargo metadata:*)",
    "Bash(cargo tree:*)", "Bash(npm ci)", "Bash(ls:*)", "Bash(cat:*)",
    "Bash(head:*)", "Bash(wc:*)", "mcp__outside-in-tdd", "mcp__project-mcp",
];
/// Rules of orq's own that run a project's script from the folder that holds it, with the script's
/// file name: each is allowed only while every file of that name in the worktree is one the run's
/// base holds checked in and the run has not changed.
const ALLOWED_SCRIPTS: [(&str, &str); 1] = [("Bash(node build.mjs)", "build.mjs")];
/// Publishing to a remote is orq's own step, never the session's, whatever else is allowed.
const DENIED: [&str; 3] = ["Bash(git push:*)", "Bash(cargo publish:*)", "Bash(npm publish:*)"];

/// Shells, wrappers and interpreters run any command or code they are given, publishing included.
const RUNS_ANYTHING: [&str; 21] = [
    "sh", "bash", "zsh", "dash", "fish", "env", "xargs", "eval", "exec", "sudo", "nohup", "command", "python", "node", "perl",
    "ruby", "php", "deno", "bun", "lua", "osascript",
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
    let Some(command) = rule.strip_prefix("Bash(").and_then(|rest| rest.strip_suffix(')')) else {
        return false;
    };
    let command = command.trim_end();
    let command = command.strip_suffix(":*").or_else(|| command.strip_suffix('*')).unwrap_or(command);
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
    let plain = script.chars().all(|c| c.is_ascii_alphanumeric() || "._-+/".contains(c));
    let segments_plain = script.split('/').all(|segment| !matches!(segment, "" | "." | ".."));
    Some((plain && segments_plain && !script.starts_with('-')).then_some(script))
}

/// Whether `script` is a regular file of the commit `base`, read in `worktree`.
fn tracked_at(worktree: &Path, base: &str, script: &str) -> bool {
    if base.is_empty() || !base.chars().all(|c| c.is_ascii_hexdigit()) {
        return false;
    }
    let listed = Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(["--literal-pathspecs", "ls-tree", "-z", base, "--", script])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    let Ok(listed) = listed else { return false };
    let entry = String::from_utf8_lossy(&listed.stdout);
    let Some((meta, path)) = entry.trim_end_matches('\0').split_once('\t') else { return false };
    listed.status.success()
        && path == script
        && (meta.starts_with("100644 blob ") || meta.starts_with("100755 blob "))
}

/// Whether `script` in the worktree, committed or not, differs from the commit `base`; a failed
/// check counts as a change.
fn changed_since(worktree: &Path, base: &str, script: &str) -> bool {
    let same = Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(["--literal-pathspecs", "diff", "--quiet", base, "--", script])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    !same.is_ok_and(|status| status.success())
}

/// What the worktree holds of the files named `name`, in any folder, against the commit `base`.
enum NamedScripts {
    /// `base` holds no such file as a regular file, or git cannot tell.
    Absent,
    /// Repositories inside the worktree (submodules or not), whose files the worktree's git does not see.
    Nested(Vec<String>),
    /// The files that differ from what `base` holds: changed, added or removed in a commit of the
    /// run or in the worktree, untracked or ignored ones included. Empty when none does.
    Changed(Vec<String>),
}

fn named_scripts(worktree: &Path, base: &str, name: &str) -> NamedScripts {
    named_scripts_checked(worktree, base, name).unwrap_or(NamedScripts::Absent)
}

fn named_scripts_checked(worktree: &Path, base: &str, name: &str) -> Option<NamedScripts> {
    if !base.chars().all(|c| c.is_ascii_hexdigit()) || base.is_empty() {
        return None;
    }
    let pattern = format!(":(glob)**/{name}");
    let git = |args: &[&str]| -> Option<String> {
        let output = Command::new("git").arg("-C").arg(worktree).args(args).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
        output.status.success().then(|| String::from_utf8_lossy(&output.stdout).into_owned())
    };
    // ls-tree takes no glob: the base's files are listed and picked by name here.
    let held = git(&["ls-tree", "-r", "-z", base])?;
    let named = |entry: &&str| entry.split_once('\t').is_some_and(|(_, path)| path.rsplit('/').next() == Some(name));
    let entries: Vec<&str> = held.split('\0').filter(named).collect();
    let regular = |entry: &&str| entry.starts_with("100644 blob ") || entry.starts_with("100755 blob ");
    if entries.is_empty() || !entries.iter().all(regular) {
        return None;
    }
    // A submodule at the base or in the index, or a repository left untracked: listed file by file,
    // the untracked files show such a repository alone as a folder, however deep in an untracked
    // folder it lies (`--directory` would show only the outermost untracked folder).
    let staged = git(&["ls-files", "-s", "-z"])?;
    let gitlinks = held.split('\0').chain(staged.split('\0')).filter(|entry| entry.starts_with("160000 ")).filter_map(|entry| entry.split_once('\t')).map(|(_, path)| path.to_string());
    let others = git(&["ls-files", "--others", "-z"])?;
    let untracked = others.split('\0').filter_map(|entry| entry.strip_suffix('/')).map(str::to_string);
    let mut nested: Vec<String> = gitlinks.chain(untracked).collect();
    nested.sort();
    nested.dedup();
    if !nested.is_empty() {
        return Some(NamedScripts::Nested(nested));
    }
    let changed = git(&["diff", "--name-only", "-z", base, "--", &pattern])?;
    let added = git(&["ls-files", "--others", "-z", "--", &pattern])?;
    Some(NamedScripts::Changed(changed.split('\0').chain(added.split('\0')).filter(|path| !path.is_empty()).map(str::to_string).collect()))
}

/// Lets the worktree's session work with nobody to answer a prompt: its edits are accepted, the
/// commands and tools of `ALLOWED` and the project's `extra` (its `allowed_tools`) run without
/// asking, publishing is refused, and `logs` is writable so the session can leave its exit marker.
/// Anything else the session is refused. An added rule that would publish is left out; an
/// interpreter is kept only to run a script the run's `base` commit holds and the run has not
/// changed since; a rule left out is also taken out of an allow list an earlier call wrote. Other
/// keys and earlier entries are kept, and nothing is listed twice. Returns notes, never fails the run.
pub fn write_run_permissions(worktree: &Path, extra: &[String], logs: &Path, base: &str) -> Vec<String> {
    let (mut notes, unrevoked) = apply_run_permissions(worktree, extra, logs, base);
    notes.extend(unrevoked);
    notes
}

/// What [`write_run_permissions`] does: its notes, and why a rule left out is still allowed by
/// settings that could not be updated.
fn apply_run_permissions(worktree: &Path, extra: &[String], logs: &Path, base: &str) -> (Vec<String>, Option<String>) {
    let mut notes = Vec::new();
    let logs = logs.to_string_lossy();
    // A path rule beginning with `//` is absolute: the session writes its exit marker there.
    let marker_rule = format!("Edit(/{logs}/**)");
    let mut allowed: Vec<&str> = ALLOWED.to_vec();
    allowed.push(&marker_rule);
    // A rule left out now goes from the settings too: an earlier session may have been allowed it.
    let mut withdrawn: Vec<&str> = Vec::new();
    for (rule, name) in ALLOWED_SCRIPTS {
        match named_scripts(worktree, base, name) {
            // A project without the script has no use for the rule: it goes without a note.
            NamedScripts::Absent => withdrawn.push(rule),
            NamedScripts::Nested(repositories) => {
                let what = if repositories.len() == 1 { "is a repository of its own" } else { "are repositories of their own" };
                notes.push(format!("{} {what}, whose {name} orq cannot check, so the rule {rule} was left out", repositories.join(", ")));
                withdrawn.push(rule);
            }
            NamedScripts::Changed(changed) if changed.is_empty() => allowed.push(rule),
            NamedScripts::Changed(changed) => {
                notes.push(format!("{} was changed by this run, so the rule {rule} was left out", changed.join(", ")));
                withdrawn.push(rule);
            }
        }
    }
    for rule in extra {
        let could_publish = || format!("allowed_tools rule {rule} could publish to a remote, which orq does itself; it was left out, name the subcommands the run needs instead");
        let left_out = match interpreter_script(rule) {
            _ if names_publishing(rule) => Some(could_publish()),
            None => publishes(rule).then(could_publish),
            Some(Some(script)) if tracked_at(worktree, base, script) => {
                changed_since(worktree, base, script).then(|| format!("{script} was changed by this run, so allowed_tools rule {rule} was left out"))
            }
            Some(_) => Some(format!("allowed_tools rule {rule} runs an interpreter on something other than a checked-in script; it was left out")),
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
    let edited = edit_settings(worktree, |root| {
        let permissions = root.entry("permissions").or_insert_with(|| serde_json::json!({}));
        let permissions = permissions.as_object_mut().ok_or("its permissions is not a JSON object")?;
        permissions.insert("defaultMode".to_string(), serde_json::json!("acceptEdits"));
        for (key, rules) in [("allow", allowed.as_slice()), ("deny", DENIED.as_slice()), ("additionalDirectories", &[logs.as_ref()][..])] {
            let list = permissions.entry(key).or_insert_with(|| serde_json::json!([]));
            let list = list.as_array_mut().ok_or_else(|| format!("its permissions.{key} is not a JSON array"))?;
            if key == "allow" {
                list.retain(|item| !item.as_str().is_some_and(|item| withdrawn.contains(&item)));
            }
            for rule in rules {
                if !list.iter().any(|item| item.as_str() == Some(rule)) {
                    list.push(serde_json::json!(rule));
                }
            }
        }
        Ok(())
    });
    let Err(why) = edited else { return (notes, None) };
    // Rules a file Claude Code still reads allows; one it cannot parse allows nothing.
    let held = std::fs::read_to_string(worktree.join(SETTINGS)).ok().and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok());
    let still: Vec<&str> = withdrawn.into_iter().filter(|rule| held.as_ref().and_then(|held| held["permissions"]["allow"].as_array()).is_some_and(|list| list.iter().any(|item| item.as_str() == Some(rule)))).collect();
    if still.is_empty() {
        notes.push(format!("{SETTINGS} was not updated: {why}"));
        return (notes, None);
    }
    let unrevoked = format!("{SETTINGS} was not updated ({why}) and still allows {}, which must be left out", still.join(", "));
    (notes, Some(unrevoked))
}

/// Like [`write_run_permissions`], for a session that must not start with a rule it was allowed
/// before and must lose now: fails, saying which, when the settings still allow such a rule.
pub fn refresh_run_permissions(worktree: &Path, extra: &[String], logs: &Path, base: &str) -> Result<Vec<String>, String> {
    match apply_run_permissions(worktree, extra, logs, base) {
        (_, Some(unrevoked)) => Err(unrevoked),
        (notes, None) => Ok(notes),
    }
}

/// Takes out of `entries` the commands `write_needs_input_hook` wrote and nothing beside them: an
/// entry goes only when one of ours was all it held.
fn remove_own_hooks(entries: &mut Vec<serde_json::Value>) {
    entries.retain_mut(|entry| {
        let Some(hooks) = entry.get_mut("hooks").and_then(|hooks| hooks.as_array_mut()) else { return true };
        let held = hooks.len();
        hooks.retain(|hook| !hook.get("command").and_then(|c| c.as_str()).is_some_and(is_needs_input_command));
        hooks.len() == held || !hooks.is_empty()
    });
}

/// Whether `command` is exactly what Orq writes, `'<orq>' run needs-input '<run>' --repo '<root>'`, one
/// quoted word in each place: someone's own command, compound or merely with these words in it, stays.
fn is_needs_input_command(command: &str) -> bool {
    let whole = || {
        let rest = after_quoted_word(command)?.strip_prefix(NEEDS_INPUT_COMMAND)?;
        let rest = after_quoted_word(rest)?.strip_prefix(" --repo ")?;
        after_quoted_word(rest)
    };
    whole().is_some_and(str::is_empty)
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
    write().map_err(|err| format!("it could not be written ({:?})", err.kind()))
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
    let linked = std::fs::symlink_metadata(worktree.join(MCPCTL_TOML)).is_ok_and(|meta| meta.file_type().is_symlink());
    if linked {
        notes.push(format!("{MCPCTL_TOML} is a symbolic link in the worktree; no MCP server was set up"));
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

/// The servers the `[[mcp]]` tables of the worktree's `.mcpctl.toml` name. A file that cannot be
/// read selects none and a name that would read as an option is left out, each with a note; a
/// missing file is simply no selection.
fn selected_in_mcpctl_toml(worktree: &Path, notes: &mut Vec<String>) -> Vec<String> {
    let text = match std::fs::read_to_string(worktree.join(MCPCTL_TOML)) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(err) => {
            notes.push(format!("{MCPCTL_TOML} could not be read ({:?}); no MCP server was set up", err.kind()));
            return Vec::new();
        }
    };
    let Ok(doc) = text.parse::<toml_edit::DocumentMut>() else {
        notes.push(format!("{MCPCTL_TOML} is not valid TOML; no MCP server was set up"));
        return Vec::new();
    };
    let tables = doc.get("mcp").and_then(|item| item.as_array_of_tables());
    let names = tables.into_iter().flat_map(|tables| tables.iter());
    let names = names.filter_map(|table| table.get("name")?.as_str());
    let (options, names): (Vec<&str>, Vec<&str>) = names.partition(|name| name.starts_with('-'));
    if !options.is_empty() {
        notes.push(format!("{MCPCTL_TOML} names MCP servers that look like options; they were left out"));
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
    let mut seen = std::collections::HashSet::new();
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
