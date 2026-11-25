//! Per-repository run settings: forge, base branch, gate, setup steps and the CI wait
//! timeout. They come from a private file in the git directory (`orq/run.toml`), a checked-in
//! `.orq/run.toml`, and `ORQ_*` environment variables; the environment wins over the private
//! file, which wins over the checked-in one, key by key. Commands are argv lists, never shell
//! text, and every value is validated before a run uses it. The one exception: when none of those
//! names a gate, the `GATE_CMD` of the old script's settings file is run with `bash -c`, as the
//! script ran it. That file is only ever parsed here, never evaluated.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use crate::outcome::git_raw;
use crate::review_request::ReviewPeople;

const SHARED_FILE: &str = ".orq/run.toml";
const PRIVATE_LABEL: &str = "orq/run.toml in the git directory";
/// The old script's settings files, private one first; only `GATE_CMD` is taken from them.
const SCRIPT_PRIVATE_FILE: &str = "orchestrate.env";
const SCRIPT_REPO_FILE: &str = ".orchestrate.env";
/// A CI wait longer than a day is a typo, not a wish.
const MAX_CI_TIMEOUT_SECS: i64 = 86_400;
const MAX_COMPACT_WINDOW: i64 = 10_000_000;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunSettings {
    /// `gh` or `glab`.
    pub forge: Option<String>,
    pub base_branch: Option<String>,
    /// The gate command; `None` leaves the project's own detection in charge.
    pub gate: Option<Vec<String>>,
    /// The gate run after each design group member instead of `gate`; `None` runs `gate` (or the
    /// detected gate, scoped to what the group changed).
    pub member_gate: Option<Vec<String>>,
    /// The old script's settings file that `gate` was taken from, when it was.
    pub gate_from_script: Option<String>,
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
    /// Close the implement session once it has written its exit marker (on when unset).
    pub auto_exit: Option<bool>,
    pub remote_control: Option<bool>,
    pub explain_failures: Option<bool>,
    /// Also send a push notification to the Claude app when a run ends (on when unset; needs Remote Control).
    pub claude_push: Option<bool>,
    /// The shape of a per-worktree database name, with `{name}` and `{branch}`.
    pub db_name_template: Option<String>,
    /// The model for the implement session and the review draft; `None` leaves the chosen profile's own.
    pub model: Option<String>,
    /// The prefix of a new run's branch (`fix--` when unset).
    pub branch_prefix: Option<String>,
    /// The context size, in tokens, at which a run's sessions auto-compact; `None` leaves the harness's own.
    pub compact_window: Option<u32>,
    /// Permission rules a run's session may use without asking, beside orq's own; `None` adds none.
    pub allowed_tools: Option<Vec<String>>,
    /// The implement session may run the full suite itself, for a project whose gate is not one (off when
    /// unset: the session is told the gate runs it).
    pub full_suite_in_session: Option<bool>,
    /// `[rebase] generated`: the files a rebase's conflict session rebuilds instead of merging by hand.
    pub rebase_generated: Option<Vec<String>>,
    /// `[rebase] rebuild`: the command that rebuilds the generated files.
    pub rebase_rebuild: Option<Vec<String>>,
    /// `[design]`: the frontend run's browser and loop settings; `None` when no file has the table.
    pub design: Option<DesignSettings>,
}

/// The `[design]` table, with the defaults filled in for every key no source set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesignSettings {
    /// The argv that starts the project's dev server; `{port}` stands for the run's port. Required
    /// for a frontend run, so `None` is only refused when one starts (see [`DesignSettings::start_argv`]).
    pub start_command: Option<Vec<String>>,
    /// The path polled until it answers 2xx.
    pub ready_path: String,
    pub ready_timeout: Duration,
    /// 0..=3; 3 is also the hard cap.
    pub max_correction_rounds: u8,
    /// An issue still open after this many rounds goes to the verifier.
    pub verify_after_rounds: u8,
    pub mode: DesignMode,
    /// Reference name to viewport: the built-in `desktop`, `tablet` and `mobile`, plus any added.
    pub viewports: BTreeMap<String, Viewport>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesignMode {
    Prepared,
    Infer,
}

/// A viewport size in CSS pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Viewport {
    pub width: u32,
    pub height: u32,
}

impl DesignSettings {
    /// The start command for a run on `port`, every `{port}` replaced; an error naming the key when
    /// the project set none, so a frontend run stops before any agent starts.
    pub fn start_argv(&self, port: u16) -> Result<Vec<String>, String> {
        let command = self.start_command.as_ref().ok_or_else(|| {
            format!("design.start_command is required for a frontend run: set it in the [design] table of {SHARED_FILE}")
        })?;
        let port = port.to_string();
        Ok(command
            .iter()
            .map(|word| word.replace(PORT_PLACEHOLDER, &port))
            .collect())
    }
}

const PORT_PLACEHOLDER: &str = "{port}";
const MAX_DESIGN_ROUNDS: i64 = 3;
const MAX_READY_TIMEOUT_SECS: i64 = 3_600;
const VIEWPORT_SIDES: std::ops::RangeInclusive<u32> = 200..=4000;

