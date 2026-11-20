//! A project's own settings, kept in orq.db on this machine and keyed by the path of the project's
//! main checkout: the agents a run uses when it names none, and what a run does when it finishes.
//! A role left unset falls back to `.tdd-config.json`, `[roles]` and the machine defaults.

use std::io::{Error, ErrorKind, Result};

/// What a finished run does with its branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Delivery {
    /// Push, open the PR/MR and wait for CI.
    #[default]
    Review,
    /// Pushed the branch and stopped. Removed: no run or setting may choose it, and it is kept only
    /// so a run record that names it still reads.
    Push,
    /// Fast-forward the base branch locally; nothing is pushed.
    MergeLocal,
}

/// Why a run, a flag or a setting may not name [`Delivery::Push`].
pub const PUSH_REMOVED: &str = "the push delivery was removed: use review (pushes and opens a review) or merge-local";

impl Delivery {
    pub fn as_str(self) -> &'static str {
        match self {
            Delivery::Review => "review",
            Delivery::Push => "push",
            Delivery::MergeLocal => "merge_local",
        }
    }

    pub fn parse(text: &str) -> Option<Delivery> {
        match text {
            "review" => Some(Delivery::Review),
            "push" => Some(Delivery::Push),
            "merge_local" => Some(Delivery::MergeLocal),
            _ => None,
        }
    }
}

/// What the server does once it sees a run's MR merged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AfterMerge {
    /// Offer Clean up on the run, its review and the project, for a person to press.
    #[default]
    Ask,
    /// Clean up on its own, and send a notification saying how it went.
    Auto,
}

impl AfterMerge {
    pub fn as_str(self) -> &'static str {
        match self {
            AfterMerge::Ask => "ask",
            AfterMerge::Auto => "auto",
        }
    }

    pub fn parse(text: &str) -> Option<AfterMerge> {
        match text {
            "ask" => Some(AfterMerge::Ask),
            "auto" => Some(AfterMerge::Auto),
            _ => None,
        }
    }
}

/// What a project's `.orq/run.toml` `[after_merge]` table asks for once a merge-local run's merge
/// lands: argv lists, run in order in the project's main checkout. Never shell text.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AfterMergeCommands {
    pub commands: Vec<Vec<String>>,
}

impl AfterMergeCommands {
    /// The `[after_merge]` table of `run.toml` text: `None` when it has none, and an error naming
    /// the key when `commands` is not a list of non-empty argv lists of strings.
    pub fn parse(text: &str) -> std::result::Result<Option<AfterMergeCommands>, String> {
        let doc: toml_edit::DocumentMut = text.parse().map_err(|error| format!("run.toml is not valid TOML: {error}"))?;
        let Some(table) = doc.get("after_merge") else { return Ok(None) };
        let Some(table) = table.as_table_like() else { return Err("after_merge must be a table".to_string()) };
        let Some(list) = table.get("commands") else { return Ok(Some(AfterMergeCommands::default())) };
        let Some(list) = list.as_array() else { return Err("after_merge.commands must be a list of argv lists".to_string()) };
        let mut commands = Vec::new();
        for (i, argv) in list.iter().enumerate() {
            let key = format!("after_merge.commands[{i}]");
            let Some(argv) = argv.as_array() else { return Err(format!("{key} must be a list of strings, not shell text")) };
            let mut words = Vec::new();
            for (j, word) in argv.iter().enumerate() {
                let Some(word) = word.as_str() else { return Err(format!("{key}[{j}] must be a string")) };
                words.push(word.to_string());
            }
            if words.first().is_none_or(|program| program.is_empty()) {
                return Err(format!("{key} must start with a program name"));
            }
            commands.push(words);
        }
        Ok(Some(AfterMergeCommands { commands }))
    }
}

/// Whether a project's merge-local merges get a release commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReleaseOnMerge {
    #[default]
    Off,
    Auto,
}

/// What a project's `.orq/run.toml` `[release]` table asks for after a merge-local merge: the file and
/// key holding the version, and the argv lists run in the release worktree after the bump.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseSettings {
    pub release_on_merge: ReleaseOnMerge,
    pub version_file: String,
    pub version_key: String,
    pub commands: Vec<Vec<String>>,
}

