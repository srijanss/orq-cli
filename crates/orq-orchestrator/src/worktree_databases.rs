//! Giving each run's worktree its own test database. The main checkout's dotenv file names the
//! databases (`DB_NAME=shop`); a worktree for branch `fix--COM-01` is handed `DB_NAME=shop_fix_com_01`
//! instead, so the test databases Django creates (`test_<name>`) of concurrent runs never collide.
//! Only the named variables are read from the dotenv file, and only database names ever leave this
//! module: no other value from the file is returned, logged or put in a note.

use std::path::Path;

/// The longest database name MySQL accepts.
const MAX_DATABASE_NAME: usize = 64;
/// What Django puts in front of a test database's name.
const TEST_PREFIX: &str = "test_";
/// The shortest slice of the branch worth keeping next to the hash.
const MIN_KEPT_SLUG: usize = 4;

/// The environment a worktree gets, and why anything expected is missing from it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorktreeDatabases {
    pub env: Vec<(String, String)>,
    pub notes: Vec<String>,
}

/// The per-worktree database names for `branch`: one entry per variable of `vars` that the main
/// checkout's dotenv file gives a plain database name. `named_by_repo` is whether the repository
/// chose `vars` itself; only then is a variable the file lacks worth a note.
pub fn derive_worktree_databases(main_checkout: &Path, vars: &[String], branch: &str, named_by_repo: bool) -> WorktreeDatabases {
    let mut found = WorktreeDatabases::default();
    let text = match std::fs::read_to_string(main_checkout.join(".env")) {
        Ok(text) => text,
        // No dotenv file simply means there is nothing to isolate.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return found,
        Err(err) => {
            found.notes.push(format!("the main checkout's dotenv file cannot be read ({:?}); its databases are not isolated", err.kind()));
            return found;
        }
    };
    let slug = branch_slug(branch);
    for var in vars {
        let Some(main) = dotenv_value(&text, var) else {
            if named_by_repo {
                found.notes.push(format!("{var} has no value in the main checkout's dotenv file; its database is not isolated"));
            }
            continue;
        };
        if !main.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            found.notes.push(format!("{var} is not a plain database name; its database is not isolated"));
            continue;
        }
        // "test_" + <main> + "_" + <branch part>
        let room = MAX_DATABASE_NAME.saturating_sub(TEST_PREFIX.len() + main.len() + 1);
        let part = if slug.len() <= room {
            slug.clone()
        } else {
            let hash = stable_hash(branch).to_string();
            let keep = room.saturating_sub(1 + hash.len());
            if keep < MIN_KEPT_SLUG {
                found.notes.push(format!("{var} is too long to add a branch suffix; its database is not isolated"));
                continue;
            }
            format!("{}_{hash}", &slug[..keep])
        };
        found.env.push((var.clone(), format!("{main}_{part}")));
    }
    found
}

/// Lowercase letters and digits, every other run of characters one underscore.
fn branch_slug(branch: &str) -> String {
    let mut slug = String::new();
    for c in branch.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.ends_with('_') {
            slug.push('_');
        }
    }
    slug.trim_matches('_').to_string()
}

/// FNV-1a: the same branch always gets the same suffix, whatever the platform or release.
fn stable_hash(text: &str) -> u32 {
    text.bytes().fold(0x811c_9dc5_u32, |hash, byte| (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193))
}

/// The value of the last `[export] NAME = value` line, without quotes or a trailing ` # comment`.
fn dotenv_value(text: &str, name: &str) -> Option<String> {
    let mut found = None;
    for line in text.lines() {
        let line = line.trim_start();
        let line = line.strip_prefix("export").filter(|rest| rest.starts_with([' ', '\t'])).unwrap_or(line).trim_start();
        let Some(rest) = line.strip_prefix(name) else { continue };
        let Some(value) = rest.trim_start().strip_prefix('=') else { continue };
        let value = value.trim();
        let value = match value.find(" #").or_else(|| value.find("\t#")) {
            Some(at) if !value.starts_with(['"', '\'']) => value[..at].trim_end(),
            _ => value,
        };
        let value = match value.chars().next() {
            Some(quote @ ('"' | '\'')) => {
                let inner = &value[1..];
                inner.find(quote).map_or(inner, |end| &inner[..end])
            }
            _ => value,
        };
        found = Some(value.to_string()).filter(|v| !v.is_empty());
    }
    found
}