/// One source's `[design]` values.
#[derive(Default)]
struct DesignLayer {
    /// A file of this layer has the table; the environment alone never makes one.
    present: bool,
    start_command: Option<Vec<String>>,
    ready_path: Option<String>,
    ready_timeout: Option<Duration>,
    max_correction_rounds: Option<u8>,
    verify_after_rounds: Option<u8>,
    mode: Option<DesignMode>,
    viewports: BTreeMap<String, Viewport>,
}

impl DesignLayer {
    fn over(self, lower: DesignLayer) -> DesignLayer {
        let mut viewports = lower.viewports;
        viewports.extend(self.viewports);
        DesignLayer {
            present: self.present || lower.present,
            start_command: self.start_command.or(lower.start_command),
            ready_path: self.ready_path.or(lower.ready_path),
            ready_timeout: self.ready_timeout.or(lower.ready_timeout),
            max_correction_rounds: self.max_correction_rounds.or(lower.max_correction_rounds),
            verify_after_rounds: self.verify_after_rounds.or(lower.verify_after_rounds),
            mode: self.mode.or(lower.mode),
            viewports,
        }
    }

    fn settings(self) -> Option<DesignSettings> {
        if !self.present {
            return None;
        }
        let mut viewports: BTreeMap<String, Viewport> = [
            ("desktop", 1440, 900),
            ("tablet", 768, 1024),
            ("mobile", 390, 844),
        ]
        .into_iter()
        .map(|(name, width, height)| (name.to_string(), Viewport { width, height }))
        .collect();
        viewports.extend(self.viewports);
        Some(DesignSettings {
            start_command: self.start_command,
            ready_path: self.ready_path.unwrap_or_else(|| "/".to_string()),
            ready_timeout: self.ready_timeout.unwrap_or(Duration::from_secs(60)),
            max_correction_rounds: self.max_correction_rounds.unwrap_or(3),
            verify_after_rounds: self.verify_after_rounds.unwrap_or(2),
            mode: self.mode.unwrap_or(DesignMode::Prepared),
            viewports,
        })
    }
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
    member_gate: Option<Vec<String>>,
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
    auto_exit: Option<bool>,
    remote_control: Option<bool>,
    explain_failures: Option<bool>,
    claude_push: Option<bool>,
    db_name_template: Option<String>,
    model: Option<String>,
    branch_prefix: Option<String>,
    compact_window: Option<u32>,
    allowed_tools: Option<Vec<String>>,
    full_suite_in_session: Option<bool>,
    rebase_generated: Option<Vec<String>>,
    rebase_rebuild: Option<Vec<String>>,
    design: DesignLayer,
}

impl Layer {
    /// `self` wins; whatever it leaves unset comes from `lower`.
    fn over(self, lower: Layer) -> Layer {
        Layer {
            forge: self.forge.or(lower.forge),
            base_branch: self.base_branch.or(lower.base_branch),
            gate: self.gate.or(lower.gate),
            member_gate: self.member_gate.or(lower.member_gate),
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
            auto_exit: self.auto_exit.or(lower.auto_exit),
            remote_control: self.remote_control.or(lower.remote_control),
            explain_failures: self.explain_failures.or(lower.explain_failures),
            claude_push: self.claude_push.or(lower.claude_push),
            db_name_template: self.db_name_template.or(lower.db_name_template),
            model: self.model.or(lower.model),
            branch_prefix: self.branch_prefix.or(lower.branch_prefix),
            compact_window: self.compact_window.or(lower.compact_window),
            allowed_tools: self.allowed_tools.or(lower.allowed_tools),
            full_suite_in_session: self.full_suite_in_session.or(lower.full_suite_in_session),
            rebase_generated: self.rebase_generated.or(lower.rebase_generated),
            rebase_rebuild: self.rebase_rebuild.or(lower.rebase_rebuild),
            design: self.design.over(lower.design),
        }
    }
}