impl ReleaseSettings {
    /// The `[release]` table of `run.toml` text: `None` when it has none, and an error naming the key
    /// when a value is not what the key takes.
    pub fn parse(text: &str) -> std::result::Result<Option<ReleaseSettings>, String> {
        let doc: toml_edit::DocumentMut = text.parse().map_err(|error| format!("run.toml is not valid TOML: {error}"))?;
        let Some(table) = doc.get("release") else { return Ok(None) };
        let Some(table) = table.as_table_like() else { return Err("release must be a table".to_string()) };
        let release_on_merge = match table.get("release_on_merge") {
            None => ReleaseOnMerge::Off,
            Some(value) => match value.as_str() {
                Some("off") => ReleaseOnMerge::Off,
                Some("auto") => ReleaseOnMerge::Auto,
                _ => return Err("release.release_on_merge must be \"off\" or \"auto\"".to_string()),
            },
        };
        let text_of = |key: &str, default: &str| -> std::result::Result<String, String> {
            match table.get(key) {
                None => Ok(default.to_string()),
                Some(value) => match value.as_str() {
                    Some(text) if !text.is_empty() => Ok(text.to_string()),
                    _ => Err(format!("release.{key} must be a non-empty string")),
                },
            }
        };
        let version_file = text_of("version_file", "Cargo.toml")?;
        let version_key = text_of("version_key", "workspace.package.version")?;
        let mut commands = Vec::new();
        if let Some(list) = table.get("commands") {
            let Some(list) = list.as_array() else { return Err("release.commands must be a list of argv lists".to_string()) };
            for (i, argv) in list.iter().enumerate() {
                let key = format!("release.commands[{i}]");
                let Some(argv) = argv.as_array() else { return Err(format!("{key} must be a list of strings, not shell text")) };
                let mut words = Vec::new();
                for (j, word) in argv.iter().enumerate() {
                    let Some(word) = word.as_str() else { return Err(format!("{key}[{j}] must be a string")) };
                    words.push(word.to_string());
                }
                if words.first().is_none_or(|program| program.is_empty()) {
                    return Err(format!("{key} must start with a program name"));
                }
                commands.push(words);
            }
        }
        Ok(Some(ReleaseSettings { release_on_merge, version_file, version_key, commands }))
    }
}

/// The version string at dotted `key` of `text`, a TOML (`.toml`) or JSON (`.json`) file called `file`.
pub fn version_in(file: &str, text: &str, key: &str) -> std::result::Result<String, String> {
    let parts: Vec<&str> = key.split('.').collect();
    if file.ends_with(".toml") {
        let doc: toml_edit::DocumentMut = text.parse().map_err(|error| format!("{file} is not valid TOML: {error}"))?;
        let mut item = doc.as_item();
        for part in &parts {
            item = item.get(part).ok_or_else(|| format!("{file} has no {key}"))?;
        }
        item.as_str().map(str::to_string).ok_or_else(|| format!("{key} in {file} is not a string"))
    } else if file.ends_with(".json") {
        let doc: serde_json::Value = serde_json::from_str(text).map_err(|error| format!("{file} is not valid JSON: {error}"))?;
        let mut value = &doc;
        for part in &parts {
            value = value.get(part).ok_or_else(|| format!("{file} has no {key}"))?;
        }
        value.as_str().map(str::to_string).ok_or_else(|| format!("{key} in {file} is not a string"))
    } else {
        Err(format!("the version file {file} is neither .toml nor .json"))
    }
}

/// `text` with the version at dotted `key` replaced by `version`, the rest of the file as it was.
pub fn with_version(file: &str, text: &str, key: &str, version: &str) -> std::result::Result<String, String> {
    let old = version_in(file, text, key)?;
    if file.ends_with(".toml") {
        let mut doc: toml_edit::DocumentMut = text.parse().map_err(|error| format!("{file} is not valid TOML: {error}"))?;
        let mut item = doc.as_item_mut();
        for part in key.split('.') {
            item = item.get_mut(part).ok_or_else(|| format!("{file} has no {key}"))?;
        }
        let value = item.as_value_mut().ok_or_else(|| format!("{key} in {file} is not a string"))?;
        let decor = value.decor().clone();
        *value = toml_edit::Value::from(version);
        *value.decor_mut() = decor;
        Ok(doc.to_string())
    } else {
        // JSON is edited as text, so its layout and key order stay: the one `"name": "old"` pair.
        let name = key.rsplit('.').next().unwrap_or(key);
        let (quoted_name, quoted_old) = (format!("\"{name}\""), format!("\"{old}\""));
        let mut found = Vec::new();
        for (at, _) in text.match_indices(&quoted_name) {
            let rest = text[at + quoted_name.len()..].trim_start();
            let Some(rest) = rest.strip_prefix(':') else { continue };
            if rest.trim_start().starts_with(&quoted_old) {
                found.push(text.len() - rest.trim_start().len());
            }
        }
        match found.as_slice() {
            [start] => Ok(format!("{}\"{version}\"{}", &text[..*start], &text[start + quoted_old.len()..])),
            _ => Err(format!("{key} in {file} is not written once as \"{name}\": \"{old}\"")),
        }
    }
}

