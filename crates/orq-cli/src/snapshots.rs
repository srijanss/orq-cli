//! `orq snapshots`: the ledger and database copies orq keeps before it changes them.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

use orq_core::storage::{list_database_snapshots, list_ledger_snapshots, readable_utc as readable, LedgerSnapshot, StorageRoots};

pub(crate) fn snapshots(args: &[OsString]) -> ExitCode {
    match args.first().and_then(|a| a.to_str()) {
        Some("list") => list(&args[1..]),
        Some("diff") => diff(&args[1..]),
        Some("restore") => restore(&args[1..]),
        _ => crate::usage_error(),
    }
}

fn flag_value(args: &[OsString], flag: &str) -> Option<PathBuf> {
    let at = args.iter().position(|a| a == flag)?;
    args.get(at + 1).map(PathBuf::from)
}

fn roots() -> Result<StorageRoots, String> {
    StorageRoots::resolve(&|name| std::env::var(name).ok()).map_err(|err| format!("cannot locate the Orq data directory: {err}"))
}

/// "draft 1, pending 0, in_progress 0, completed 0" for the snapshot's ledger.
fn counts(snapshot: &LedgerSnapshot) -> String {
    let Ok(text) = std::fs::read_to_string(&snapshot.path) else { return "unreadable".to_string() };
    let Ok(entries) = orq_features::parse_ledger(&text) else { return "unreadable".to_string() };
    ["draft", "pending", "in_progress", "completed"]
        .iter()
        .map(|status| format!("{status} {}", entries.iter().filter(|e| e.status.as_str() == *status).count()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The ledger snapshot `name` (or a path) names, for the project at `repo` (default: the working directory).
fn find_snapshot(roots: &StorageRoots, repo: Option<PathBuf>, name: &str) -> Result<(PathBuf, PathBuf), String> {
    let repo = repo.map_or_else(|| std::env::current_dir().map_err(|e| e.to_string()), Ok)?;
    let project = orq_git::resolve_project(&repo).map_err(|err| format!("{}: {err:?}", repo.display()))?;
    let id = orq_git::project_id(&project);
    let file_name = std::path::Path::new(name).file_name().ok_or_else(|| format!("{name:?} is not a snapshot name"))?;
    let found = list_ledger_snapshots(&roots.data, &id).into_iter().find(|s| s.path.file_name() == Some(file_name));
    let snapshot = found.ok_or_else(|| format!("no snapshot {name} for this project; `orq snapshots list --project <path>` shows them"))?;
    Ok((snapshot.path, project.root.join(".tdd-features.json")))
}

fn read_entries(path: &std::path::Path) -> Result<Vec<orq_features::FeatureEntry>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    orq_features::parse_ledger(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// What changed from the snapshot's ledger to the current one, one line per feature.
fn diff_lines(old: &[orq_features::FeatureEntry], new: &[orq_features::FeatureEntry]) -> Vec<String> {
    let mut lines = Vec::new();
    for entry in new {
        match old.iter().find(|o| o.feature_name == entry.feature_name) {
            None => lines.push(format!("added: {}", entry.feature_name)),
            Some(before) if before.status != entry.status => {
                lines.push(format!("changed: {} (status {} -> {})", entry.feature_name, before.status.as_str(), entry.status.as_str()))
            }
            Some(_) => {}
        }
    }
    for entry in old {
        if !new.iter().any(|n| n.feature_name == entry.feature_name) {
            lines.push(format!("removed: {}", entry.feature_name));
        }
    }
    lines
}

fn diff(args: &[OsString]) -> ExitCode {
    let roots = match roots() {
        Ok(roots) => roots,
        Err(message) => return crate::fail(message),
    };
    let Some(name) = args.first().and_then(|a| a.to_str()) else { return crate::usage_error() };
    let (snapshot, ledger) = match find_snapshot(&roots, flag_value(args, "--repo"), name) {
        Ok(found) => found,
        Err(message) => return crate::fail(message),
    };
    let (old, new) = match (read_entries(&snapshot), read_entries(&ledger)) {
        (Ok(old), Ok(new)) => (old, new),
        (Err(message), _) | (_, Err(message)) => return crate::fail(message),
    };
    for line in diff_lines(&old, &new) {
        println!("{}", crate::printable(&line));
    }
    ExitCode::SUCCESS
}

/// Shows what restoring would change, then replaces the ledger with the snapshot once the person
/// types `yes` (or passed `--yes`). The save snapshots the ledger it replaces first.
fn restore(args: &[OsString]) -> ExitCode {
    let roots = match roots() {
        Ok(roots) => roots,
        Err(message) => return crate::fail(message),
    };
    let Some(name) = args.first().and_then(|a| a.to_str()) else { return crate::usage_error() };
    if name.starts_with("orq-db-") {
        return restore_database(&roots, name);
    }
    let (snapshot, ledger) = match find_snapshot(&roots, flag_value(args, "--repo"), name) {
        Ok(found) => found,
        Err(message) => return crate::fail(message),
    };
    let text = match std::fs::read_to_string(&snapshot) {
        Ok(text) => text,
        Err(e) => return crate::fail(format!("{}: {e}", snapshot.display())),
    };
    let restored = match orq_features::parse_ledger(&text) {
        Ok(entries) => entries,
        Err(e) => return crate::fail(format!("{}: {e}", snapshot.display())),
    };
    let loaded = match orq_features::LoadedLedger::load_or_new(&ledger) {
        Ok(loaded) => loaded,
        Err(e) => return crate::fail(format!("{}: {e:?}", ledger.display())),
    };
    for line in diff_lines(&restored, loaded.entries()) {
        // Restoring undoes what `diff` shows, so the sense is the diff's, seen from the snapshot.
        println!("{}", crate::printable(&line));
    }
    if !args.iter().any(|a| a == "--yes") {
        eprint!("Replace {} with this snapshot? Type yes to continue: ", ledger.display());
        let mut answer = String::new();
        let _ = std::io::stdin().read_line(&mut answer);
        if answer.trim() != "yes" {
            return crate::fail("not restored: the ledger is unchanged");
        }
    }
    match loaded.save(&text) {
        Ok(()) => {
            println!("restored {}", ledger.display());
            ExitCode::SUCCESS
        }
        Err(e) => crate::fail(format!("could not restore the ledger: {e:?}")),
    }
}

/// Replaces orq.db with a database snapshot. Refused while `orq serve` runs, which holds the database open.
fn restore_database(roots: &StorageRoots, name: &str) -> ExitCode {
    if let Some(server) = crate::remote_serve::live_servers(roots).first() {
        let how = match &server.unit {
            Some(unit) => format!("systemctl --user stop {unit}"),
            None => format!("kill {}", server.pid),
        };
        return crate::fail(format!("orq serve is running (pid {}); stop it first (`{how}`), restore, then start it again", server.pid));
    }
    let Some(file_name) = std::path::Path::new(name).file_name() else { return crate::fail(format!("{name:?} is not a snapshot name")) };
    let Some((snapshot, _)) = list_database_snapshots(&roots.data).into_iter().find(|(path, _)| path.file_name() == Some(file_name)) else {
        return crate::fail(format!("no database snapshot {name}; `orq snapshots list` shows them"));
    };
    let database = roots.database();
    let temp = roots.data.join(format!(".orq-db-restore-{}.tmp", std::process::id()));
    let replaced = std::fs::copy(&snapshot, &temp).and_then(|_| std::fs::rename(&temp, &database));
    if let Err(e) = replaced {
        let _ = std::fs::remove_file(&temp);
        return crate::fail(format!("could not restore the database: {e}"));
    }
    // The old database's journal files do not belong to the restored one.
    for suffix in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(roots.data.join(format!("orq.db{suffix}")));
    }
    println!("restored {}", database.display());
    ExitCode::SUCCESS
}

fn list(args: &[OsString]) -> ExitCode {
    let roots = match roots() {
        Ok(roots) => roots,
        Err(message) => return crate::fail(message),
    };
    let ids: Vec<String> = match flag_value(args, "--project") {
        Some(repo) => match orq_git::resolve_project(&repo) {
            Ok(project) => vec![orq_git::project_id(&project)],
            Err(err) => return crate::fail(format!("{}: {err:?}", repo.display())),
        },
        None => {
            for (path, written) in list_database_snapshots(&roots.data) {
                let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                println!("{}  {}  {size} bytes  database", readable(written), path.file_name().unwrap_or_default().to_string_lossy());
            }
            let mut ids: Vec<String> = std::fs::read_dir(orq_core::storage::snapshots_dir(&roots.data))
                .into_iter()
                .flatten()
                .flatten()
                .filter(|entry| entry.path().is_dir())
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect();
            ids.sort();
            ids
        }
    };
    for id in ids {
        for snapshot in list_ledger_snapshots(&roots.data, &id) {
            let size = std::fs::metadata(&snapshot.path).map(|m| m.len()).unwrap_or(0);
            let name = snapshot.path.file_name().unwrap_or_default().to_string_lossy();
            println!("{}  {name}  {size} bytes  {}", readable(snapshot.taken_at), counts(&snapshot));
        }
    }
    ExitCode::SUCCESS
}
