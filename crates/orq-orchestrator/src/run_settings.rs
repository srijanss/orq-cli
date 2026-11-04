//! Per-repository run settings: forge, base branch, gate, setup steps and the CI wait
//! timeout. They come from a private file in the git directory (`orq/run.toml`), a checked-in
//! `.orq/run.toml`, and `ORQ_*` environment variables; the environment wins over the private
//! file, which wins over the checked-in one, key by key. Commands are argv lists, never shell
//! text, and every value is validated before a run uses it.

use std::path::Path;
use std::time::Duration;

use crate::outcome::git_raw;
use crate::review_request::ReviewPeople;

const SHARED_FILE: &str = ".orq/run.toml";
const PRIVATE_LABEL: &str = "orq/run.toml in the git directory";
/// A CI wait longer than a day is a typo, not a wish.
const MAX_CI_TIMEOUT_SECS: i64 = 86_400;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunSettings {
    /// `gh` or `glab`.
    pub forge: Option<String>,
    pub base_branch: Option<String>,
    /// The gate command; `None` leaves the project's own detection in charge.
    pub gate: Option<Vec<String>>,
    /// Extra setup steps, run before the detected install step.
    pub setup: Vec<Vec<String>>,
    pub ci_timeout: Option<Duration>,
    /// Who a new review request is assigned to, and who is asked to review it.
    pub mr_assignee: Option<String>,
    pub mr_reviewer: Option<String>,
    /// The command that tells a person a run ended, and which endings it is run for (`all` or `failures`).
    pub notify_cmd: Option<Vec<String>>,
    pub notify_on: Option<String>,
    /// Names of environment variables the notifier may keep (a webhook address, a token).
    pub notify_env: Option<Vec<String>>,
    /// Names of the variables in the main checkout's dotenv file that hold database names, each of
    /// which a run's worktree gets its own value for. `None` leaves the default (`DB_NAME`, `B2C_DB_NAME`).
    pub test_db_vars: Option<Vec<String>>,
    /// The git-ignored files of the main checkout a run's worktree gets. `None` leaves the default
    /// (the dotenv file, `.mcp.json`, `.mcpctl.toml`).
    pub local_files: Option<Vec<String>>,
    /// The MCP servers of `.mcp.json` a run's session approves and `mcpctl setup` is run for. `None`
    /// approves every declared server and sets none up.
    pub mcp_servers: Option<Vec<String>>,
    /// The command that drops a test database; the database's name is appended as the last word.
    pub test_db_drop_cmd: Option<Vec<String>>,
    /// Leave the test databases of a finished run in place (`ORQ_KEEP_TEST_DB`).
    pub keep_test_db: Option<bool>,
    /// The shape of a per-worktree database name, with `{name}` and `{branch}`.
    pub db_name_template: Option<String>,
    /// The model for the implement session and the review draft; `None` leaves the chosen profile's own.
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsError {
    /// A settings file exists but cannot be read.
    Unreadable { path: String, reason: String },
    /// A value (or a whole file) is not acceptable; `source` names the file or variable.
    Invalid { source: String, reason: String },
}

impl std::fmt::Display for SettingsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SettingsError::Unreadable { path, reason } => write!(f, "cannot read {path}: {reason}"),
            SettingsError::Invalid { source, reason } => write!(f, "{source}: {reason}"),
        }
    }
}

impl std::error::Error for SettingsError {}

/// One source's values, before the sources are merged.
#[derive(Default)]
struct Layer {
    forge: Option<String>,
    base_branch: Option<String>,
    gate: Option<Vec<String>>,
    setup: Option<Vec<Vec<String>>>,
    ci_timeout: Option<Duration>,
    mr_assignee: Option<String>,
    mr_reviewer: Option<String>,
    notify_cmd: Option<Vec<String>>,
    notify_on: Option<String>,
    notify_env: Option<Vec<String>>,
    test_db_vars: Option<Vec<String>>,
    local_files: Option<Vec<String>>,
    mcp_servers: Option<Vec<String>>,
    test_db_drop_cmd: Option<Vec<String>>,
    keep_test_db: Option<bool>,
    db_name_template: Option<String>,
    model: Option<String>,
}