/// The agent a project uses for one role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectAgent {
    pub harness: String,
    pub model: String,
    /// The run's thinking level; unset leaves the harness's own default.
    pub effort: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectSettings {
    pub implementer: Option<ProjectAgent>,
    pub reviewer: Option<ProjectAgent>,
    pub delivery: Delivery,
    /// How many of the project's runs may be active at once; unset is no limit.
    pub max_active_runs: Option<u8>,
    /// The free disk, in GB, a run needs to start or resume; unset is no check.
    pub min_free_disk_gb: Option<u32>,
    /// When the queue may start runs, as `HH:MM-HH:MM` in local time (the end excluded; a window
    /// may wrap midnight); unset is any time.
    pub queue_hours: Option<String>,
    /// Whether a queued run that fails or is blocked pauses the queue.
    pub queue_stop_on_failure: bool,
    /// Whether the queue starts nothing more while a run it started waits for review: an open MR,
    /// or a run that needs a person's review. On by default; off, only the run limit holds it.
    pub queue_wait_for_review: bool,
    /// Whether a project whose runs end in an MR has its local target fast-forwarded from the
    /// remote after an MR merges and before each run. On by default; off, the target is left as it is.
    pub pull_target_before_runs: bool,
    /// What happens once a run's MR merged: Clean up offered (the default), or done unasked.
    pub after_merge: AfterMerge,
}

impl Default for ProjectSettings {
    fn default() -> ProjectSettings {
        ProjectSettings {
            implementer: None,
            reviewer: None,
            delivery: Delivery::default(),
            max_active_runs: None,
            min_free_disk_gb: None,
            queue_hours: None,
            queue_stop_on_failure: false,
            queue_wait_for_review: true,
            pull_target_before_runs: true,
            after_merge: AfterMerge::default(),
        }
    }
}

/// The range a project's run limit may take.
pub const MAX_ACTIVE_RUNS: std::ops::RangeInclusive<u8> = 1..=8;
/// The range a project's disk floor may take, in GB.
pub const MIN_FREE_DISK_GB: std::ops::RangeInclusive<u32> = 1..=500;

/// A project's settings with the revision a later save must name.
/// Revision 0 means the project has no record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredProjectSettings {
    pub settings: ProjectSettings,
    pub revision: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveOutcome {
    Saved { revision: u64 },
    /// The project was saved since the revision the save was based on; nothing was written.
    Conflict,
}

/// Every value is a single non-empty line with no surrounding blanks, the delivery is one that
/// may still be chosen, and each limit is in its range.
pub(crate) fn check(settings: &ProjectSettings) -> Result<()> {
    if settings.delivery == Delivery::Push {
        return Err(Error::new(ErrorKind::InvalidInput, PUSH_REMOVED));
    }
    if let Some(limit) = settings.max_active_runs.filter(|limit| !MAX_ACTIVE_RUNS.contains(limit)) {
        return Err(Error::new(ErrorKind::InvalidInput, format!("max_active_runs must be empty or 1 to 8, got {limit}")));
    }
    if let Some(floor) = settings.min_free_disk_gb.filter(|floor| !MIN_FREE_DISK_GB.contains(floor)) {
        return Err(Error::new(ErrorKind::InvalidInput, format!("min_free_disk_gb must be empty or 1 to 500, got {floor}")));
    }
    if let Some(hours) = settings.queue_hours.as_deref().filter(|hours| parse_queue_hours(hours).is_none()) {
        return Err(Error::new(ErrorKind::InvalidInput, format!("queue_hours must be empty or HH:MM-HH:MM with two different times, got {hours:?}")));
    }
    for (role, agent) in [("implementer", &settings.implementer), ("reviewer", &settings.reviewer)] {
        let Some(agent) = agent else { continue };
        check_value(role, "harness", &agent.harness)?;
        check_value(role, "model", &agent.model)?;
        if let Some(effort) = &agent.effort {
            check_value(role, "effort", effort)?;
        }
    }
    Ok(())
}

fn check_value(role: &str, field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() || value.trim() != value || value.chars().any(char::is_control) {
        return Err(Error::new(ErrorKind::InvalidInput, format!("{role} {field} must be a single non-blank line without surrounding spaces, got {value:?}")));
    }
    Ok(())
}

/// The start and end of a queue window `HH:MM-HH:MM`, in minutes after midnight; `None` when it is
/// not one, or starts and ends at the same time.
pub fn parse_queue_hours(hours: &str) -> Option<(u16, u16)> {
    let minute = |time: &str| -> Option<u16> {
        let (h, m) = time.split_once(':')?;
        if h.len() != 2 || m.len() != 2 || !h.bytes().chain(m.bytes()).all(|b| b.is_ascii_digit()) {
            return None;
        }
        let (h, m): (u16, u16) = (h.parse().ok()?, m.parse().ok()?);
        (h < 24 && m < 60).then_some(h * 60 + m)
    };
    let (start, end) = hours.split_once('-')?;
    let (start, end) = (minute(start)?, minute(end)?);
    (start != end).then_some((start, end))
}

/// Whether `minute_of_day` (minutes after local midnight) falls inside the window `hours`: from its
/// start up to, not including, its end, across midnight when the end comes first. A window that
/// cannot be read holds no time.
pub fn within_queue_hours(hours: &str, minute_of_day: u16) -> bool {
    match parse_queue_hours(hours) {
        Some((start, end)) if start < end => (start..end).contains(&minute_of_day),
        Some((start, end)) => minute_of_day >= start || minute_of_day < end,
        None => false,
    }
}