/// The settings for the repository at `repo`; `env` looks a variable up.
pub fn load_run_settings(
    repo: &Path,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<RunSettings, SettingsError> {
    let shared = read_layer(&repo.join(SHARED_FILE), SHARED_FILE)?;
    let private = match git_dir(repo) {
        Some(dir) => read_layer(&dir.join("orq/run.toml"), PRIVATE_LABEL)?,
        None => Layer::default(),
    };
    let mut merged = env_layer(env)?.over(private).over(shared);
    // The design variables are only read, and so only checked, for a project that has the table.
    if merged.design.present {
        let get = |name: &str| env(name).filter(|v| !v.trim().is_empty());
        merged.design = design_env_layer(&get)?.over(merged.design);
    }
    let script_gate = match merged.gate {
        Some(_) => None,
        None => script_gate(repo)?,
    };
    let (script_command, gate_from_script) = script_gate.unzip();
    Ok(RunSettings {
        forge: merged.forge,
        base_branch: merged.base_branch,
        gate: merged.gate.or(script_command),
        member_gate: merged.member_gate,
        gate_from_script,
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
        auto_exit: merged.auto_exit,
        remote_control: merged.remote_control,
        explain_failures: merged.explain_failures,
        claude_push: merged.claude_push,
        db_name_template: merged.db_name_template,
        model: merged.model,
        branch_prefix: merged.branch_prefix,
        compact_window: merged.compact_window,
        allowed_tools: merged.allowed_tools,
        full_suite_in_session: merged.full_suite_in_session,
        rebase_generated: merged.rebase_generated,
        rebase_rebuild: merged.rebase_rebuild,
        design: merged.design.settings(),
    })
}

/// Saves `servers` as the `mcp_servers` setting in the private file of the repository at `repo`,
/// keeping its other keys and comments. A file that is not valid TOML is left untouched.
pub fn save_mcp_servers_setting(repo: &Path, servers: &[String]) -> Result<(), String> {
    let dir = git_dir(repo)
        .ok_or_else(|| format!("{} is not in a git repository", repo.display()))?
        .join("orq");
    let path = dir.join("run.toml");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(format!("{PRIVATE_LABEL}: {:?}", err.kind())),
    };
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|err| {
        format!("{PRIVATE_LABEL} is not valid TOML and was left untouched: {err}")
    })?;
    let mut array = toml_edit::Array::new();
    for server in servers {
        array.push(server.as_str());
    }
    doc["mcp_servers"] = toml_edit::value(array);
    std::fs::create_dir_all(&dir).map_err(|err| format!("{PRIVATE_LABEL}: {:?}", err.kind()))?;
    // A new file is private; a file that exists keeps the permissions it has.
    let mode = std::fs::metadata(&path)
        .map(|meta| std::os::unix::fs::PermissionsExt::mode(&meta.permissions()) & 0o777)
        .unwrap_or(0o600);
    let fail = |err: std::io::Error| format!("{PRIVATE_LABEL}: {:?}", err.kind());
    let (temp, mut file) = create_temp(&dir, mode).map_err(fail)?;
    let written = std::io::Write::write_all(&mut file, doc.to_string().as_bytes())
        .and_then(|()| file.sync_all())
        .and_then(|()| std::fs::rename(&temp, &path));
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written.map_err(fail)
}

/// A temporary file created exclusively beside the settings file, so a planted name or link is never
/// written through; its permissions are set at creation.
fn create_temp(dir: &Path, mode: u32) -> std::io::Result<(std::path::PathBuf, std::fs::File)> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut attempt = 0u32;
    loop {
        let temp = dir.join(format!("run.toml.{}.{attempt}.tmp", std::process::id()));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&temp)
        {
            Ok(file) => {
                // The mode given at creation is filtered by the umask, so it is set again outright.
                if let Err(err) =
                    file.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(mode))
                {
                    let _ = std::fs::remove_file(&temp);
                    return Err(err);
                }
                return Ok((temp, file));
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists && attempt < 100 => {
                attempt += 1
            }
            Err(err) => return Err(err),
        }
    }
}

fn git_dir(repo: &Path) -> Option<std::path::PathBuf> {
    let got = git_raw(repo, &["rev-parse", "--git-dir"]).ok()?;
    let line = got.stdout.lines().next()?.trim();
    (got.success && !line.is_empty()).then(|| repo.join(line))
}

/// The gate the old script would have run: its `GATE_CMD`, and the file that set it. The first
/// file to set the variable wins, even to nothing, which leaves the project's detection in charge.
fn script_gate(repo: &Path) -> Result<Option<(Vec<String>, String)>, SettingsError> {
    let private = git_dir(repo).map(|dir| dir.join(SCRIPT_PRIVATE_FILE));
    for path in private.into_iter().chain([repo.join(SCRIPT_REPO_FILE)]) {
        let Some(text) = read_script_file(&path)? else {
            continue;
        };
        if let Some(command) = text.lines().find_map(script_gate_line) {
            return Ok((!command.is_empty()).then(|| {
                let argv = vec!["bash".to_string(), "-c".to_string(), command.to_string()];
                (argv, path.display().to_string())
            }));
        }
    }
    Ok(None)
}

/// The text of a script settings file. Like the script, only a regular file is read, so a named
/// pipe cannot block the run; one that is there but unreadable is an error, not a different gate.
fn read_script_file(path: &Path) -> Result<Option<String>, SettingsError> {
    let unreadable = |err: std::io::Error| SettingsError::Unreadable {
        path: path.display().to_string(),
        reason: err.to_string(),
    };
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() => {}
        Ok(_) => return Ok(None),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(unreadable(err)),
    }
    let bytes = std::fs::read(path).map_err(unreadable)?;
    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
}

/// The value of a `[export ]GATE_CMD = value` line, with one pair of surrounding quotes removed.
fn script_gate_line(line: &str) -> Option<&str> {
    let line = line.trim();
    let line = line
        .strip_prefix("export")
        .filter(|rest| rest.starts_with(char::is_whitespace))
        .map_or(line, str::trim_start);
    let value = line
        .strip_prefix("GATE_CMD")?
        .trim_start()
        .strip_prefix('=')?
        .trim_start();
    let quoted = value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')));
    Some(if quoted {
        &value[1..value.len() - 1]
    } else {
        value
    })
}