impl Layer {
    /// `self` wins; whatever it leaves unset comes from `lower`.
    fn over(self, lower: Layer) -> Layer {
        Layer {
            forge: self.forge.or(lower.forge),
            base_branch: self.base_branch.or(lower.base_branch),
            gate: self.gate.or(lower.gate),
            setup: self.setup.or(lower.setup),
            ci_timeout: self.ci_timeout.or(lower.ci_timeout),
            mr_assignee: self.mr_assignee.or(lower.mr_assignee),
            mr_reviewer: self.mr_reviewer.or(lower.mr_reviewer),
            notify_cmd: self.notify_cmd.or(lower.notify_cmd),
            notify_on: self.notify_on.or(lower.notify_on),
            notify_env: self.notify_env.or(lower.notify_env),
            test_db_vars: self.test_db_vars.or(lower.test_db_vars),
            local_files: self.local_files.or(lower.local_files),
            mcp_servers: self.mcp_servers.or(lower.mcp_servers),
            test_db_drop_cmd: self.test_db_drop_cmd.or(lower.test_db_drop_cmd),
            keep_test_db: self.keep_test_db.or(lower.keep_test_db),
            db_name_template: self.db_name_template.or(lower.db_name_template),
            model: self.model.or(lower.model),
        }
    }
}

/// The settings for the repository at `repo`; `env` looks a variable up.
pub fn load_run_settings(repo: &Path, env: &dyn Fn(&str) -> Option<String>) -> Result<RunSettings, SettingsError> {
    let shared = read_layer(&repo.join(SHARED_FILE), SHARED_FILE)?;
    let private = match git_dir(repo) {
        Some(dir) => read_layer(&dir.join("orq/run.toml"), PRIVATE_LABEL)?,
        None => Layer::default(),
    };
    let merged = env_layer(env)?.over(private).over(shared);
    Ok(RunSettings {
        forge: merged.forge,
        base_branch: merged.base_branch,
        gate: merged.gate,
        setup: merged.setup.unwrap_or_default(),
        ci_timeout: merged.ci_timeout,
        mr_assignee: merged.mr_assignee,
        mr_reviewer: merged.mr_reviewer,
        notify_cmd: merged.notify_cmd,
        notify_on: merged.notify_on,
        notify_env: merged.notify_env,
        test_db_vars: merged.test_db_vars,
        local_files: merged.local_files,
        mcp_servers: merged.mcp_servers,
        test_db_drop_cmd: merged.test_db_drop_cmd,
        keep_test_db: merged.keep_test_db,
        db_name_template: merged.db_name_template,
        model: merged.model,
    })
}

fn git_dir(repo: &Path) -> Option<std::path::PathBuf> {
    let got = git_raw(repo, &["rev-parse", "--git-dir"]).ok()?;
    let line = got.stdout.lines().next()?.trim();
    (got.success && !line.is_empty()).then(|| repo.join(line))
}

fn invalid(source: &str, reason: impl Into<String>) -> SettingsError {
    SettingsError::Invalid { source: source.to_string(), reason: reason.into() }
}

fn read_layer(path: &Path, label: &str) -> Result<Layer, SettingsError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Layer::default()),
        Err(err) => return Err(SettingsError::Unreadable { path: label.to_string(), reason: err.to_string() }),
    };
    let doc: toml_edit::DocumentMut = text.parse().map_err(|err: toml_edit::TomlError| invalid(label, err.to_string()))?;
    let mut layer = Layer::default();
    for (key, item) in doc.iter() {
        let value = item.as_value().ok_or_else(|| invalid(label, format!("{key} must be a plain value")))?;
        let bad = |reason: &str| invalid(label, format!("{key} {reason}"));
        match key {
            "forge" => layer.forge = Some(forge(value.as_str().ok_or_else(|| bad("must be a string"))?).map_err(|r| bad(&r))?),
            "base_branch" => {
                layer.base_branch = Some(branch(value.as_str().ok_or_else(|| bad("must be a string"))?).map_err(|r| bad(&r))?)
            }
            "gate" => layer.gate = Some(argv(&json_of(value)).map_err(|r| bad(&r))?),
            "setup" => layer.setup = Some(steps(&json_of(value)).map_err(|r| bad(&r))?),
            "ci_timeout_secs" => {
                layer.ci_timeout = Some(timeout(value.as_integer().ok_or_else(|| bad("must be a whole number"))?).map_err(|r| bad(&r))?)
            }
            "mr_assignee" => layer.mr_assignee = Some(person(value.as_str().ok_or_else(|| bad("must be a string"))?, true).map_err(|r| bad(&r))?),
            "mr_reviewer" => layer.mr_reviewer = Some(person(value.as_str().ok_or_else(|| bad("must be a string"))?, false).map_err(|r| bad(&r))?),
            "notify_cmd" => layer.notify_cmd = Some(argv(&json_of(value)).map_err(|r| bad(&r))?),
            "notify_on" => layer.notify_on = Some(notify_on(value.as_str().ok_or_else(|| bad("must be a string"))?).map_err(|r| bad(&r))?),
            "notify_env" => layer.notify_env = Some(variable_names(&json_of(value)).map_err(|r| bad(&r))?),
            "test_db_vars" => layer.test_db_vars = Some(variable_names(&json_of(value)).map_err(|r| bad(&r))?),
            "local_files" => layer.local_files = Some(argv(&json_of(value)).map_err(|r| bad(&r))?),
            "mcp_servers" => layer.mcp_servers = Some(argv(&json_of(value)).map_err(|r| bad(&r))?),
            "test_db_drop_cmd" => layer.test_db_drop_cmd = Some(argv(&json_of(value)).map_err(|r| bad(&r))?),
            "db_name_template" => layer.db_name_template = Some(value.as_str().ok_or_else(|| bad("must be a string"))?.to_string()),
            "model" => layer.model = Some(model(value.as_str().ok_or_else(|| bad("must be a string"))?).map_err(|r| bad(&r))?),
            "keep_test_db" => layer.keep_test_db = Some(value.as_bool().ok_or_else(|| bad("must be true or false"))?),
            other => return Err(invalid(label, format!("unknown setting {other}"))),
        }
    }
    Ok(layer)
}

