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