fn invalid(source: &str, reason: impl Into<String>) -> SettingsError {
    SettingsError::Invalid {
        source: source.to_string(),
        reason: reason.into(),
    }
}

fn read_layer(path: &Path, label: &str) -> Result<Layer, SettingsError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Layer::default()),
        Err(err) => {
            return Err(SettingsError::Unreadable {
                path: label.to_string(),
                reason: err.to_string(),
            })
        }
    };
    let doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|err: toml_edit::TomlError| invalid(label, err.to_string()))?;
    let mut layer = Layer::default();
    for (key, item) in doc.iter() {
        if key == "rebase" {
            read_rebase_table(item, label, &mut layer)?;
            continue;
        }
        if key == "design" {
            read_design_table(item, label, &mut layer.design)?;
            continue;
        }
        if key == "release" || key == "after_merge" {
            // Read by the project's merge steps; checked here with their parsers so every reader of the
            // file agrees, and a bad value names the same key everywhere.
            if item.as_value().is_some_and(|value| !value.is_inline_table()) {
                let reason = match key {
                    "after_merge" => "after_merge must be a table: a plain after_merge value cannot sit beside the [after_merge] table of commands".to_string(),
                    _ => "release must be a table".to_string(),
                };
                return Err(invalid(label, reason));
            }
            let checked = if key == "release" {
                orq_core::project_settings::ReleaseSettings::parse(&text).map(|_| ())
            } else {
                orq_core::project_settings::AfterMergeCommands::parse(&text).map(|_| ())
            };
            checked.map_err(|reason| invalid(label, reason))?;
            continue;
        }
        let value = item
            .as_value()
            .ok_or_else(|| invalid(label, format!("{key} must be a plain value")))?;
        let bad = |reason: &str| invalid(label, format!("{key} {reason}"));
        match key {
            "forge" => {
                layer.forge = Some(
                    forge(value.as_str().ok_or_else(|| bad("must be a string"))?)
                        .map_err(|r| bad(&r))?,
                )
            }
            "base_branch" => {
                layer.base_branch = Some(
                    branch(value.as_str().ok_or_else(|| bad("must be a string"))?)
                        .map_err(|r| bad(&r))?,
                )
            }
            "gate" => layer.gate = Some(argv(&json_of(value)).map_err(|r| bad(&r))?),
            "member_gate" => layer.member_gate = Some(argv(&json_of(value)).map_err(|r| bad(&r))?),
            "setup" => layer.setup = Some(steps(&json_of(value)).map_err(|r| bad(&r))?),
            "ci_timeout_secs" => {
                layer.ci_timeout = Some(
                    timeout(
                        value
                            .as_integer()
                            .ok_or_else(|| bad("must be a whole number"))?,
                    )
                    .map_err(|r| bad(&r))?,
                )
            }
            "mr_assignee" => {
                layer.mr_assignee = Some(
                    person(value.as_str().ok_or_else(|| bad("must be a string"))?, true)
                        .map_err(|r| bad(&r))?,
                )
            }
            "mr_reviewer" => {
                layer.mr_reviewer = Some(
                    person(
                        value.as_str().ok_or_else(|| bad("must be a string"))?,
                        false,
                    )
                    .map_err(|r| bad(&r))?,
                )
            }
            "notify_cmd" => layer.notify_cmd = Some(argv(&json_of(value)).map_err(|r| bad(&r))?),
            "notify_on" => {
                layer.notify_on = Some(
                    notify_on(value.as_str().ok_or_else(|| bad("must be a string"))?)
                        .map_err(|r| bad(&r))?,
                )
            }
            "notify_env" => {
                layer.notify_env = Some(variable_names(&json_of(value)).map_err(|r| bad(&r))?)
            }
            "test_db_vars" => {
                layer.test_db_vars = Some(variable_names(&json_of(value)).map_err(|r| bad(&r))?)
            }
            "local_files" => layer.local_files = Some(argv(&json_of(value)).map_err(|r| bad(&r))?),
            "mcp_servers" => layer.mcp_servers = Some(argv(&json_of(value)).map_err(|r| bad(&r))?),
            "test_db_drop_cmd" => {
                layer.test_db_drop_cmd = Some(argv(&json_of(value)).map_err(|r| bad(&r))?)
            }
            "db_name_template" => {
                layer.db_name_template = Some(
                    value
                        .as_str()
                        .ok_or_else(|| bad("must be a string"))?
                        .to_string(),
                )
            }
            "allowed_tools" => {
                layer.allowed_tools = Some(tool_rules(&json_of(value)).map_err(|r| bad(&r))?)
            }
            "compact_window" => {
                layer.compact_window = Some(
                    window(
                        value
                            .as_integer()
                            .ok_or_else(|| bad("must be a whole number"))?,
                    )
                    .map_err(|r| bad(&r))?,
                )
            }
            "branch_prefix" => {
                layer.branch_prefix = Some(
                    branch_prefix(value.as_str().ok_or_else(|| bad("must be a string"))?)
                        .map_err(|r| bad(&r))?,
                )
            }
            "model" => {
                layer.model = Some(
                    model(value.as_str().ok_or_else(|| bad("must be a string"))?)
                        .map_err(|r| bad(&r))?,
                )
            }
            "keep_test_db" => {
                layer.keep_test_db = Some(
                    value
                        .as_bool()
                        .ok_or_else(|| bad("must be true or false"))?,
                )
            }
            "auto_exit" => {
                layer.auto_exit = Some(
                    value
                        .as_bool()
                        .ok_or_else(|| bad("must be true or false"))?,
                )
            }
            "remote_control" => {
                layer.remote_control = Some(
                    value
                        .as_bool()
                        .ok_or_else(|| bad("must be true or false"))?,
                )
            }
            "explain_failures" => {
                layer.explain_failures = Some(
                    value
                        .as_bool()
                        .ok_or_else(|| bad("must be true or false"))?,
                )
            }
            "claude_push" => {
                layer.claude_push = Some(
                    value
                        .as_bool()
                        .ok_or_else(|| bad("must be true or false"))?,
                )
            }
            "full_suite_in_session" => {
                layer.full_suite_in_session = Some(
                    value
                        .as_bool()
                        .ok_or_else(|| bad("must be true or false"))?,
                )
            }
            other => return Err(invalid(label, format!("unknown setting {other}"))),
        }
    }
    Ok(layer)
}