fn env_layer(env: &dyn Fn(&str) -> Option<String>) -> Result<Layer, SettingsError> {
    let get = |name: &str| env(name).filter(|v| !v.trim().is_empty());
    let json = |name: &str, text: &str| -> Result<serde_json::Value, SettingsError> {
        serde_json::from_str(text).map_err(|_| invalid(name, "must be a JSON value"))
    };
    let mut layer = Layer::default();
    if let Some(v) = get("ORQ_FORGE") {
        layer.forge = Some(forge(v.trim()).map_err(|r| invalid("ORQ_FORGE", r))?);
    }
    if let Some(v) = get("ORQ_BASE_BRANCH") {
        layer.base_branch = Some(branch(v.trim()).map_err(|r| invalid("ORQ_BASE_BRANCH", r))?);
    }
    if let Some(v) = get("ORQ_GATE") {
        layer.gate = Some(argv(&json("ORQ_GATE", &v)?).map_err(|r| invalid("ORQ_GATE", r))?);
    }
    if let Some(v) = get("ORQ_SETUP") {
        layer.setup = Some(steps(&json("ORQ_SETUP", &v)?).map_err(|r| invalid("ORQ_SETUP", r))?);
    }
    if let Some(v) = get("ORQ_CI_TIMEOUT_SECS") {
        let secs: i64 = v.trim().parse().map_err(|_| invalid("ORQ_CI_TIMEOUT_SECS", "must be a whole number"))?;
        layer.ci_timeout = Some(timeout(secs).map_err(|r| invalid("ORQ_CI_TIMEOUT_SECS", r))?);
    }
    if let Some(v) = get("ORQ_NOTIFY_CMD") {
        layer.notify_cmd = Some(argv(&json("ORQ_NOTIFY_CMD", &v)?).map_err(|r| invalid("ORQ_NOTIFY_CMD", r))?);
    }
    if let Some(v) = get("ORQ_NOTIFY_ON") {
        layer.notify_on = Some(notify_on(v.trim()).map_err(|r| invalid("ORQ_NOTIFY_ON", r))?);
    }
    if let Some(v) = get("ORQ_NOTIFY_ENV") {
        layer.notify_env = Some(variable_names(&json("ORQ_NOTIFY_ENV", &v)?).map_err(|r| invalid("ORQ_NOTIFY_ENV", r))?);
    }
    if let Some(v) = get("ORQ_TEST_DB_VARS") {
        layer.test_db_vars = Some(variable_names(&json("ORQ_TEST_DB_VARS", &v)?).map_err(|r| invalid("ORQ_TEST_DB_VARS", r))?);
    }
    if let Some(v) = get("ORQ_LOCAL_FILES") {
        layer.local_files = Some(argv(&json("ORQ_LOCAL_FILES", &v)?).map_err(|r| invalid("ORQ_LOCAL_FILES", r))?);
    }
    if let Some(v) = get("ORQ_MCP_SERVERS") {
        layer.mcp_servers = Some(argv(&json("ORQ_MCP_SERVERS", &v)?).map_err(|r| invalid("ORQ_MCP_SERVERS", r))?);
    }
    if let Some(v) = get("ORQ_TEST_DB_DROP_CMD") {
        layer.test_db_drop_cmd = Some(argv(&json("ORQ_TEST_DB_DROP_CMD", &v)?).map_err(|r| invalid("ORQ_TEST_DB_DROP_CMD", r))?);
    }
    if let Some(v) = get("ORQ_DB_NAME_TEMPLATE") {
        layer.db_name_template = Some(v.trim().to_string());
    }
    if let Some(v) = get("ORQ_MODEL") {
        layer.model = Some(model(v.trim()).map_err(|r| invalid("ORQ_MODEL", r))?);
    }
    if let Some(v) = get("ORQ_KEEP_TEST_DB") {
        layer.keep_test_db = Some(match v.trim() {
            "1" | "true" => true,
            "0" | "false" => false,
            _ => return Err(invalid("ORQ_KEEP_TEST_DB", "must be 1, 0, true or false")),
        });
    }
    for (name, assignee) in [("ORQ_MR_ASSIGNEE", true), ("ORQ_MR_REVIEWER", false)] {
        if let Some(v) = get(name) {
            let who = Some(person(&v, assignee).map_err(|r| invalid(name, r))?);
            if assignee { layer.mr_assignee = who } else { layer.mr_reviewer = who }
        }
    }
    Ok(layer)
}

