//! What a run's worktree needs so that the agent session, the MCP servers and a hand-resumed session
//! all see the run's environment: the git-ignored `.claude/settings.local.json`, the MCP servers it
//! approves, and the one-time `mcpctl` setup. Every problem is a note, never a failed run, and
//! environment values are never put into a note.

use std::path::Path;
use std::process::{Command, Stdio};

use crate::local_files::{crosses_symlink, exclude_locally};

const SETTINGS: &str = ".claude/settings.local.json";
const WORKTREES_DIR: &str = ".claude/worktrees";

/// Merges `env` into the `env` object of the worktree's `.claude/settings.local.json` and approves
/// MCP servers in `enabledMcpjsonServers`: those of `servers` that `.mcp.json` declares, or every
/// server it declares when `servers` is `None`. Other keys and earlier entries are kept.
pub fn write_session_settings(
    worktree: &Path,
    env: &[(String, String)],
    servers: Option<&[String]>,
) -> Vec<String> {
    let mut notes = Vec::new();
    let declared = declared_servers(worktree, &mut notes);
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

/// The `<package>` of a server entry started with `mcpctl run <package>`.
fn mcpctl_package(entry: &serde_json::Value) -> Option<String> {
    let command = Path::new(entry.get("command")?.as_str()?);
    if command.file_name()? != "mcpctl" {
        return None;
    }
    match entry.get("args")?.as_array()?.as_slice() {
        [run, package, ..] if run.as_str() == Some("run") => package.as_str().map(str::to_string),
        _ => None,
    }
}

/// The servers `.mcp.json` declares; a missing or unreadable file is a note.
fn declared_servers(worktree: &Path, notes: &mut Vec<String>) -> Vec<DeclaredServer> {
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
                        package: mcpctl_package(entry),
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

/// Runs `mcpctl setup --mcp <name>` in `worktree` for each of `servers`, with `env` added to the
/// environment. `mcpctl` is the program, `None` when it is not installed.
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
    let mut notes = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for name in servers.iter().filter(|name| seen.insert(name.as_str())) {
        let done = Command::new(program)
            .args(["setup", "--mcp", name])
            .current_dir(worktree)
            .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        match done {
            Ok(status) if status.success() => {}
            Ok(status) => notes.push(format!(
                "mcpctl setup for MCP server {name} failed ({status})"
            )),
            Err(err) => notes.push(format!(
                "mcpctl setup for MCP server {name} could not be started ({:?})",
                err.kind()
            )),
        }
    }
    notes
}

/// Keeps the directory run worktrees live in out of the main checkout's git status, through the local
/// exclude only. `None` when done (or already ignored); otherwise the note.
pub fn exclude_worktrees_dir(main_checkout: &Path) -> Option<String> {
    exclude_locally(main_checkout, WORKTREES_DIR)
        .err()
        .map(|why| format!("{WORKTREES_DIR} could not be kept out of git status: {why}"))
}