/// The `[rebase]` table: `generated`, a list of paths, and `rebuild`, a command.
fn read_rebase_table(
    item: &toml_edit::Item,
    label: &str,
    layer: &mut Layer,
) -> Result<(), SettingsError> {
    let table = item
        .as_table_like()
        .ok_or_else(|| invalid(label, "rebase must be a table".to_string()))?;
    for (key, item) in table.iter() {
        let bad = |reason: &str| invalid(label, format!("rebase.{key} {reason}"));
        let value = item
            .as_value()
            .ok_or_else(|| bad("must be a plain value"))?;
        match key {
            "generated" => {
                layer.rebase_generated = Some(paths(&json_of(value)).map_err(|r| bad(&r))?)
            }
            "rebuild" => layer.rebase_rebuild = Some(argv(&json_of(value)).map_err(|r| bad(&r))?),
            _ => return Err(bad("is not a setting")),
        }
    }
    Ok(())
}

/// The `[design]` table and its `[design.viewports]` subtable. Every key is checked by the same
/// validator the environment's variable goes through.
fn read_design_table(
    item: &toml_edit::Item,
    label: &str,
    design: &mut DesignLayer,
) -> Result<(), SettingsError> {
    let table = item
        .as_table_like()
        .ok_or_else(|| invalid(label, "design must be a table".to_string()))?;
    design.present = true;
    for (key, item) in table.iter() {
        let bad = |reason: &str| invalid(label, format!("design.{key} {reason}"));
        if key == "viewports" {
            let viewports = item
                .as_table_like()
                .ok_or_else(|| bad("must be a table of name = \"<width>x<height>\""))?;
            for (name, size) in viewports.iter() {
                viewport_name(name).map_err(|r| bad(&r))?;
                let bad =
                    |reason: &str| invalid(label, format!("design.viewports.{name} {reason}"));
                let size = size
                    .as_value()
                    .and_then(toml_edit::Value::as_str)
                    .ok_or_else(|| bad("must be a string such as \"1440x900\""))?;
                design
                    .viewports
                    .insert(name.to_string(), viewport(size).map_err(|r| bad(&r))?);
            }
            continue;
        }
        let value = item
            .as_value()
            .ok_or_else(|| bad("must be a plain value"))?;
        let string = || value.as_str().ok_or_else(|| bad("must be a string"));
        let number = || {
            value
                .as_integer()
                .ok_or_else(|| bad("must be a whole number"))
        };
        match key {
            "start_command" => {
                design.start_command = Some(start_command(&json_of(value)).map_err(|r| bad(&r))?)
            }
            "ready_path" => design.ready_path = Some(ready_path(string()?).map_err(|r| bad(&r))?),
            "ready_timeout_secs" => {
                design.ready_timeout = Some(ready_timeout(number()?).map_err(|r| bad(&r))?)
            }
            "max_correction_rounds" => {
                design.max_correction_rounds = Some(rounds(number()?).map_err(|r| bad(&r))?)
            }
            "verify_after_rounds" => {
                design.verify_after_rounds = Some(rounds(number()?).map_err(|r| bad(&r))?)
            }
            "mode" => design.mode = Some(design_mode(string()?).map_err(|r| bad(&r))?),
            _ => return Err(bad("is not a setting")),
        }
    }
    Ok(())
}