/// A TOML value as JSON, so one validator serves both file and environment.
fn json_of(value: &toml_edit::Value) -> serde_json::Value {
    match value {
        toml_edit::Value::String(s) => serde_json::Value::String(s.value().clone()),
        toml_edit::Value::Array(items) => serde_json::Value::Array(items.iter().map(json_of).collect()),
        _ => serde_json::Value::Null,
    }
}

fn forge(value: &str) -> Result<String, String> {
    match value {
        "gh" | "glab" => Ok(value.to_string()),
        _ => Err("must be gh or glab".to_string()),
    }
}

fn person(value: &str, assignee: bool) -> Result<String, String> {
    let people = if assignee { ReviewPeople::checked(Some(value), None) } else { ReviewPeople::checked(None, Some(value)) }?;
    people.assignee.or(people.reviewer).ok_or_else(|| "must not be blank".to_string())
}

fn variable_names(value: &serde_json::Value) -> Result<Vec<String>, String> {
    let names: Vec<String> = value
        .as_array()
        .ok_or("must be a list of variable names")?
        .iter()
        .map(|n| n.as_str().map(str::to_string).ok_or("must hold only strings"))
        .collect::<Result<_, _>>()?;
    for name in &names {
        let mut chars = name.chars();
        let plain = chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !plain || name == "NOTIFY_MESSAGE" {
            return Err(format!("{name:?} is not a usable variable name"));
        }
    }
    Ok(names)
}

/// A model name is handed to the agent as one option value: never blank, never an option, one word.
pub fn model(value: &str) -> Result<String, String> {
    // The same rule as the agent adapter's `plain` check, so a name accepted here never fails after the run exists.
    let allowed = |c: char| c.is_ascii_alphanumeric() || "-_./:@+".contains(c);
    if value.is_empty() || value.starts_with('-') || !value.chars().all(allowed) {
        return Err("must be a model name: letters, digits and - _ . / : @ +, not starting with a dash".to_string());
    }
    Ok(value.to_string())
}

fn notify_on(value: &str) -> Result<String, String> {
    match value {
        "all" | "failures" => Ok(value.to_string()),
        _ => Err("must be all or failures".to_string()),
    }
}

fn branch(value: &str) -> Result<String, String> {
    // Git's own rules for a branch name, plus: never an option, never padded.
    let plain = !value.is_empty() && !value.starts_with('-') && value.trim() == value;
    let git_accepts = plain
        && std::process::Command::new("git")
            .args(["check-ref-format", &format!("refs/heads/{value}")])
            .stdin(std::process::Stdio::null())
            .output()
            .is_ok_and(|out| out.status.success());
    if git_accepts {
        Ok(value.to_string())
    } else {
        Err("must be a valid branch name".to_string())
    }
}

fn argv(value: &serde_json::Value) -> Result<Vec<String>, String> {
    let words: Vec<String> = value
        .as_array()
        .ok_or("must be a list of words, not a shell string")?
        .iter()
        .map(|w| w.as_str().map(str::to_string).ok_or("must hold only strings"))
        .collect::<Result<_, _>>()?;
    match words.first() {
        None => Err("must not be empty".to_string()),
        Some(program) if program.trim().is_empty() => Err("must start with a program".to_string()),
        Some(_) => Ok(words),
    }
}

fn steps(value: &serde_json::Value) -> Result<Vec<Vec<String>>, String> {
    value.as_array().ok_or("must be a list of commands")?.iter().map(argv).collect()
}

fn timeout(secs: i64) -> Result<Duration, String> {
    if (1..=MAX_CI_TIMEOUT_SECS).contains(&secs) {
        Ok(Duration::from_secs(secs as u64))
    } else {
        Err(format!("must be between 1 and {MAX_CI_TIMEOUT_SECS}"))
    }
}