/// The `ORQ_DESIGN_*` variables. They only override: they are read only for a project whose files
/// have a `[design]` table, so a project without one is never stopped by them.
fn design_env_layer(get: &dyn Fn(&str) -> Option<String>) -> Result<DesignLayer, SettingsError> {
    let mut design = DesignLayer::default();
    let number = |name: &str, text: &str| -> Result<i64, SettingsError> {
        text.trim()
            .parse()
            .map_err(|_| invalid(name, "must be a whole number"))
    };
    if let Some(v) = get("ORQ_DESIGN_START_COMMAND") {
        let name = "ORQ_DESIGN_START_COMMAND";
        let value = serde_json::from_str(&v).map_err(|_| invalid(name, "must be a JSON value"))?;
        design.start_command = Some(start_command(&value).map_err(|r| invalid(name, r))?);
    }
    if let Some(v) = get("ORQ_DESIGN_READY_PATH") {
        design.ready_path =
            Some(ready_path(v.trim()).map_err(|r| invalid("ORQ_DESIGN_READY_PATH", r))?);
    }
    if let Some(v) = get("ORQ_DESIGN_READY_TIMEOUT_SECS") {
        let name = "ORQ_DESIGN_READY_TIMEOUT_SECS";
        design.ready_timeout =
            Some(ready_timeout(number(name, &v)?).map_err(|r| invalid(name, r))?);
    }
    if let Some(v) = get("ORQ_DESIGN_MAX_CORRECTION_ROUNDS") {
        let name = "ORQ_DESIGN_MAX_CORRECTION_ROUNDS";
        design.max_correction_rounds =
            Some(rounds(number(name, &v)?).map_err(|r| invalid(name, r))?);
    }
    if let Some(v) = get("ORQ_DESIGN_VERIFY_AFTER_ROUNDS") {
        let name = "ORQ_DESIGN_VERIFY_AFTER_ROUNDS";
        design.verify_after_rounds = Some(rounds(number(name, &v)?).map_err(|r| invalid(name, r))?);
    }
    if let Some(v) = get("ORQ_DESIGN_MODE") {
        design.mode = Some(design_mode(v.trim()).map_err(|r| invalid("ORQ_DESIGN_MODE", r))?);
    }
    if let Some(v) = get("ORQ_DESIGN_VIEWPORTS") {
        let name = "ORQ_DESIGN_VIEWPORTS";
        let value: serde_json::Value =
            serde_json::from_str(&v).map_err(|_| invalid(name, "must be a JSON value"))?;
        let object = value
            .as_object()
            .ok_or_else(|| invalid(name, "must be a JSON object of name: \"<width>x<height>\""))?;
        for (viewport_key, size) in object {
            viewport_name(viewport_key).map_err(|r| invalid(name, r))?;
            let size = size.as_str().ok_or_else(|| {
                invalid(
                    name,
                    format!("{viewport_key} must be a string such as \"1440x900\""),
                )
            })?;
            let size = viewport(size).map_err(|r| invalid(name, format!("{viewport_key} {r}")))?;
            design.viewports.insert(viewport_key.clone(), size);
        }
    }
    Ok(design)
}

/// An argv like the gate's, whose only placeholder is `{port}`.
fn start_command(value: &serde_json::Value) -> Result<Vec<String>, String> {
    let words = argv(value)?;
    for word in &words {
        let mut rest = word.as_str();
        while let Some(open) = rest.find('{') {
            let Some(close) = rest[open..].find('}') else {
                break;
            };
            let token = &rest[open..=open + close];
            if token != PORT_PLACEHOLDER {
                return Err(format!(
                    "uses the placeholder {token}: {PORT_PLACEHOLDER} is the only one"
                ));
            }
            rest = &rest[open + close + 1..];
        }
    }
    Ok(words)
}

/// A path on the dev server, such as `/` or `/health?ready=1`: never a full or scheme-relative
/// address, never blank, no spaces or control characters.
fn ready_path(value: &str) -> Result<String, String> {
    let plain = value.starts_with('/')
        && !value.starts_with("//")
        && value.chars().all(|c| c.is_ascii_graphic() && c != '\\');
    if plain {
        Ok(value.to_string())
    } else {
        Err("must be a path starting with /, such as /health".to_string())
    }
}

fn ready_timeout(secs: i64) -> Result<Duration, String> {
    if (1..=MAX_READY_TIMEOUT_SECS).contains(&secs) {
        Ok(Duration::from_secs(secs as u64))
    } else {
        Err(format!("must be between 1 and {MAX_READY_TIMEOUT_SECS}"))
    }
}

fn rounds(count: i64) -> Result<u8, String> {
    if (0..=MAX_DESIGN_ROUNDS).contains(&count) {
        Ok(count as u8)
    } else {
        Err(format!("must be between 0 and {MAX_DESIGN_ROUNDS}"))
    }
}

fn design_mode(value: &str) -> Result<DesignMode, String> {
    match value {
        "prepared" => Ok(DesignMode::Prepared),
        "infer" => Ok(DesignMode::Infer),
        _ => Err("must be prepared or infer".to_string()),
    }
}

/// A reference name: letters, digits, `-` and `_`.
fn viewport_name(name: &str) -> Result<(), String> {
    let plain = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if plain {
        Ok(())
    } else {
        Err(format!(
            "{name:?} is not a usable viewport name (letters, digits, - and _)"
        ))
    }
}

/// `<width>x<height>` in CSS pixels, each side within the bounds.
fn viewport(size: &str) -> Result<Viewport, String> {
    let shape = || {
        format!(
            "must be <width>x<height> in CSS pixels, each between {} and {}",
            VIEWPORT_SIDES.start(),
            VIEWPORT_SIDES.end()
        )
    };
    let (width, height) = size.split_once('x').ok_or_else(shape)?;
    let side = |text: &str| {
        text.parse::<u32>()
            .ok()
            .filter(|n| text.chars().all(|c| c.is_ascii_digit()) && VIEWPORT_SIDES.contains(n))
    };
    match (side(width), side(height)) {
        (Some(width), Some(height)) => Ok(Viewport { width, height }),
        _ => Err(shape()),
    }
}

/// A list, possibly empty, of non-empty one-line paths.
fn paths(value: &serde_json::Value) -> Result<Vec<String>, String> {
    value
        .as_array()
        .ok_or("must be a list of paths")?
        .iter()
        .map(|path| match path.as_str() {
            Some(path) if path.trim().is_empty() => Err("must not hold an empty path".to_string()),
            Some(path) if path.chars().any(char::is_control) => {
                Err("must hold one-line paths".to_string())
            }
            Some(path) => Ok(path.to_string()),
            None => Err("must hold only strings".to_string()),
        })
        .collect()
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
    if let Some(v) = get("ORQ_MEMBER_GATE") {
        layer.member_gate = Some(argv(&json("ORQ_MEMBER_GATE", &v)?).map_err(|r| invalid("ORQ_MEMBER_GATE", r))?);
    }
    if let Some(v) = get("ORQ_SETUP") {
        layer.setup = Some(steps(&json("ORQ_SETUP", &v)?).map_err(|r| invalid("ORQ_SETUP", r))?);
    }
    if let Some(v) = get("ORQ_CI_TIMEOUT_SECS") {
        let secs: i64 = v
            .trim()
            .parse()
            .map_err(|_| invalid("ORQ_CI_TIMEOUT_SECS", "must be a whole number"))?;
        layer.ci_timeout = Some(timeout(secs).map_err(|r| invalid("ORQ_CI_TIMEOUT_SECS", r))?);
    }
    if let Some(v) = get("ORQ_NOTIFY_CMD") {
        layer.notify_cmd =
            Some(argv(&json("ORQ_NOTIFY_CMD", &v)?).map_err(|r| invalid("ORQ_NOTIFY_CMD", r))?);
    }
    if let Some(v) = get("ORQ_NOTIFY_ON") {
        layer.notify_on = Some(notify_on(v.trim()).map_err(|r| invalid("ORQ_NOTIFY_ON", r))?);
    }
    if let Some(v) = get("ORQ_NOTIFY_ENV") {
        layer.notify_env = Some(
            variable_names(&json("ORQ_NOTIFY_ENV", &v)?)
                .map_err(|r| invalid("ORQ_NOTIFY_ENV", r))?,
        );
    }
    if let Some(v) = get("ORQ_TEST_DB_VARS") {
        layer.test_db_vars = Some(
            variable_names(&json("ORQ_TEST_DB_VARS", &v)?)
                .map_err(|r| invalid("ORQ_TEST_DB_VARS", r))?,
        );
    }
    if let Some(v) = get("ORQ_LOCAL_FILES") {
        layer.local_files =
            Some(argv(&json("ORQ_LOCAL_FILES", &v)?).map_err(|r| invalid("ORQ_LOCAL_FILES", r))?);
    }
    if let Some(v) = get("ORQ_MCP_SERVERS") {
        layer.mcp_servers =
            Some(argv(&json("ORQ_MCP_SERVERS", &v)?).map_err(|r| invalid("ORQ_MCP_SERVERS", r))?);
    }
    if let Some(v) = get("ORQ_TEST_DB_DROP_CMD") {
        layer.test_db_drop_cmd = Some(
            argv(&json("ORQ_TEST_DB_DROP_CMD", &v)?)
                .map_err(|r| invalid("ORQ_TEST_DB_DROP_CMD", r))?,
        );
    }
    if let Some(v) = get("ORQ_DB_NAME_TEMPLATE") {
        layer.db_name_template = Some(v.trim().to_string());
    }
    if let Some(v) = get("ORQ_ALLOWED_TOOLS") {
        layer.allowed_tools = Some(
            tool_rules(&json("ORQ_ALLOWED_TOOLS", &v)?)
                .map_err(|r| invalid("ORQ_ALLOWED_TOOLS", r))?,
        );
    }
    if let Some(v) = get("ORQ_COMPACT_WINDOW") {
        let tokens: i64 = v
            .trim()
            .parse()
            .map_err(|_| invalid("ORQ_COMPACT_WINDOW", "must be a whole number"))?;
        layer.compact_window = Some(window(tokens).map_err(|r| invalid("ORQ_COMPACT_WINDOW", r))?);
    }
    if let Some(v) = get("ORQ_BRANCH_PREFIX") {
        layer.branch_prefix =
            Some(branch_prefix(v.trim()).map_err(|r| invalid("ORQ_BRANCH_PREFIX", r))?);
    }
    if let Some(v) = get("ORQ_MODEL") {
        layer.model = Some(model(v.trim()).map_err(|r| invalid("ORQ_MODEL", r))?);
    }
    if let Some(v) = get("ORQ_AUTO_EXIT") {
        layer.auto_exit = Some(match v.trim() {
            "1" | "true" => true,
            "0" | "false" => false,
            _ => return Err(invalid("ORQ_AUTO_EXIT", "must be 1, 0, true or false")),
        });
    }
    if let Some(v) = get("ORQ_REMOTE_CONTROL") {
        layer.remote_control = Some(match v.trim() {
            "1" | "true" => true,
            "0" | "false" => false,
            _ => return Err(invalid("ORQ_REMOTE_CONTROL", "must be 1, 0, true or false")),
        });
    }
    if let Some(v) = get("ORQ_EXPLAIN_FAILURES") {
        layer.explain_failures = Some(match v.trim() {
            "1" | "true" => true,
            "0" | "false" => false,
            _ => {
                return Err(invalid(
                    "ORQ_EXPLAIN_FAILURES",
                    "must be 1, 0, true or false",
                ))
            }
        });
    }
    if let Some(v) = get("ORQ_CLAUDE_PUSH") {
        layer.claude_push = Some(match v.trim() {
            "1" | "true" => true,
            "0" | "false" => false,
            _ => return Err(invalid("ORQ_CLAUDE_PUSH", "must be 1, 0, true or false")),
        });
    }
    if let Some(v) = get("ORQ_FULL_SUITE_IN_SESSION") {
        layer.full_suite_in_session = Some(match v.trim() {
            "1" | "true" => true,
            "0" | "false" => false,
            _ => {
                return Err(invalid(
                    "ORQ_FULL_SUITE_IN_SESSION",
                    "must be 1, 0, true or false",
                ))
            }
        });
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
            if assignee {
                layer.mr_assignee = who
            } else {
                layer.mr_reviewer = who
            }
        }
    }
    Ok(layer)
}

/// A TOML value as JSON, so one validator serves both file and environment.
fn json_of(value: &toml_edit::Value) -> serde_json::Value {
    match value {
        toml_edit::Value::String(s) => serde_json::Value::String(s.value().clone()),
        toml_edit::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(json_of).collect())
        }
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
    let people = if assignee {
        ReviewPeople::checked(Some(value), None)
    } else {
        ReviewPeople::checked(None, Some(value))
    }?;
    people
        .assignee
        .or(people.reviewer)
        .ok_or_else(|| "must not be blank".to_string())
}

fn variable_names(value: &serde_json::Value) -> Result<Vec<String>, String> {
    let names: Vec<String> = value
        .as_array()
        .ok_or("must be a list of variable names")?
        .iter()
        .map(|n| {
            n.as_str()
                .map(str::to_string)
                .ok_or("must hold only strings")
        })
        .collect::<Result<_, _>>()?;
    for name in &names {
        let mut chars = name.chars();
        let plain = chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
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
        return Err(
            "must be a model name: letters, digits and - _ . / : @ +, not starting with a dash"
                .to_string(),
        );
    }
    Ok(value.to_string())
}

/// A branch prefix is checked by the same rule that names the branch, so a bad one stops the run before it starts.
fn branch_prefix(value: &str) -> Result<String, String> {
    if orq_core::naming::is_valid_prefix(value) {
        Ok(value.to_string())
    } else {
        Err(
            "must be a branch prefix such as fix-- or feat/ (letters, digits, . _ - and /)"
                .to_string(),
        )
    }
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
        .map(|w| {
            w.as_str()
                .map(str::to_string)
                .ok_or("must hold only strings")
        })
        .collect::<Result<_, _>>()?;
    match words.first() {
        None => Err("must not be empty".to_string()),
        Some(program) if program.trim().is_empty() => Err("must start with a program".to_string()),
        Some(_) => Ok(words),
    }
}

/// Permission rules, such as `Bash(make lint:*)`: a list, possibly empty, of one-line strings.
fn tool_rules(value: &serde_json::Value) -> Result<Vec<String>, String> {
    value
        .as_array()
        .ok_or("must be a list of permission rules")?
        .iter()
        .map(|rule| match rule.as_str() {
            Some(rule) if rule.trim().is_empty() => Err("must not hold an empty rule".to_string()),
            Some(rule) if rule.chars().any(char::is_control) => {
                Err("must hold one-line rules".to_string())
            }
            Some(rule) => Ok(rule.to_string()),
            None => Err("must hold only strings".to_string()),
        })
        .collect()
}

fn steps(value: &serde_json::Value) -> Result<Vec<Vec<String>>, String> {
    value
        .as_array()
        .ok_or("must be a list of commands")?
        .iter()
        .map(argv)
        .collect()
}

/// A context size in tokens: positive, and no larger than any model's window.
fn window(tokens: i64) -> Result<u32, String> {
    if (1..=MAX_COMPACT_WINDOW).contains(&tokens) {
        Ok(tokens as u32)
    } else {
        Err(format!("must be between 1 and {MAX_COMPACT_WINDOW} tokens"))
    }
}

fn timeout(secs: i64) -> Result<Duration, String> {
    if (1..=MAX_CI_TIMEOUT_SECS).contains(&secs) {
        Ok(Duration::from_secs(secs as u64))
    } else {
        Err(format!("must be between 1 and {MAX_CI_TIMEOUT_SECS}"))
    }
}
