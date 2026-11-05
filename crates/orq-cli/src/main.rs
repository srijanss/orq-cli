use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

mod desktop;
mod pipeline;
mod review;
mod storage;

use orq_git::ResolveError;
use orq_tui::{PopupModel, PopupView};

const USAGE: &str = "usage: orq --version | orq project inspect [--cwd <path>] | orq features --repo <path> | orq feature set-status --repo <path> --id <name> --status pending | orq feature add --repo <path> --name <name> [--description <text>] [--depends-on <name>]... [--parallelizable yes|no] [--test-file <path>] [--target-file <path>]... | orq feature edit --repo <path> --name <name> [same field flags as add] | orq models --repo <path> | orq runs --repo <path> [--json] | orq review build <run-id> --repo <path> --feature <id> --base <rev> [--head <rev>] [--review-input <file>] | orq review show <run-id> [--json] | orq review refresh <run-id> | orq storage status|list|prune [--dry-run]|maintain | orq run pin|unpin <run-id> | orq run --repo <path> (--feature <id> | --group <id,id,...>) --remote <name> --target <branch> [--detach] | orq ui [--snapshot] | orq doctor herdr [--cwd <path>]";

/// Width and height of the frame printed by `orq ui --snapshot`.
const SNAPSHOT_SIZE: (u16, u16) = (100, 3);

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let word = |index: usize| args.get(index).and_then(|arg| arg.to_str());

    match (word(0), word(1)) {
        (Some("--version"), None) => {
            println!("orq {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        (Some("project"), Some("inspect")) => project_inspect(&args[2..]),
        (Some("features"), _) => features(&args[1..]),
        (Some("models"), _) => models(&args[1..]),
        (Some("runs"), _) => runs(&args[1..]),
        (Some("review"), _) => review::review(&args[1..]),
        (Some("feature"), Some("set-status")) => feature_set_status(&args[2..]),
        (Some("feature"), Some("add")) => feature_add(&args[2..]),
        (Some("feature"), Some("edit")) => feature_edit(&args[2..]),
        (Some("storage"), _) => storage::storage(&args[1..]),
        (Some("run"), Some("pin")) => storage::pin(&args[2..], true),
        (Some("run"), Some("unpin")) => storage::pin(&args[2..], false),
        (Some("run"), Some("pause")) => run_pause(&args[2..]),
        (Some("run"), Some("stop")) => run_stop(&args[2..]),
        (Some("run"), Some("discard")) => run_discard(&args[2..]),
        (Some("run"), Some("switch")) => run_switch(&args[2..]),
        (Some("run"), Some("resume")) if args.get(2).is_some_and(|a| !a.to_string_lossy().starts_with("-")) => run_resume(&args[2..]),
        (Some("run"), _) => run(&args[1..]),
        (Some("ui"), _) => ui(&args[1..]),
        (Some("doctor"), Some("herdr")) => doctor_herdr(&args[2..]),
        _ => usage_error(),
    }
}

fn usage_error() -> ExitCode {
    eprintln!("{USAGE}");
    ExitCode::from(2)
}

/// Diagnostics can carry repository-controlled text (paths, ledger names), so
/// control characters are shown escaped and never reach the terminal raw.
fn printable(text: &str) -> String {
    text.chars()
        .flat_map(|c| if c.is_control() { c.escape_default().collect::<Vec<_>>() } else { vec![c] })
        .collect()
}

fn fail(message: impl std::fmt::Display) -> ExitCode {
    eprintln!("orq: {}", printable(&message.to_string()));
    ExitCode::FAILURE
}

/// The directory `orq` was launched for: `--cwd`, else the Herdr active pane
/// directory, else the process working directory. Fallbacks are only consulted
/// when no higher-priority source is present, so a broken fallback never
/// defeats a valid explicit choice.
fn launch_cwd(explicit: Option<&Path>) -> Result<PathBuf, ExitCode> {
    let pane_cwd = if explicit.is_none() {
        match std::env::var_os("HERDR_ACTIVE_PANE_CWD") {
            None => None,
            Some(value) => match value.into_string() {
                Ok(value) => Some(value),
                Err(_) => return Err(fail("HERDR_ACTIVE_PANE_CWD is not valid UTF-8")),
            },
        }
    } else {
        None
    };
    let process_cwd = if explicit.is_none() && pane_cwd.is_none() {
        match std::env::current_dir() {
            Ok(dir) => dir,
            Err(err) => return Err(fail(format!("cannot determine the working directory: {err}"))),
        }
    } else {
        PathBuf::new() // never consulted: a higher-priority source is present
    };

    orq_herdr::resolve_launch_cwd(explicit, pane_cwd.as_deref(), &process_cwd)
        .map_err(|err| fail(launch_cwd_error_text(&err)))
}

fn launch_cwd_error_text(err: &orq_herdr::LaunchCwdError) -> String {
    use orq_herdr::{CwdSource, LaunchCwdError::*};
    let source = |source: &CwdSource| match source {
        CwdSource::Explicit => "--cwd",
        CwdSource::HerdrActivePaneCwd => "HERDR_ACTIVE_PANE_CWD",
        CwdSource::ProcessCwd => "the process working directory",
    };
    match err {
        Empty { source: s } => format!("invalid working directory: {} is empty", source(s)),
        NotFound { source: s, path } => {
            format!("invalid working directory: {} names {}, which is not a directory", source(s), path.display())
        }
    }
}

fn project_inspect(args: &[OsString]) -> ExitCode {
    let explicit: Option<PathBuf> = match args {
        [] => None,
        [flag, path] if flag == "--cwd" => Some(PathBuf::from(path)),
        _ => return usage_error(),
    };

    let cwd = match launch_cwd(explicit.as_deref()) {
        Ok(cwd) => cwd,
        Err(code) => return code,
    };
    let project = match orq_git::resolve_project(&cwd) {
        Ok(project) => project,
        Err(err) => return fail(format!("cannot resolve project: {err:?}")),
    };

    let (Some(root), Some(common)) = (path_str(&project.root), path_str(&project.git_common_dir)) else {
        return fail("repository path is not valid UTF-8");
    };
    let json = serde_json::json!({
        "name": project.name,
        "id": pipeline::project_id(&project),
        "root": root,
        "git_common_dir": common,
        "is_linked_worktree": project.is_linked_worktree,
    });
    println!("{json}");
    ExitCode::SUCCESS
}

/// `orq features --repo <path>`: the project's ledger as a JSON list. A ledger
/// that cannot be loaded prints its typed reason on stderr and no list.
fn features(args: &[OsString]) -> ExitCode {
    let repo = match args {
        [flag, path] if flag == "--repo" => PathBuf::from(path),
        _ => return usage_error(),
    };
    let project = match orq_git::resolve_project(&repo) {
        Ok(project) => project,
        Err(err) => return fail(format!("cannot resolve project: {err:?}")),
    };
    let entries = match orq_features::load_ledger(&orq_features::ledger_path(&project)) {
        Ok(entries) => entries,
        Err(err) => return fail(ledger_error_text(&err)),
    };
    let list: Vec<serde_json::Value> = entries.iter().map(entry_json).collect();
    println!("{}", serde_json::Value::Array(list));
    ExitCode::SUCCESS
}

/// Exit codes of `orq run`, one per outcome (0 is a run that reached review_ready).
const EXIT_INELIGIBLE: u8 = 20;
const EXIT_MODEL_SELECTION: u8 = 21;
const EXIT_BUSY: u8 = 22;
const EXIT_BLOCKED: u8 = 30;
const EXIT_FAILED: u8 = 31;

/// The flags of `orq run`.
#[derive(Default)]
struct RunArgs {
    repo: Option<PathBuf>,
    feature: Option<String>,
    /// `--group A,B,C`: several features for one run.
    group: Option<String>,
    remote: Option<String>,
    target: Option<String>,
    implementer: Option<String>,
    reviewer: Option<String>,
    model: Option<String>,
    context: Option<String>,
    issue: Option<String>,
    mr_assignee: Option<String>,
    mr_reviewer: Option<String>,
    no_rebase: bool,
    no_gate: bool,
    no_ci_check: bool,
    publish: bool,
    resume: bool,
    /// Set by `orq run resume` for a role whose profile was switched; other roles keep the paused run's.
    switch_implementer: bool,
    switch_reviewer: bool,
    detach: bool,
    /// Set by a detached launch for the runner it starts; not meant for people.
    run_id: Option<String>,
}

/// The `orq run` flags that take a value; parsing and forwarding both read this list.
const RUN_VALUE_FLAGS: [&str; 13] = [
    "--repo",
    "--feature",
    "--group",
    "--remote",
    "--target",
    "--implementer",
    "--reviewer",
    "--model",
    "--context",
    "--issue",
    "--mr-assignee",
    "--mr-reviewer",
    "--run-id",
];

fn parse_run_args(args: &[OsString]) -> Option<RunArgs> {
    let mut parsed = RunArgs::default();
    let mut iter = args.iter();
    while let Some(flag) = iter.next() {
        match flag.to_str()? {
            "--no-rebase" => parsed.no_rebase = true,
            "--no-gate" => parsed.no_gate = true,
            "--no-ci-check" => parsed.no_ci_check = true,
            "--publish" => parsed.publish = true,
            "--resume" => parsed.resume = true,
            "--switch-implementer" => parsed.switch_implementer = true,
            "--switch-reviewer" => parsed.switch_reviewer = true,
            "--detach" => parsed.detach = true,
            name if RUN_VALUE_FLAGS.contains(&name) => {
                let value = iter.next()?;
                match name {
                    "--run-id" => parsed.run_id = Some(value.to_str()?.to_string()),
                    "--repo" => parsed.repo = Some(PathBuf::from(value)),
                    "--feature" => parsed.feature = Some(value.to_str()?.to_string()),
                    "--group" => parsed.group = Some(value.to_str()?.to_string()),
                    "--remote" => parsed.remote = Some(value.to_str()?.to_string()),
                    "--target" => parsed.target = Some(value.to_str()?.to_string()),
                    "--implementer" => parsed.implementer = Some(value.to_str()?.to_string()),
                    "--model" => parsed.model = Some(value.to_str()?.to_string()),
                    "--context" => parsed.context = Some(value.to_str()?.to_string()),
                    "--issue" => parsed.issue = Some(value.to_str()?.to_string()),
                    "--mr-assignee" => parsed.mr_assignee = Some(value.to_str()?.to_string()),
                    "--mr-reviewer" => parsed.mr_reviewer = Some(value.to_str()?.to_string()),
                    _ => parsed.reviewer = Some(value.to_str()?.to_string()),
                }
            }
            _ => return None,
        }
    }
    Some(parsed)
}

/// A `--group` value: feature ids separated by commas, each a plain word, none repeated.
fn checked_group(text: &str) -> Result<Vec<String>, String> {
    let mut ids: Vec<String> = Vec::new();
    for id in text.split(',') {
        let plain = !id.is_empty()
            && !id.starts_with('-')
            && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | '@' | '+'));
        if !plain {
            return Err(format!("{} is not a plain feature id", printable(id)));
        }
        if ids.iter().any(|seen| seen == id) {
            return Err(format!("{} is listed twice", printable(id)));
        }
        ids.push(id.to_string());
    }
    Ok(ids)
}

fn group_error(message: String) -> ExitCode {
    eprintln!("orq: {message}");
    ExitCode::from(2)
}

/// The `orq run` arguments without the `--detach` flag. An option value that happens to be
/// spelled `--detach` is kept: values are skipped as the parser reads them.
fn without_detach_flag(args: &[OsString]) -> Vec<OsString> {
    let mut kept = Vec::with_capacity(args.len());
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.to_str() {
            Some("--detach") => {}
            Some(name) if RUN_VALUE_FLAGS.contains(&name) => {
                kept.push(arg.clone());
                kept.extend(iter.next().cloned());
            }
            _ => kept.push(arg.clone()),
        }
    }
    kept
}

/// `orq run --repo <path> --feature <id> --remote <name> --target <branch>`: check that the
/// feature may start and which models it would use, before anything is created.
fn run(args: &[OsString]) -> ExitCode {
    let Some(parsed) = parse_run_args(args) else { return usage_error() };
    // A group of one is the feature itself; several features wait for grouped runs.
    let group = match (parsed.group.as_deref(), parsed.feature.as_deref()) {
        (Some(_), Some(_)) => return group_error("--group and --feature cannot be given together".to_string()),
        (Some(text), None) => match checked_group(text) {
            Ok(ids) => Some(ids),
            Err(reason) => return group_error(format!("--group: {reason}")),
        },
        (None, _) => None,
    };
    let feature = group.as_ref().and_then(|ids| ids.first().map(String::as_str)).or(parsed.feature.as_deref());
    let (Some(repo), Some(feature), Some(remote), Some(target)) =
        (parsed.repo.as_deref(), feature, parsed.remote.as_deref(), parsed.target.as_deref())
    else {
        return usage_error();
    };
    let project = match orq_git::resolve_project(repo) {
        Ok(project) => project,
        Err(err) => return fail(format!("cannot resolve project: {err:?}")),
    };
    let ledger = match orq_features::load_ledger(&orq_features::ledger_path(&project)) {
        Ok(entries) => entries,
        Err(err) => return fail(ledger_error_text(&err)),
    };
    // The features of the run, in the order to do them: dependencies first for a group.
    let features: Vec<String> = match &group {
        Some(ids) => match orq_features::order_run_group(&ledger, ids) {
            Ok(ordered) => ordered.into_iter().map(|entry| entry.feature_name.clone()).collect(),
            Err(err) => {
                eprintln!("orq: the group may not start a run: {}", printable(&err.to_string()));
                return ExitCode::from(EXIT_INELIGIBLE);
            }
        },
        None => {
            if let Err(err) = orq_features::check_run_eligibility(&ledger, feature) {
                eprintln!("orq: feature {} may not start a run: {}", printable(feature), printable(&err.to_string()));
                return ExitCode::from(EXIT_INELIGIBLE);
            }
            vec![feature.to_string()]
        }
    };
    let feature = features[0].as_str();
    let (profiles, reviewers) = match load_model_config(&project.root) {
        Ok(config) => config,
        Err(message) => return fail(message),
    };
    let selection = orq_features::RunSelection {
        implementer: parsed.implementer.as_deref(),
        reviewer: parsed.reviewer.as_deref(),
    };
    let models = match orq_features::resolve_run_models(&profiles, &reviewers, &selection) {
        Ok(models) => models,
        Err(err) => {
            eprintln!("orq: {}", printable(&err.to_string()));
            return ExitCode::from(EXIT_MODEL_SELECTION);
        }
    };
    // Checked before a detached launch spawns anything, so a bad settings file is an error the
    // caller sees, not a runner that exits unseen.
    let context = match parsed.context.as_deref().map(orq_agents::checked_context).transpose() {
        Ok(text) => text.flatten().map(str::to_string),
        Err(err) => return fail(format!("{err}")),
    };
    let issue = match parsed.issue.as_deref().map(orq_orchestrator::issue_number).transpose() {
        Ok(number) => number,
        Err(err) => return fail(format!("--issue: {err}")),
    };
    if let Err(err) = orq_orchestrator::ReviewPeople::checked(parsed.mr_assignee.as_deref(), parsed.mr_reviewer.as_deref()) {
        return fail(format!("--mr-assignee/--mr-reviewer: {err}"));
    }
    if let Some(Err(reason)) = parsed.model.as_deref().map(orq_orchestrator::checked_model) {
        return fail(format!("--model: {reason}"));
    }
    if let Err(err) = orq_orchestrator::load_run_settings(&project.root, &|name| std::env::var(name).ok()) {
        return fail(format!("run settings: {err}"));
    }
    if parsed.detach {
        if parsed.run_id.is_some() {
            return usage_error();
        }
        match pipeline::active_group_run(&project, &features) {
            Ok(None) => {}
            Ok(Some((busy, active))) => {
                eprintln!("orq: feature {} already has an active run: {}", printable(&busy), printable(&active));
                return ExitCode::from(EXIT_BUSY);
            }
            Err(message) => return fail(message),
        }
        let forwarded = without_detach_flag(args);
        return match pipeline::launch_detached(&forwarded) {
            Ok((run_id, log)) => {
                println!("run: {run_id}");
                println!("log: {}", log.display());
                println!("state: launched");
                ExitCode::SUCCESS
            }
            Err(message) => fail(message),
        };
    }
    let options = pipeline::Options {
        project: &project,
        features: &features,
        remote,
        target,
        implementer: models.implementer,
        reviewer: models.reviewer,
        model: parsed.model.as_deref(),
        rebase: !parsed.no_rebase,
        gate: !parsed.no_gate,
        ci_check: !parsed.no_ci_check,
        run_id: parsed.run_id.as_deref(),
        context: context.as_deref(),
        issue: issue.as_deref(),
        mr_assignee: parsed.mr_assignee.as_deref(),
        mr_reviewer: parsed.mr_reviewer.as_deref(),
        publish: parsed.publish,
        resume: parsed.resume,
        switch_implementer: parsed.switch_implementer,
        switch_reviewer: parsed.switch_reviewer,
    };
    match pipeline::execute(&options) {
        Ok(report) => {
            println!("run: {}", report.run_id);
            println!("state: {}", report.ending.name());
            if let Some(url) = &report.review_url {
                println!("review: {}", printable(url));
            }
            if let Some(detail) = &report.detail {
                eprintln!("orq: {}", printable(detail));
            }
            ExitCode::from(match report.ending {
                pipeline::Ending::ReviewReady | pipeline::Ending::Paused => 0,
                pipeline::Ending::Blocked => EXIT_BLOCKED,
                pipeline::Ending::Failed => EXIT_FAILED,
            })
        }
        Err(pipeline::StartError::Busy(active)) => {
            if features.len() > 1 {
                eprintln!("orq: a feature of the group {} already has an active run: {}", printable(&features.join(",")), printable(&active));
            } else {
                eprintln!("orq: feature {} already has an active run: {}", printable(feature), printable(&active));
            }
            ExitCode::from(EXIT_BUSY)
        }
        Err(pipeline::StartError::Other(message)) => fail(message),
    }
}

/// The model profiles and the reviewer policy a project is configured with.
fn load_model_config(
    root: &Path,
) -> Result<(orq_features::ModelProfiles, orq_features::TddReviewerConfig), String> {
    let user_config = orq_features::settings_path(
        std::env::var_os("XDG_CONFIG_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    );
    let profiles = orq_features::load_model_profiles(
        user_config.as_deref().unwrap_or(Path::new("")),
        &root.join(".orq.toml"),
    )
    .map_err(|err| err.to_string())?;
    let reviewers =
        orq_features::load_reviewer_config(&root.join(".tdd-config.json")).map_err(|err| err.to_string())?;
    Ok((profiles, reviewers))
}

/// `orq models --repo <path>`: the model profiles, which roles each can fill,
/// and the defaults a run would use. Unusable defaults are reported in the
/// output; configuration that cannot be read fails the command.
/// `orq runs --repo <path> [--json]`: the runs of the repository's project, from the run store.
/// `orq runs reap --repo <path> [--dry-run] [--yes]`: remove the local worktree and branch of
/// every finished run whose review was merged or closed, then the private rebase refs no live run
/// owns and the test databases of finished runs whose worktree is gone. Without `--yes` it only reports.
fn reap(args: &[OsString]) -> ExitCode {
    let (mut repo, mut yes, mut dry) = (None, false, false);
    let mut words = args.iter();
    while let Some(word) = words.next() {
        match word.to_str() {
            Some("--repo") => match words.next() {
                Some(path) if repo.is_none() => repo = Some(PathBuf::from(path)),
                _ => return usage_error(),
            },
            Some("--yes") => yes = true,
            Some("--dry-run") => dry = true,
            _ => return usage_error(),
        }
    }
    let Some(repo) = repo else { return usage_error() };
    if yes && dry {
        return usage_error();
    }
    let project = match orq_git::resolve_project(&repo) {
        Ok(project) => project,
        Err(err) => return fail(format!("cannot resolve project: {err:?}")),
    };
    let (_, db, store) = match storage::open() {
        Ok(opened) => opened,
        Err(err) => return fail(err),
    };
    let pinned = match db.pinned() {
        Ok(pinned) => pinned,
        Err(err) => return fail(format!("cannot read the pinned runs: {err}")),
    };
    let program_for = |provider: &str| pipeline::find_program(if provider == "gitlab" { "glab" } else { "gh" });
    let items = match orq_orchestrator::plan_reap(
        &store,
        &repo,
        &pipeline::project_id(&project),
        &pinned,
        &program_for,
        std::time::Duration::from_secs(60),
    ) {
        Ok(items) => items,
        Err(err) => return fail(err),
    };
    let mut failed = false;
    for item in &items {
        match &item.verdict {
            orq_orchestrator::ReapVerdict::Keep(reason) => println!("keep\t{}\t{}\t{reason}", item.run_id, item.feature_id),
            orq_orchestrator::ReapVerdict::Reap if !yes => println!("would remove\t{}\t{}\t{}", item.run_id, item.feature_id, item.branch),
            orq_orchestrator::ReapVerdict::Reap => match orq_orchestrator::reap_run(&repo, item) {
                Ok(()) => {
                    println!("removed\t{}\t{}\t{}", item.run_id, item.feature_id, item.branch);
                    match orq_orchestrator::load_run_settings(&repo, &|name| std::env::var(name).ok()) {
                        Ok(settings) => {
                            for note in orq_orchestrator::reap_run_databases(&repo, &item.branch, &settings) {
                                eprintln!("orq: {}: {note}", item.run_id);
                            }
                        }
                        Err(err) => eprintln!("orq: {}: test databases not dropped, run settings: {err}", item.run_id),
                    }
                }
                Err(err) => {
                    failed = true;
                    eprintln!("orq: {}: {err}", item.run_id);
                }
            },
        }
    }
    let orphans = match orq_orchestrator::plan_orphans(&store, &repo, &pipeline::project_id(&project)) {
        Ok(orphans) => orphans,
        Err(err) => {
            eprintln!("orq: orphaned refs and databases not checked: {err}");
            return ExitCode::from(1);
        }
    };
    for name in &orphans.refs {
        if !yes {
            println!("would remove ref\t{name}");
        } else {
            match orq_orchestrator::reap_orphan_ref(&repo, name) {
                Ok(()) => println!("removed ref\t{name}"),
                Err(err) => {
                    failed = true;
                    eprintln!("orq: {err}");
                }
            }
        }
    }
    if !orphans.database_branches.is_empty() {
        let settings = orq_orchestrator::load_run_settings(&repo, &|name| std::env::var(name).ok());
        for (run_id, branch) in &orphans.database_branches {
            if !yes {
                println!("would drop databases\t{run_id}\t{branch}");
                continue;
            }
            match &settings {
                Ok(settings) => {
                    let notes = orq_orchestrator::reap_run_databases(&repo, branch, settings);
                    println!("dropped databases\t{run_id}\t{branch}");
                    for note in notes {
                        eprintln!("orq: {run_id}: {note}");
                    }
                }
                Err(err) => eprintln!("orq: {run_id}: test databases not dropped, run settings: {err}"),
            }
        }
    }
    if failed {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn runs(args: &[OsString]) -> ExitCode {
    if args.first().is_some_and(|a| a == "reap") {
        return reap(&args[1..]);
    }
    let (repo, json) = match args {
        [flag, path] if flag == "--repo" => (PathBuf::from(path), false),
        [flag, path, json] if flag == "--repo" && json == "--json" => (PathBuf::from(path), true),
        _ => return usage_error(),
    };
    let project = match orq_git::resolve_project(&repo) {
        Ok(project) => project,
        Err(err) => return fail(format!("cannot resolve project: {err:?}")),
    };
    let Some(root) = pipeline::state_root() else {
        return fail("cannot locate the Orq state directory: neither XDG_STATE_HOME nor HOME is set");
    };
    let store = orq_orchestrator::RunStore::new(&root);
    let project_id = pipeline::project_id(&project);
    let (listings, unreadable) = match list_runs_reconciled(&store, &project_id) {
        Ok(list) => (list.runs, list.unreadable),
        Err(err) => return fail(err),
    };
    let phase = |phase: orq_orchestrator::RunPhase| phase.as_str();
    if json {
        let list: Vec<serde_json::Value> = listings
            .iter()
            .map(|run| {
                serde_json::json!({
                    "run_id": run.run_id,
                    "feature_id": run.feature_id,
                    "phase": phase(run.phase),
                    "stage": run.stage,
                    "reason": run.reason,
                    "implementer_model": run.implementer_model,
                    "reviewer_model": run.reviewer_model,
                    "review_url": run.review_url,
                })
            })
            .collect();
        println!("{}", serde_json::json!({ "runs": list, "unreadable": unreadable }));
    } else {
        for run in &listings {
            let field = |value: &Option<String>| value.as_deref().unwrap_or("-").to_string();
            println!(
                "{}\t{}\t{}\t{}\t{}/{}\t{}\t{}",
                printable(&run.run_id),
                printable(&run.feature_id),
                phase(run.phase),
                printable(&field(&run.stage)),
                printable(&field(&run.implementer_model)),
                printable(&field(&run.reviewer_model)),
                printable(&field(&run.reason)),
                printable(&field(&run.review_url)),
            );
        }
        if unreadable > 0 {
            eprintln!("{unreadable} {} could not be read", if unreadable == 1 { "run" } else { "runs" });
        }
    }
    ExitCode::SUCCESS
}

fn models(args: &[OsString]) -> ExitCode {
    let repo = match args {
        [flag, path] if flag == "--repo" => PathBuf::from(path),
        _ => return usage_error(),
    };
    let project = match orq_git::resolve_project(&repo) {
        Ok(project) => project,
        Err(err) => return fail(format!("cannot resolve project: {err:?}")),
    };
    let user_config = orq_features::settings_path(
        std::env::var_os("XDG_CONFIG_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    );
    let profiles = match orq_features::load_model_profiles(
        user_config.as_deref().unwrap_or(Path::new("")),
        &project.root.join(".orq.toml"),
    ) {
        Ok(profiles) => profiles,
        Err(err) => return fail(err),
    };
    let reviewers = match orq_features::load_reviewer_config(&project.root.join(".tdd-config.json")) {
        Ok(config) => config,
        Err(err) => return fail(err),
    };

    let list: Vec<serde_json::Value> = profiles
        .profiles()
        .iter()
        .map(|profile| {
            let reviewer = reviewers.allowed_models.as_ref().is_none_or(|allowed| allowed.contains(&profile.model));
            serde_json::json!({
                "name": profile.name,
                "harness": profile.harness,
                "model": profile.model,
                "thinking": profile.thinking,
                "provider": profile.provider,
                "roles": {"implementer": profile.harness == "claude", "reviewer": reviewer},
            })
        })
        .collect();
    let mut out = serde_json::json!({
        "profiles": list,
        "allowedModels": reviewers.allowed_models,
        "defaults": serde_json::Value::Null,
    });
    match orq_features::resolve_run_models(&profiles, &reviewers, &orq_features::RunSelection::default()) {
        Ok(resolved) => {
            let role = |m: &orq_features::ResolvedModel| {
                serde_json::json!({"harness": m.harness, "model": m.model, "thinking": m.thinking})
            };
            out["defaults"] = serde_json::json!({
                "implementer": role(&resolved.implementer),
                "reviewer": role(&resolved.reviewer),
            });
        }
        Err(err) => out["defaultsError"] = serde_json::Value::String(err.to_string()),
    }
    println!("{out}");
    ExitCode::SUCCESS
}

fn entry_json(entry: &orq_features::FeatureEntry) -> serde_json::Value {
    serde_json::json!({
        "name": entry.feature_name,
        "status": status_text(&entry.status),
        "description": entry.description,
        "dependsOn": entry.depends_on,
    })
}

/// The flags `feature add` takes; a flag that is not given stays `None`, so
/// the field is left out of the ledger.
#[derive(Default)]
struct FieldFlags {
    repo: Option<PathBuf>,
    name: Option<String>,
    description: Option<String>,
    depends_on: Option<Vec<String>>,
    parallelizable: Option<bool>,
    test_file: Option<String>,
    target_files: Option<Vec<String>>,
}

/// Parses `--flag value` pairs.
/// Errors are the message to show.
fn parse_field_flags(args: &[OsString]) -> Result<FieldFlags, String> {
    let mut flags = FieldFlags::default();
    let mut iter = args.iter();
    while let Some(flag) = iter.next() {
        let flag = flag.to_str().ok_or("arguments must be valid UTF-8")?;
        let value = iter.next().ok_or(format!("{flag} needs a value"))?;
        let text = || value.to_str().map(str::to_string).ok_or("arguments must be valid UTF-8".to_string());
        match flag {
            "--repo" => flags.repo = Some(PathBuf::from(value)),
            "--name" => flags.name = Some(text()?),
            "--description" => flags.description = Some(text()?),
            "--depends-on" => flags.depends_on.get_or_insert_with(Vec::new).push(text()?),
            "--test-file" => flags.test_file = Some(text()?),
            "--target-file" => flags.target_files.get_or_insert_with(Vec::new).push(text()?),
            "--parallelizable" => {
                flags.parallelizable = Some(match text()?.as_str() {
                    "yes" => true,
                    "no" => false,
                    other => return Err(format!("--parallelizable must be yes or no, not {other:?}")),
                })
            }
            other => return Err(format!("unknown flag {other}")),
        }
    }
    Ok(flags)
}

/// The saved entry as printed: the list fields plus the optional ones that are present.
fn saved_entry_json(entry: &orq_features::FeatureEntry) -> serde_json::Value {
    let mut json = entry_json(entry);
    let map = json.as_object_mut().expect("entry_json is an object");
    if let Some(flag) = entry.parallelizable {
        map.insert("parallelizable".into(), flag.into());
    }
    if let Some(test_file) = &entry.test_file {
        map.insert("testFile".into(), test_file.clone().into());
    }
    if !entry.target_files.is_empty() {
        map.insert("targetFiles".into(), entry.target_files.clone().into());
    }
    json
}

/// `orq feature add --repo <path> --name <name> [...]`: appends a draft and
/// prints it. Never starts an agent, worktree or run.
fn feature_add(args: &[OsString]) -> ExitCode {
    let flags = match parse_field_flags(args) {
        Ok(flags) => flags,
        Err(message) => return fail(message),
    };
    let Some(repo) = flags.repo else { return fail("--repo is required") };
    let Some(name) = flags.name else { return fail("--name is required") };
    let project = match orq_git::resolve_project(&repo) {
        Ok(project) => project,
        Err(err) => return fail(format!("cannot resolve project: {err:?}")),
    };
    let path = orq_features::ledger_path(&project);
    let loaded = match orq_features::LoadedLedger::load_or_new(&path) {
        Ok(loaded) => loaded,
        Err(err) => return fail(ledger_error_text(&err)),
    };
    let draft = orq_features::FeatureDraft {
        feature_name: name,
        description: flags.description,
        depends_on: flags.depends_on,
        parallelizable: flags.parallelizable,
        test_file: flags.test_file,
        target_files: flags.target_files,
    };
    match loaded.append(&draft) {
        Ok(entry) => {
            println!("{}", saved_entry_json(&entry));
            ExitCode::SUCCESS
        }
        Err(err) => fail(err),
    }
}

/// `orq feature edit --repo <path> --name <name> [...]`: changes only the
/// given fields of an existing feature. There is no rename or status flag.
fn feature_edit(args: &[OsString]) -> ExitCode {
    let flags = match parse_field_flags(args) {
        Ok(flags) => flags,
        Err(message) => return fail(message),
    };
    let Some(repo) = flags.repo else { return fail("--repo is required") };
    let Some(name) = flags.name else { return fail("--name is required") };
    let project = match orq_git::resolve_project(&repo) {
        Ok(project) => project,
        Err(err) => return fail(format!("cannot resolve project: {err:?}")),
    };
    let loaded = match orq_features::LoadedLedger::load(&orq_features::ledger_path(&project)) {
        Ok(loaded) => loaded,
        Err(err) => return fail(ledger_error_text(&err)),
    };
    let edits = orq_features::FieldEdits {
        description: flags.description,
        depends_on: flags.depends_on,
        parallelizable: flags.parallelizable,
        test_file: flags.test_file,
        target_files: flags.target_files,
    };
    match loaded.edit_fields(&name, &edits) {
        Ok(entry) => {
            println!("{}", saved_entry_json(&entry));
            ExitCode::SUCCESS
        }
        Err(orq_features::EditFieldsError::UnknownFeature) => fail(format!("no feature named {name:?}")),
        Err(err) => fail(err),
    }
}

/// `orq feature set-status --repo <path> --id <name> --status pending`:
/// promotes a draft and prints the updated entry. Every refusal exits
/// non-zero with a specific message and leaves the ledger untouched.
fn feature_set_status(args: &[OsString]) -> ExitCode {
    let (repo, id, status) = match args {
        [r, repo, i, id, s, status] if r == "--repo" && i == "--id" && s == "--status" => {
            match (id.to_str(), status.to_str()) {
                (Some(id), Some(status)) => (PathBuf::from(repo), id, status),
                _ => return usage_error(),
            }
        }
        _ => return usage_error(),
    };
    if status != "pending" {
        return fail(format!("cannot set status {status:?}: only --status pending is supported"));
    }
    let project = match orq_git::resolve_project(&repo) {
        Ok(project) => project,
        Err(err) => return fail(format!("cannot resolve project: {err:?}")),
    };
    let path = orq_features::ledger_path(&project);
    match orq_features::promote_feature(&path, id) {
        Ok(entry) => {
            println!("{}", entry_json(&entry));
            ExitCode::SUCCESS
        }
        Err(err) => fail(promote_error_text(&err, id)),
    }
}

fn promote_error_text(err: &orq_features::PromoteError, id: &str) -> String {
    use orq_features::PromoteError::*;
    match err {
        Ledger(err) => ledger_error_text(err),
        UnknownFeature(_) => format!("unknown feature: {id}"),
        UnrecognisedStatus(text) => format!("feature {id} has unrecognised status {text:?} and is left alone"),
        IllegalTransition(t) => format!("feature {id}: {:?} to {:?} is not allowed", t.from, t.to),
        Conflict => "ledger changed while it was being edited; nothing was written, try again".to_string(),
        Locked => "ledger is locked by another writer; nothing was written, try again".to_string(),
        Io(err) => format!("ledger could not be written: {err}"),
    }
}

fn status_text(status: &orq_features::LedgerStatus) -> &str {
    use orq_core::FeatureStatus::*;
    match status {
        orq_features::LedgerStatus::Known(Draft) => "draft",
        orq_features::LedgerStatus::Known(Pending) => "pending",
        orq_features::LedgerStatus::Known(InProgress) => "in_progress",
        orq_features::LedgerStatus::Known(Completed) => "completed",
        orq_features::LedgerStatus::Known(Abandoned) => "abandoned",
        orq_features::LedgerStatus::Unrecognised(text) => text,
    }
}

fn ledger_error_text(err: &orq_features::LedgerError) -> String {
    use orq_features::LedgerError::*;
    match err {
        Missing { path } => format!("ledger missing: {}", path.display()),
        Unreadable { path, source } => format!("ledger unreadable: {}: {source}", path.display()),
        Malformed { path, reason, .. } => format!("ledger malformed: {}: {reason}", path.display()),
    }
}

const HERDR_VARS: [&str; 6] = [
    "HERDR_ACTIVE_PANE_CWD",
    "HERDR_ACTIVE_PANE_ID",
    "HERDR_ACTIVE_TAB_ID",
    "HERDR_ACTIVE_WORKSPACE_ID",
    "HERDR_SOCKET_PATH",
    "HERDR_BIN_PATH",
];

/// `orq doctor herdr [--cwd <path>]`: a read-only report of what Herdr passed
/// in and what Orq resolved from it. An unusable launch directory is an error,
/// never a silent fallback to another project.
/// How long the Herdr version probe may run before it is abandoned.
const VERSION_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// The most version output that is read; anything longer is not a version.
const VERSION_PROBE_MAX_BYTES: u64 = 1024;

/// `<bin> --version` stdout, or `None` when it fails, cannot start or does not
/// finish in time. The child is killed and reaped on every path.
fn herdr_version(bin: &OsString) -> Option<String> {
    use std::io::Read;
    use std::process::{Command, Stdio};

    let mut child = Command::new(bin)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        // One byte past the limit is enough to know the output is too long.
        let _ = stdout.take(VERSION_PROBE_MAX_BYTES + 1).read_to_end(&mut bytes);
        let _ = tx.send(bytes);
    });

    // One deadline covers both the output and the process exiting: a child
    // that closes stdout and keeps running is cut off the same way.
    let deadline = std::time::Instant::now() + VERSION_PROBE_TIMEOUT;
    let bytes = rx
        .recv_timeout(VERSION_PROBE_TIMEOUT)
        .ok()
        .filter(|bytes| bytes.len() as u64 <= VERSION_PROBE_MAX_BYTES);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if bytes.is_some() && std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let bytes = bytes?;
    status?.success().then(|| String::from_utf8_lossy(&bytes).trim().to_string())
}

fn doctor_herdr(args: &[OsString]) -> ExitCode {
    let explicit: Option<PathBuf> = match args {
        [] => None,
        [flag, path] if flag == "--cwd" => Some(PathBuf::from(path)),
        _ => return usage_error(),
    };

    for var in HERDR_VARS {
        match std::env::var_os(var) {
            Some(value) => println!("{var}: {}", printable(&value.to_string_lossy())),
            None => println!("{var}: (not set)"),
        }
    }
    let herdr_bin = std::env::var_os("HERDR_BIN_PATH").unwrap_or_else(|| OsString::from("herdr"));
    match herdr_version(&herdr_bin) {
        Some(version) => println!("herdr version: {}", printable(&version)),
        None => println!("herdr version: unavailable ({})", printable(&herdr_bin.to_string_lossy())),
    }

    let cwd = match launch_cwd(explicit.as_deref()) {
        Ok(cwd) => cwd,
        Err(code) => return code,
    };
    println!("launch directory: {}", printable(&cwd.to_string_lossy()));
    let project = match orq_git::resolve_project(&cwd) {
        Ok(project) => project,
        Err(ResolveError::NotARepository { .. }) => {
            println!("repo root: none (not a Git repository)");
            return ExitCode::FAILURE;
        }
        Err(err) => return fail(format!("cannot resolve project: {err:?}")),
    };
    println!("repo root: {}", printable(&project.root.to_string_lossy()));
    println!("linked worktree: {}", if project.is_linked_worktree { "yes" } else { "no" });
    let ledger = orq_features::ledger_path(&project);
    println!("ledger: {}", printable(&ledger.to_string_lossy()));
    match orq_features::load_ledger(&ledger) {
        Ok(entries) => println!("ledger status: ok ({} features)", entries.len()),
        Err(orq_features::LedgerError::Missing { .. }) => println!("ledger status: missing"),
        Err(orq_features::LedgerError::Unreadable { source, .. }) => {
            println!("ledger status: unreadable ({})", printable(&source.to_string()));
        }
        Err(orq_features::LedgerError::Malformed { reason, .. }) => {
            println!("ledger status: malformed ({})", printable(&reason));
        }
    }
    ExitCode::SUCCESS
}

fn ui(args: &[OsString]) -> ExitCode {
    let snapshot = match args {
        [] => false,
        [flag] if flag == "--snapshot" => true,
        _ => return usage_error(),
    };

    let cwd = match launch_cwd(None) {
        Ok(cwd) => cwd,
        Err(code) => return code,
    };
    let view = match view_for(&cwd) {
        Ok(view) => view,
        Err(code) => return code,
    };
    if !snapshot {
        let model = match view {
            PopupView::Project(model) => model,
            PopupView::NotARepository { path } => PopupModel {
                project_name: "not a Git repository".to_string(),
                repo_path: path,
            },
        };
        let result = match orq_git::resolve_project(&cwd) {
            Ok(project) => {
                let mut backend = LedgerBackend::new(orq_features::ledger_path(&project));
                backend.repo_root = Some(project.root.clone());
                orq_tui::run_browser_on_terminal(&model, &mut backend)
            }
            Err(_) => orq_tui::run_on_terminal(&model).map(|_| ()),
        };
        return match result {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => fail(err),
        };
    }
    let (width, height) = SNAPSHOT_SIZE;
    println!("{}", orq_tui::render_snapshot(&view, width, height));
    ExitCode::SUCCESS
}

/// The popup's data source: the project's own ledger, loaded when the popup
/// opens (and on reload) and promoted against exactly those loaded bytes, so a
/// file changed behind the popup's back is a conflict rather than overwritten.
struct LedgerBackend {
    path: PathBuf,
    /// The working tree specs are read from; the ledger may live in another
    /// checkout, so this is not always the ledger's directory.
    repo_root: Option<PathBuf>,
    loaded: Option<orq_features::LoadedLedger>,
    /// The last load failed only because the file does not exist yet.
    absent: bool,
    /// The user-level settings file, when `XDG_CONFIG_HOME` or `HOME` give one.
    settings_path: Option<PathBuf>,
    /// The settings as last loaded for the tab: saves are checked against
    /// this snapshot, so an outside edit is a conflict, not an overwrite.
    settings_snapshot: Option<orq_features::Settings>,
    /// The cleanup plan last shown for confirmation; confirming runs exactly this.
    previewed_cleanup: Option<orq_core::retention::PrunePlan>,
    /// The last generated proposal, kept so accepting names what was reviewed.
    staged: std::sync::Arc<std::sync::Mutex<Vec<orq_features::ReviewedCandidate>>>,
    /// Replaces the PATH lookup of the harness program (tests).
    program_override: Option<PathBuf>,
    /// Replaces the XDG state lookup of the generation report directory (tests).
    log_dir_override: Option<PathBuf>,
    /// Replaces the XDG state lookup of the run store root (tests).
    state_root_override: Option<PathBuf>,
    /// Runs the last `load_runs` left out because their files could not be read.
    unreadable_runs: usize,
    /// Harness name and program pairs replacing the PATH lookup of the
    /// picker's discovery (tests).
    harness_programs: Option<Vec<(String, PathBuf)>>,
    /// What discovery found, kept for the rest of the session.
    harness_cache: Option<Vec<orq_tui::HarnessOption>>,
}

/// `provider/model` names from `pi --list-models` output: a header row, then
/// whitespace-separated rows whose first two columns are provider and model.
fn parse_pi_models(table: &str) -> Vec<String> {
    table
        .lines()
        .skip(1)
        .filter_map(|line| {
            let mut columns = line.split_whitespace();
            Some(format!("{}/{}", columns.next()?, columns.next()?))
        })
        .collect()
}

/// Reads at most `cap + 1` bytes; `None` when the read fails, so partial
/// output is never mistaken for a whole listing.
fn read_capped(reader: impl std::io::Read, cap: u64) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut out = Vec::new();
    reader.take(cap + 1).read_to_end(&mut out).ok()?;
    Some(out)
}

/// Runs `pi --list-models` (argv only). One deadline covers the output and the
/// process's end; whatever is still running then is killed. Empty when it
/// cannot be listed, in which case the picker falls back to free text.
fn list_pi_models(program: &Path, timeout: std::time::Duration) -> Vec<String> {
    const CAP: u64 = 256 * 1024;
    let Ok(mut child) = std::process::Command::new(program)
        .arg("--list-models")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    else {
        return Vec::new();
    };
    let Some(stdout) = child.stdout.take() else {
        return Vec::new();
    };
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(read_capped(stdout, CAP));
    });
    let deadline = std::time::Instant::now() + timeout;
    let mut output: Option<Option<Vec<u8>>> = None;
    let status = loop {
        if output.is_none() {
            output = receiver.try_recv().ok();
        }
        match child.try_wait() {
            Ok(Some(status)) if output.is_some() => break status,
            Ok(None | Some(_)) if std::time::Instant::now() < deadline => {}
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Vec::new();
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    let Some(output) = output.flatten() else {
        return Vec::new();
    };
    if !status.success() || output.len() as u64 > CAP {
        return Vec::new();
    }
    parse_pi_models(&String::from_utf8_lossy(&output))
}

/// The executable for `harness`: a bare name found on PATH, never a path.
fn find_program(harness: &str) -> Option<PathBuf> {
    if harness.is_empty() || harness.contains('/') {
        return None;
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|dir| dir.join(harness)).find(|candidate| {
        use std::os::unix::fs::PermissionsExt;
        candidate.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    })
}

/// What a generation attempt knew, for the failure report.
#[derive(Default)]
struct ReportContext {
    harness: String,
    model: String,
    spec: String,
    focus: String,
    program: String,
    cwd: String,
    diagnostics: orq_features::RunDiagnostics,
}

fn generate_reviewed(
    ledger_path: &Path,
    repo_root: Option<PathBuf>,
    settings_file: Result<PathBuf, String>,
    program_override: Option<PathBuf>,
    log_dir: Option<PathBuf>,
    cancel: &std::sync::atomic::AtomicBool,
    choice: Option<(&str, &str)>,
) -> Result<Vec<orq_features::ReviewedCandidate>, String> {
    let mut context = ReportContext::default();
    let outcome = generate_inner(ledger_path, repo_root, settings_file, program_override, cancel, choice, &mut context);
    let message = match outcome {
        Ok(candidates) if !candidates.is_empty() => return Ok(candidates),
        Ok(_) => "the agent returned 0 features".to_string(),
        Err(message) => message,
    };
    // A cancel is the user's own doing; there is nothing to debug.
    if cancel.load(std::sync::atomic::Ordering::SeqCst) {
        return Err(message);
    }
    let Some(dir) = log_dir else {
        return Err(format!("{message} (no report location: set XDG_STATE_HOME or HOME)"));
    };
    let report = orq_features::GenerationReport {
        harness: context.harness,
        model: context.model,
        spec: context.spec,
        focus: context.focus,
        program: context.program,
        argv: context.diagnostics.argv,
        cwd: context.cwd,
        exit: context.diagnostics.exit,
        duration: context.diagnostics.duration,
        stderr_tail: context.diagnostics.stderr_tail,
        stdout_head: context.diagnostics.stdout_head,
        error: message.clone(),
    };
    match orq_features::write_generation_report(&dir, &report) {
        Ok(path) => Err(format!("{message} (details: {})", path.display())),
        Err(err) => Err(format!("{message} (could not write a report: {err})")),
    }
}

fn generate_inner(
    ledger_path: &Path,
    repo_root: Option<PathBuf>,
    settings_file: Result<PathBuf, String>,
    program_override: Option<PathBuf>,
    cancel: &std::sync::atomic::AtomicBool,
    choice: Option<(&str, &str)>,
    context: &mut ReportContext,
) -> Result<Vec<orq_features::ReviewedCandidate>, String> {
    let repo_root = match repo_root.or_else(|| ledger_path.parent().map(Path::to_path_buf)) {
        Some(root) => root,
        None => return Err("the feature file has no directory".to_string()),
    };
    context.cwd = repo_root.display().to_string();
    if let Some((spec, focus)) = choice {
        context.spec = spec.to_string();
        context.focus = focus.to_string();
    }
    let (spec, focus) = match choice {
        Some((spec, focus)) => (
            orq_features::resolve_spec_file(&repo_root, spec).map_err(|e| e.to_string())?,
            Some(focus).filter(|f| !f.trim().is_empty()),
        ),
        None => (
            orq_features::spec_file_candidates(&repo_root)
                .into_iter()
                .next()
                .ok_or("no Markdown spec file was found at the repository root")?,
            None,
        ),
    };
    context.spec = spec.display().to_string();
    let settings = orq_features::Settings::load(&settings_file?).map_err(|e| e.to_string())?;
    let model = settings
        .generation_agent()
        .map_err(|e| e.to_string())?
        .ok_or(orq_features::GenerationError::NotConfigured.to_string())?;
    context.harness = model.harness.clone();
    context.model = model.model.clone();
    let program = match &program_override {
        Some(program) => program.clone(),
        None => find_program(&model.harness).ok_or_else(|| {
            orq_features::GenerationError::AgentNotFound { harness: model.harness.clone() }.to_string()
        })?,
    };
    context.program = program.display().to_string();
    let artifact_dir = orq_features::generation_artifact_dir(
        std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
    )
    .ok_or("no cache location: set XDG_CACHE_HOME or HOME")?;
    let request = orq_features::GenerationRequest {
        program: &program,
        model: &model,
        repo_root: &repo_root,
        spec: &spec,
        artifact_dir: &artifact_dir,
        timeout: std::time::Duration::from_secs(600),
        focus,
    };
    let (result, diagnostics) = orq_features::run_generation_diagnosed(&request, cancel);
    context.diagnostics = diagnostics;
    let artifact = result.map_err(|e| e.to_string())?;
    let text = std::fs::read_to_string(&artifact)
        .map_err(|e| format!("could not read the agent output {}: {e}", artifact.display()))?;
    let candidates = orq_features::extract_candidates(&text)
        .map_err(|e| format!("{e} (agent output kept at {})", artifact.display()))?;
    let existing = match orq_features::LoadedLedger::load_or_new(ledger_path) {
        Ok(loaded) => loaded.entries().to_vec(),
        Err(err) => return Err(ledger_error_text(&err)),
    };
    Ok(orq_features::review_candidates(&existing, candidates))
}

        fn candidate_views(reviewed: &[orq_features::ReviewedCandidate]) -> Vec<orq_tui::CandidateView> {
    use orq_features::{CandidateProblem, Verdict};
    use orq_tui::{CandidateKind, CandidateView};
    reviewed
            .iter()
            .map(|r| {
                let draft = &r.candidate.draft;
                let kind = match &r.verdict {
                    Verdict::New => CandidateKind::New,
                    Verdict::DuplicateOfExisting => CandidateKind::Duplicate("already in the feature file".into()),
                    Verdict::DuplicateInBatch => CandidateKind::Duplicate("repeated in this output".into()),
                    Verdict::Invalid(problems) => CandidateKind::Invalid(
                        problems
                            .iter()
                            .map(|p| match p {
                                CandidateProblem::UnknownDependency(dep) => format!("depends on unknown \"{dep}\""),
                                CandidateProblem::Cycle => "dependency cycle".to_string(),
                            })
                            .collect::<Vec<_>>()
                            .join("; "),
                    ),
                };
                CandidateView {
                    name: draft.feature_name.clone(),
                    description: draft.description.clone(),
                    depends_on: draft.depends_on.clone().unwrap_or_default(),
                    kind,
                    suggests_parallel: r.suggests_parallel,
                    ignored_fields: r.candidate.ignored_fields.clone(),
                }
            })
            .collect()
}

/// Generation running on its own thread; staging the result on completion
/// keeps `accept_candidates` working exactly as for a synchronous run.
struct WorkerJob {
    receiver: std::sync::mpsc::Receiver<Result<Vec<orq_features::ReviewedCandidate>, String>>,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    staged: std::sync::Arc<std::sync::Mutex<Vec<orq_features::ReviewedCandidate>>>,
    /// Joined on drop so the agent's process group is gone before Orq exits.
    handle: Option<std::thread::JoinHandle<()>>,
    /// A terminal result was already delivered; later polls yield nothing.
    done: bool,
}

impl orq_tui::GenerationJob for WorkerJob {
    fn poll(&mut self) -> Option<Result<Vec<orq_tui::CandidateView>, String>> {
        use std::sync::mpsc::TryRecvError;
        if self.done {
            return None;
        }
        let result = match self.receiver.try_recv() {
            Ok(Ok(reviewed)) => {
                let views = candidate_views(&reviewed);
                *self.staged.lock().unwrap() = reviewed;
                Ok(views)
            }
            Ok(Err(reason)) => Err(reason),
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err("the generation worker stopped unexpectedly".into()),
        };
        self.done = true;
        Some(result)
    }

    fn cancel(&mut self) {
        self.cancel.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

impl Drop for WorkerJob {
    fn drop(&mut self) {
        // Cancelling makes the worker stop the agent promptly, so the join is short.
        self.cancel.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Orq's storage roots, resolved the same way `orq review` does; nothing is created.
fn review_roots() -> Result<orq_core::storage::StorageRoots, String> {
    orq_core::storage::StorageRoots::resolve(&|name| std::env::var(name).ok())
        .map_err(|err| format!("cannot locate the Orq data directory: {err}"))
}

impl LedgerBackend {
    /// Runs `act` on the review link of one of this project's runs.
    fn review_link(&self, run_id: &str, act: &dyn Fn(&str) -> Result<String, String>) -> Result<String, String> {
        let repo = self.repo_root.as_deref().ok_or("the project is not known")?;
        let project = orq_git::resolve_project(repo).map_err(|err| format!("cannot resolve project: {err:?}"))?;
        let root = self
            .state_root_override
            .clone()
            .or_else(pipeline::state_root)
            .ok_or("cannot locate the Orq state directory: neither XDG_STATE_HOME nor HOME is set")?;
        with_review_link(&orq_orchestrator::RunStore::new(&root), &pipeline::project_id(&project), run_id, act)
    }

    fn new(path: PathBuf) -> Self {
        let settings_path = orq_features::settings_path(
            std::env::var_os("XDG_CONFIG_HOME").as_deref(),
            std::env::var_os("HOME").as_deref(),
        );
        Self {
            path,
            repo_root: None,
            loaded: None,
            absent: false,
            settings_path,
            settings_snapshot: None,
            previewed_cleanup: None,
            staged: Default::default(),
            program_override: None,
            log_dir_override: None,
            state_root_override: None,
            unreadable_runs: 0,
            harness_programs: None,
            harness_cache: None,
        }
    }

    fn spec_root(&self) -> Option<PathBuf> {
        self.repo_root.clone().or_else(|| self.path.parent().map(Path::to_path_buf))
    }

    /// Where failure reports go: the XDG state directory unless overridden.
    fn log_dir(&self) -> Option<PathBuf> {
        self.log_dir_override.clone().or_else(|| {
            orq_features::generation_log_dir(
                std::env::var_os("XDG_STATE_HOME").map(PathBuf::from),
                std::env::var_os("HOME").map(PathBuf::from),
            )
        })
    }

    fn settings_file(&self) -> Result<&Path, String> {
        self.settings_path
            .as_deref()
            .ok_or_else(|| "no settings location: set XDG_CONFIG_HOME or HOME to an absolute path".to_string())
    }

    fn spawn_generation(&mut self, choice: Option<(String, String)>) -> Result<Box<dyn orq_tui::GenerationJob>, String> {
        use std::sync::atomic::AtomicBool;
        use std::sync::{mpsc, Arc};
        self.staged.lock().unwrap().clear();
        let path = self.path.clone();
        let repo_root = self.spec_root();
        let settings_file = self.settings_file().map(Path::to_path_buf);
        let program_override = self.program_override.clone();
        let log_dir = self.log_dir();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&cancel);
        let (sender, receiver) = mpsc::channel();
        let handle = std::thread::spawn(move || {
            let _ = sender.send(generate_reviewed(
                &path,
                repo_root,
                settings_file,
                program_override,
                log_dir,
                &flag,
                choice.as_ref().map(|(spec, focus)| (spec.as_str(), focus.as_str())),
            ));
        });
        Ok(Box::new(WorkerJob {
            receiver,
            cancel,
            staged: Arc::clone(&self.staged),
            handle: Some(handle),
            done: false,
        }))
    }
}

/// The run id `orq run --detach` printed, or the first line it gave as the reason it failed.
fn launch_outcome(success: bool, stdout: &str, stderr: &str) -> Result<String, String> {
    if success {
        return stdout
            .lines()
            .find_map(|line| line.strip_prefix("run: "))
            .map(str::to_string)
            .ok_or_else(|| "the run started but reported no run id".to_string());
    }
    let reason = stderr.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or("orq run failed");
    Err(printable(reason.trim_start_matches("orq: ")))
}

/// The runner log path `orq run --detach` printed.
fn launch_log(stdout: &str) -> Option<PathBuf> {
    stdout.lines().find_map(|line| line.strip_prefix("log: ")).map(PathBuf::from)
}

/// How long a launch waits for the detached runner to register its run before giving up on
/// recording the pane.
const REGISTRATION_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// Opens a Herdr tab that follows the run's log and records where it is on the run. The detached
/// runner creates its run record some time after the launch returns, so recording waits for it,
/// for at most `wait`. When it never appears the tab stays open and the error says so.
fn attach_pane(
    herdr: &dyn orq_herdr::HerdrCli,
    store: &orq_orchestrator::RunStore,
    workspace: Option<&str>,
    cwd: &Path,
    run_id: &str,
    log: &Path,
    wait: std::time::Duration,
) -> Result<(), String> {
    attach_pane_in(herdr, store, workspace, cwd, run_id, log, wait, None)
}

/// `attach_pane`, placing the log in the shared `grid` tab when there is one.
#[allow(clippy::too_many_arguments)]
fn attach_pane_in(
    herdr: &dyn orq_herdr::HerdrCli,
    store: &orq_orchestrator::RunStore,
    workspace: Option<&str>,
    cwd: &Path,
    run_id: &str,
    log: &Path,
    wait: std::time::Duration,
    grid: Option<&mut orq_herdr::GridTab>,
) -> Result<(), String> {
    let label = format!("orq {run_id}");
    let pane = match grid {
        Some(grid) => orq_herdr::open_grid_pane(herdr, grid, workspace, cwd, &label, log)?,
        None => orq_herdr::open_run_pane(herdr, workspace, cwd, &label, log)?,
    };
    let pane = orq_orchestrator::PaneRef { workspace_id: pane.workspace_id, tab_id: pane.tab_id, pane_id: pane.pane_id };
    let deadline = std::time::Instant::now() + wait;
    loop {
        match orq_orchestrator::record_pane(store, run_id, &pane) {
            Ok(()) => return Ok(()),
            Err(orq_orchestrator::StoreError::NotFound(_)) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(orq_orchestrator::StoreError::NotFound(_)) if !wait.is_zero() => {
                return Err(format!(
                    "the Herdr tab opened but {run_id} did not register within {}; the run continues and the tab keeps following its log",
                    describe_wait(wait)
                ));
            }
            Err(err) => {
                return Err(format!("the Herdr tab opened but the pane is not recorded on the run: {err}"));
            }
        }
    }
}

/// A wait bound as the user reads it: `10s`, `1.5s`, `200ms`.
fn describe_wait(wait: std::time::Duration) -> String {
    if wait.as_millis() < 1000 {
        format!("{}ms", wait.as_millis())
    } else if wait.subsec_millis() == 0 {
        format!("{}s", wait.as_secs())
    } else {
        format!("{}.{}s", wait.as_secs(), format!("{:03}", wait.subsec_millis()).trim_end_matches('0'))
    }
}

/// Brings the Herdr pane recorded for `run_id` to the front. A run with no recorded pane, or whose
/// pane Herdr no longer has, is `Missing`; anything else that goes wrong is `Failed`.
fn focus_run(herdr: &dyn orq_herdr::HerdrCli, store: &orq_orchestrator::RunStore, run_id: &str) -> orq_tui::PaneFocus {
    let pane = match orq_orchestrator::run_pane(store, run_id) {
        Ok(Some(pane)) => pane,
        Ok(None) => return orq_tui::PaneFocus::Missing(format!("{run_id} has no Herdr pane")),
        Err(err) => return orq_tui::PaneFocus::Failed(format!("cannot read the run: {err}")),
    };
    let location = orq_herdr::PaneLocation { workspace_id: pane.workspace_id, tab_id: pane.tab_id, pane_id: pane.pane_id };
    match orq_herdr::focus_pane(herdr, &location) {
        orq_herdr::FocusOutcome::Focused => orq_tui::PaneFocus::Focused,
        orq_herdr::FocusOutcome::Gone(reason) => {
            orq_tui::PaneFocus::Missing(format!("the pane of {run_id} no longer exists ({})", printable(&reason)))
        }
        orq_herdr::FocusOutcome::Failed(reason) => orq_tui::PaneFocus::Failed(printable(&reason)),
    }
}

/// The most recently written `*.log` file directly in `dir`.
fn newest_log(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "log") && path.is_file())
        .filter_map(|path| Some((path.metadata().ok()?.modified().ok()?, path)))
        .max()
        .map(|(_, path)| path)
}

/// Opens a fresh Herdr pane following the run's runner log and records it, replacing a missing one.
/// The run is already registered, so no waiting is needed.
fn reopen_run(
    herdr: &dyn orq_herdr::HerdrCli,
    store: &orq_orchestrator::RunStore,
    workspace: Option<&str>,
    cwd: &Path,
    state_root: &Path,
    run_id: &str,
) -> orq_tui::PaneFocus {
    let log = state_root.join("logs").join(run_id).join("runner.log");
    // A run started in a terminal has stage logs but no runner log: follow the one written last.
    let log = if log.is_file() { log } else { newest_log(&state_root.join("logs").join(run_id)).unwrap_or(log) };
    if !log.is_file() {
        return orq_tui::PaneFocus::Failed(format!("{run_id} has no log to follow ({})", printable(&log.display().to_string())));
    }
    match attach_pane(herdr, store, workspace, cwd, run_id, &log, std::time::Duration::ZERO) {
        Ok(()) => orq_tui::PaneFocus::Focused,
        Err(reason) => orq_tui::PaneFocus::Failed(printable(&reason)),
    }
}

/// Hands the review link of `run_id` (a run of `project_id`) to `act`. A run with no review
/// request yet, or of another project, says so instead.
fn with_review_link(
    store: &orq_orchestrator::RunStore,
    project_id: &str,
    run_id: &str,
    act: &dyn Fn(&str) -> Result<String, String>,
) -> Result<String, String> {
    let runs = orq_orchestrator::list_runs(store, project_id).map_err(|err| err.to_string())?;
    let run = runs.into_iter().find(|run| run.run_id == run_id).ok_or_else(|| format!("{run_id} is not a run of this project"))?;
    match run.review_url {
        Some(url) => with_checked_url(&url, act),
        None => Err(format!("{run_id} has no review request yet")),
    }
}

/// Hands `url` to `act` only when it is an absolute http(s) link with a host, so a tampered event
/// cannot make the desktop open a file, another scheme or an option.
fn with_checked_url(url: &str, act: &dyn Fn(&str) -> Result<String, String>) -> Result<String, String> {
    let ok = !url.chars().any(|c| c.is_whitespace() || c.is_control() || c == '\\') && has_web_host(url);
    if ok {
        act(url)
    } else {
        Err(format!("the review link is not a web link: {}", printable(url)))
    }
}

/// Whether `url` is `http(s)://[userinfo@]host[:port][/...]` with a plausible host.
fn has_web_host(url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once("://") else { return false };
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return false;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host_port = authority.rsplit('@').next().unwrap_or("");
    let (host, port) = match host_port.strip_prefix('[') {
        Some(v6) => match v6.split_once(']') {
            Some((addr, tail)) => (addr, tail.strip_prefix(':').or(if tail.is_empty() { Some("") } else { None })),
            None => return false,
        },
        None => match host_port.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (host_port, Some("")),
        },
    };
    let host_ok = if host_port.starts_with('[') {
        !host.is_empty() && host.parse::<std::net::Ipv6Addr>().is_ok()
    } else {
        !host.is_empty() && host.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
    };
    host_ok && port.is_some_and(|port| port.chars().all(|c| c.is_ascii_digit()))
}

/// Opens `url` with the desktop's opener.
fn open_url(url: &str) -> Result<String, String> {
    desktop::open_url_with(url, &find_program)
}

/// Puts `text` on the clipboard with the first clipboard tool that works.
fn copy_text(text: &str) -> Result<String, String> {
    desktop::copy_text_with(text, &find_program)
}

/// The branch a run's review targets: the remote's default branch when known, else `main`.
fn default_target_branch(repo: &Path) -> String {
    std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["symbolic-ref", "--short", "refs/remotes/origin/HEAD"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .and_then(|text| text.trim().strip_prefix("origin/").map(str::to_string))
        .filter(|branch| !branch.is_empty())
        .unwrap_or_else(|| "main".to_string())
}

/// The configured base branch (settings file or `ORQ_BASE_BRANCH`), else the remote's default.
fn configured_target_branch(repo: &Path) -> Result<String, String> {
    let settings = orq_orchestrator::load_run_settings(repo, &|name| std::env::var(name).ok()).map_err(|err| format!("run settings: {err}"))?;
    Ok(settings.base_branch.unwrap_or_else(|| default_target_branch(repo)))
}

fn agent_setting(model: Option<orq_features::AgentProfile>) -> orq_tui::AgentSetting {
    model
        .map(|p| orq_tui::AgentSetting { harness: p.harness, model: p.model })
        .unwrap_or_default()
}

/// The MCP names in the output of `mcpctl catalog list`: one `name@version (…)` line each, with
/// indented detail lines under it.
fn parse_mcp_catalog(text: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for line in text.lines().filter(|line| !line.starts_with(char::is_whitespace)) {
        let name = line.split(['@', ' ']).next().unwrap_or("");
        if !name.is_empty() && !names.iter().any(|seen| seen == name) {
            names.push(name.to_string());
        }
    }
    names
}

/// How long the picker waits for `mcpctl catalog list` before it goes without a catalog.
const MCPCTL_LIST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// The servers of the `mcpctl` catalog; none when `mcpctl` is not installed.
fn mcpctl_catalog() -> Result<Vec<String>, String> {
    match pipeline::find_program("mcpctl") {
        Some(program) => mcpctl_catalog_with(&program, MCPCTL_LIST_TIMEOUT),
        None => Ok(Vec::new()),
    }
}

/// Runs `program catalog list` for at most `timeout`; a listing that has not finished by then is killed
/// and reaped, and the catalog is taken to be empty so the popup is never held up by it.
fn mcpctl_catalog_with(program: &Path, timeout: std::time::Duration) -> Result<Vec<String>, String> {
    use std::io::Read;
    let mut child = std::process::Command::new(program)
        .args(["catalog", "list"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|err| format!("cannot run mcpctl: {:?}", err.kind()))?;
    let mut stdout = child.stdout.take().ok_or("cannot read mcpctl output")?;
    let (sender, received) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stdout.read_to_string(&mut text);
        let _ = sender.send(text);
    });
    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if std::time::Instant::now() < deadline => std::thread::sleep(std::time::Duration::from_millis(10)),
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let Some(status) = status else { return Ok(Vec::new()) };
    // The output is awaited only for what is left of the deadline: a process the listing started may hold
    // the pipe open long after the listing itself ended.
    let left = deadline.saturating_duration_since(std::time::Instant::now());
    let Ok(text) = received.recv_timeout(left) else { return Ok(Vec::new()) };
    if !status.success() {
        return Err("mcpctl catalog list failed".to_string());
    }
    Ok(parse_mcp_catalog(&text))
}

/// Starts a detached run of `feature` and, when a pane can follow its log, opens one.
fn launch_run(
repo_root: Option<PathBuf>,
state_root_override: Option<PathBuf>,
feature: String,
implementer: String,
reviewer: String,
    grid: Option<SharedGrid>,
) -> Result<String, String> {
    let repo = repo_root.as_deref().ok_or("the project is not known")?;
    let project = orq_git::resolve_project(repo).map_err(|err| format!("cannot resolve project: {err:?}"))?;
    let (profiles, reviewers) = load_model_config(&project.root)?;
    let request = orq_features::LaunchRequest {
        repo: project.root.clone(),
        feature: feature.clone(),
        remote: "origin".to_string(),
        target: configured_target_branch(&project.root)?,
        implementer: implementer.clone(),
        reviewer: reviewer.clone(),
        context: None,
        issue: None,
    };
    request.validate(&profiles, &reviewers).map_err(|err| err.to_string())?;
    // `orq run --detach` checks eligibility, starts the runner in a session of its own and
    // returns at once, so closing the popup never stops the run.
    let exe = std::env::current_exe().map_err(|err| format!("cannot find the orq executable: {err}"))?;
    let output = std::process::Command::new(exe)
        .args(request.args())
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|err| format!("cannot start orq run: {err}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let run_id = launch_outcome(output.status.success(), &stdout, &String::from_utf8_lossy(&output.stderr))?;
    // The run is already going; a pane is only a convenient view of its log.
    let pane = match (launch_log(&stdout), state_root_override.clone().or_else(pipeline::state_root)) {
        (Some(log), Some(root)) => {
            let workspace = std::env::var("HERDR_WORKSPACE_ID").ok().filter(|id| !id.is_empty());
            let mut guard = grid.as_ref().map(|grid| grid.lock().unwrap_or_else(|poisoned| poisoned.into_inner()));
            attach_pane_in(
                &orq_herdr::HerdrBinary,
                &orq_orchestrator::RunStore::new(&root),
                workspace.as_deref(),
                &project.root,
                &run_id,
                &log,
                REGISTRATION_WAIT,
                guard.as_deref_mut(),
            )
        }
        _ => Err("the run reported no log to follow".to_string()),
    };
    Ok(match pane {
        Ok(()) => run_id,
        Err(reason) => format!("{run_id} (no Herdr pane: {})", printable(&reason)),
    })
}

/// The grid tab the launches of one popup batch share.
type SharedGrid = std::sync::Arc<std::sync::Mutex<orq_herdr::GridTab>>;

/// A launch running on a thread of its own; `poll` reports once it has finished.
struct ThreadLaunch {
    result: std::sync::mpsc::Receiver<Result<String, String>>,
}

impl ThreadLaunch {
    fn spawn(work: impl FnOnce() -> Result<String, String> + Send + 'static) -> Self {
        let (sender, result) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(work());
        });
        ThreadLaunch { result }
    }

    /// Runs `works` one after another on a single thread, each reporting through its own job, so
    /// a failing launch does not stop the ones after it and they never race one another.
    fn spawn_in_sequence(works: Vec<Box<dyn FnOnce() -> Result<String, String> + Send>>) -> Vec<ThreadLaunch> {
        let mut senders = Vec::new();
        let mut jobs = Vec::new();
        for _ in &works {
            let (sender, result) = std::sync::mpsc::channel();
            senders.push(sender);
            jobs.push(ThreadLaunch { result });
        }
        std::thread::spawn(move || {
            for (work, sender) in works.into_iter().zip(senders) {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
                    .unwrap_or_else(|_| Err("the launch stopped unexpectedly".to_string()));
                let _ = sender.send(result);
            }
        });
        jobs
    }
}

impl orq_tui::LaunchJob for ThreadLaunch {
    fn poll(&mut self) -> Option<Result<String, String>> {
        match self.result.try_recv() {
            Ok(result) => Some(result),
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Some(Err("the launch stopped unexpectedly".to_string())),
        }
    }
}

impl LedgerBackend {
    fn sequenced_launches(
        &self,
        features: &[String],
        implementer: &str,
        reviewer: &str,
        grid: Option<SharedGrid>,
    ) -> Vec<Box<dyn orq_tui::LaunchJob>> {
        let works = features
            .iter()
            .map(|feature| {
                let repo_root = self.repo_root.clone();
                let state_root = self.state_root_override.clone();
                let grid = grid.clone();
                let (feature, implementer, reviewer) = (feature.clone(), implementer.to_string(), reviewer.to_string());
                Box::new(move || launch_run(repo_root, state_root, feature, implementer, reviewer, grid))
                    as Box<dyn FnOnce() -> Result<String, String> + Send>
            })
            .collect();
        ThreadLaunch::spawn_in_sequence(works)
            .into_iter()
            .map(|job| Box::new(job) as Box<dyn orq_tui::LaunchJob>)
            .collect()
    }
}

impl orq_tui::BrowserBackend for LedgerBackend {
    fn mcp_catalog(&mut self) -> Result<Vec<String>, String> {
        mcpctl_catalog()
    }

    fn save_mcp_servers(&mut self, servers: &[String]) -> Result<(), String> {
        let repo = self.repo_root.as_deref().ok_or("the project is not known")?;
        let project = orq_git::resolve_project(repo).map_err(|err| format!("cannot resolve project: {err:?}"))?;
        orq_orchestrator::save_mcp_servers_setting(&project.root, servers)
    }

    fn launch_profiles(&mut self) -> Result<Vec<String>, String> {
        let repo = self.repo_root.as_deref().ok_or("the project is not known")?;
        let project = orq_git::resolve_project(repo).map_err(|err| format!("cannot resolve project: {err:?}"))?;
        let (profiles, _) = load_model_config(&project.root)?;
        Ok(profiles.profiles().iter().map(|profile| profile.name.clone()).collect())
    }

    fn launch(&mut self, feature: &str, implementer: &str, reviewer: &str) -> Result<String, String> {
        launch_run(
            self.repo_root.clone(),
            self.state_root_override.clone(),
            feature.to_string(),
            implementer.to_string(),
            reviewer.to_string(),
            None,
        )
    }

    /// One job per feature, run one after another so the popup never races its own launches.
    fn start_launches(
        &mut self,
        features: &[String],
        implementer: &str,
        reviewer: &str,
    ) -> Vec<Box<dyn orq_tui::LaunchJob>> {
        self.sequenced_launches(features, implementer, reviewer, None)
    }

    /// As `start_launches`, with every run's log pane in one tab tiled as a golden spiral.
    fn start_grid_launches(
        &mut self,
        features: &[String],
        implementer: &str,
        reviewer: &str,
    ) -> Vec<Box<dyn orq_tui::LaunchJob>> {
        let grid = std::sync::Arc::new(std::sync::Mutex::new(orq_herdr::GridTab::new(&format!(
            "feats: {}",
            features.join(", ")
        ))));
        self.sequenced_launches(features, implementer, reviewer, Some(grid))
    }

    /// The launch waits for the run to register its Herdr pane, so it runs on a thread and the
    /// popup keeps taking keys until it reports.
    fn start_launch(&mut self, feature: &str, implementer: &str, reviewer: &str) -> Box<dyn orq_tui::LaunchJob> {
        let repo_root = self.repo_root.clone();
        let state_root = self.state_root_override.clone();
        let (feature, implementer, reviewer) = (feature.to_string(), implementer.to_string(), reviewer.to_string());
        Box::new(ThreadLaunch::spawn(move || launch_run(repo_root, state_root, feature, implementer, reviewer, None)))
    }

    fn focus_run_pane(&mut self, run_id: &str) -> orq_tui::PaneFocus {
        match self.state_root_override.clone().or_else(pipeline::state_root) {
            Some(root) => focus_run(&orq_herdr::HerdrBinary, &orq_orchestrator::RunStore::new(&root), run_id),
            None => orq_tui::PaneFocus::Failed(
                "cannot locate the Orq state directory: neither XDG_STATE_HOME nor HOME is set".to_string(),
            ),
        }
    }

    fn pause_run(&mut self, run_id: &str) -> Result<String, String> {
        let root = self.state_root_override.clone().or_else(pipeline::state_root).ok_or("cannot locate the Orq state directory")?;
        orq_orchestrator::request_pause(&orq_orchestrator::RunStore::new(&root), run_id)?;
        Ok(format!("Pause requested for {run_id}; it stops at the next stage boundary"))
    }

    fn resume_check(&mut self, run_id: &str) -> Result<String, String> {
        let root = self.state_root_override.clone().or_else(pipeline::state_root).ok_or("cannot locate the Orq state directory")?;
        let repo = self.repo_root.as_deref().ok_or("the project is not known")?;
        let store = orq_orchestrator::RunStore::new(&root);
        // A run whose process is gone is a failed run, which resumes where it stopped.
        orq_orchestrator::reconcile_interrupted(&store, run_id, &orq_orchestrator::process_is_alive).map_err(|err| format!("cannot read the run: {err:?}"))?;
        let stopped = store
            .events(run_id)
            .map(|events| matches!(orq_orchestrator::summarize_run(&events).phase, orq_orchestrator::RunPhase::Failed | orq_orchestrator::RunPhase::Blocked))
            .unwrap_or(false);
        if stopped {
            // The pipeline continues the latest run of these features, so that is what is checked.
            check_group_resume(&store, repo, run_id)?;
            return Ok("where it stopped".to_string());
        }
        let checkpoint = orq_orchestrator::validate_checkpoint(&store, repo, run_id)?;
        Ok(printable(checkpoint.next_stage.as_deref().unwrap_or("the end")))
    }

    fn switch_model(&mut self, run_id: &str, role: &str, profile: &str) -> Result<String, String> {
        let repo = self.repo_root.as_deref().ok_or("the project is not known")?;
        // The command checks the profile exists and that the run is paused.
        let exe = std::env::current_exe().map_err(|err| format!("cannot find the orq executable: {err}"))?;
        let output = std::process::Command::new(exe)
            .args(["run", "switch", run_id, "--role", role, "--profile", profile, "--repo"])
            .arg(repo)
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|err| format!("cannot start orq run switch: {err}"))?;
        if output.status.success() {
            Ok(format!("Switch of {role} to {profile} recorded for {run_id}; resume to apply it"))
        } else {
            Err(printable(String::from_utf8_lossy(&output.stderr).trim()))
        }
    }

    fn resume_run(&mut self, run_id: &str) -> Result<String, String> {
        let repo = self.repo_root.as_deref().ok_or("the project is not known")?;
        // The command validates again, so a checkpoint that went stale since the check is refused.
        let exe = std::env::current_exe().map_err(|err| format!("cannot find the orq executable: {err}"))?;
        let output = std::process::Command::new(exe)
            .args(["run", "resume", run_id, "--yes", "--detach", "--repo"])
            .arg(repo)
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|err| format!("cannot start orq run resume: {err}"))?;
        if output.status.success() {
            Ok(format!("Resumed {run_id}"))
        } else {
            Err(printable(String::from_utf8_lossy(&output.stderr).trim()))
        }
    }

    fn open_review_link(&mut self, run_id: &str) -> Result<String, String> {
        self.review_link(run_id, &open_url)
    }

    fn copy_review_link(&mut self, run_id: &str) -> Result<String, String> {
        self.review_link(run_id, &copy_text)
    }

    fn reopen_run_pane(&mut self, run_id: &str) -> orq_tui::PaneFocus {
        let Some(repo) = self.repo_root.as_deref() else {
            return orq_tui::PaneFocus::Failed("the project is not known".to_string());
        };
        let project = match orq_git::resolve_project(repo) {
            Ok(project) => project,
            Err(err) => return orq_tui::PaneFocus::Failed(format!("cannot resolve project: {err:?}")),
        };
        let Some(root) = self.state_root_override.clone().or_else(pipeline::state_root) else {
            return orq_tui::PaneFocus::Failed(
                "cannot locate the Orq state directory: neither XDG_STATE_HOME nor HOME is set".to_string(),
            );
        };
        let workspace = std::env::var("HERDR_WORKSPACE_ID").ok().filter(|id| !id.is_empty());
        reopen_run(
            &orq_herdr::HerdrBinary,
            &orq_orchestrator::RunStore::new(&root),
            workspace.as_deref(),
            &project.root,
            &root,
            run_id,
        )
    }

    fn load_reviews(&mut self) -> Result<Vec<orq_tui::ReviewRow>, String> {
        let repo = self.repo_root.as_deref().ok_or("the project is not known")?;
        let project = orq_git::resolve_project(repo).map_err(|err| format!("cannot resolve project: {err:?}"))?;
        let roots = review_roots()?;
        let rows = orq_core::review_inbox::inbox(&roots, &pipeline::project_id(&project), &project.name)?;
        Ok(rows
            .into_iter()
            .map(|r| orq_tui::ReviewRow { run_id: r.run_id, feature: r.feature, project: r.project, status: r.status, risk: r.risk, current: r.current, stale: r.stale })
            .collect())
    }

    fn load_review_detail(&mut self, run_id: &str) -> Result<orq_tui::ReviewDetail, String> {
        let d = orq_core::review_inbox::detail(&review_roots()?, run_id)?;
        Ok(orq_tui::ReviewDetail { summary: d.summary, risks: d.risks, files: d.files, tests: d.tests, rounds: d.rounds })
    }

    fn load_review_detail_with_links(&mut self, run_id: &str) -> Result<(orq_tui::ReviewDetail, Vec<Option<String>>), String> {
        let (d, links) = orq_core::review_inbox::detail_with_links(&review_roots()?, run_id)?;
        Ok((orq_tui::ReviewDetail { summary: d.summary, risks: d.risks, files: d.files, tests: d.tests, rounds: d.rounds }, links))
    }

    fn open_url(&mut self, url: &str) -> Result<String, String> {
        open_url(url)
    }

    fn copy_url(&mut self, url: &str) -> Result<String, String> {
        copy_text(url)
    }

    fn unreadable_runs(&mut self) -> usize {
        self.unreadable_runs
    }

    fn load_runs(&mut self) -> Result<Vec<orq_tui::RunRow>, String> {
        let repo = self.repo_root.as_deref().ok_or("the project is not known")?;
        let project = orq_git::resolve_project(repo).map_err(|err| format!("cannot resolve project: {err:?}"))?;
        let root = self
            .state_root_override
            .clone()
            .or_else(pipeline::state_root)
            .ok_or("cannot locate the Orq state directory: neither XDG_STATE_HOME nor HOME is set")?;
        let store = orq_orchestrator::RunStore::new(&root);
        let list = list_runs_reconciled(&store, &pipeline::project_id(&project))?;
        self.unreadable_runs = list.unreadable;
        Ok(list
            .runs
            .into_iter()
            .map(|run| orq_tui::RunRow {
                run_id: run.run_id,
                feature: run.feature_id,
                status: run.phase.as_str().to_string(),
                stage: run.stage,
                implementer_model: run.implementer_model,
                reviewer_model: run.reviewer_model,
                has_review_link: run.review_url.is_some(),
            })
            .collect())
    }

    fn load(&mut self) -> Result<Vec<orq_tui::BrowserEntry>, orq_tui::LedgerErrorView> {
        match orq_features::LoadedLedger::load(&self.path) {
            Ok(loaded) => {
                let entries = loaded
                    .entries()
                    .iter()
                    .map(|entry| orq_tui::BrowserEntry {
                        name: entry.feature_name.clone(),
                        description: entry.description.clone(),
                        status: status_text(&entry.status).to_string(),
                        depends_on: entry.depends_on.clone(),
                        test_file: entry.test_file.clone(),
                    })
                    .collect();
                self.loaded = Some(loaded);
                self.absent = false;
                Ok(entries)
            }
            Err(err) => {
                self.absent = matches!(err, orq_features::LedgerError::Missing { .. });
                // An absent file is snapshotted as "absent", so a file that
                // appears before the first add is a conflict, not an append.
                self.loaded = self.absent.then(|| orq_features::LoadedLedger::absent(&self.path));
                Err(orq_tui::LedgerErrorView {
                    path: self.path.to_string_lossy().into_owned(),
                    reason: ledger_error_text(&err),
                })
            }
        }
    }

    fn ledger_absent(&self) -> bool {
        self.absent
    }

    fn promote(&mut self, feature: &str) -> orq_tui::PromoteOutcome {
        let Some(loaded) = &self.loaded else {
            return orq_tui::PromoteOutcome::Failed { reason: "ledger is not loaded".to_string() };
        };
        match loaded.promote(feature, None) {
            Ok(_) => {
                // Our own write made the snapshot stale; re-snapshot so the next
                // promote in a bulk run is not a conflict with ourselves.
                self.loaded = orq_features::LoadedLedger::load(&self.path).ok();
                orq_tui::PromoteOutcome::Saved
            }
            Err(err) => orq_tui::PromoteOutcome::Failed { reason: promote_error_text(&err, feature) },
        }
    }

    fn feature_fields(&mut self, name: &str) -> Option<orq_tui::FeatureFields> {
        let entry = self.loaded.as_ref()?.entries().iter().find(|e| e.feature_name == name)?;
        Some(orq_tui::FeatureFields {
            name: entry.feature_name.clone(),
            description: entry.description.clone(),
            depends_on: entry.depends_on.clone(),
            parallelizable: entry.parallelizable,
            test_file: entry.test_file.clone(),
            target_files: entry.target_files.clone(),
        })
    }

    fn edit_feature(&mut self, fields: &orq_tui::FeatureFields) -> orq_tui::FormSaveOutcome {
        use orq_features::{EditFieldsError, SaveError};
        use orq_tui::FormSaveOutcome;
        let Some(loaded) = self.loaded.take() else {
            return FormSaveOutcome::Failed { reason: "ledger is not loaded".to_string() };
        };
        let Some(current) = loaded.entries().iter().find(|e| e.feature_name == fields.name).cloned() else {
            self.loaded = Some(loaded);
            return FormSaveOutcome::Failed { reason: format!("no feature named {}", fields.name) };
        };
        // Only what differs is sent; blank text leaves a field as it is.
        let changed_text = |new: &Option<String>, old: &Option<String>| match new {
            Some(value) if new != old => Some(value.clone()),
            _ => None,
        };
        let edits = orq_features::FieldEdits {
            description: changed_text(&fields.description, &current.description),
            depends_on: (fields.depends_on != current.depends_on).then(|| fields.depends_on.clone()),
            parallelizable: match fields.parallelizable {
                Some(value) if Some(value) != current.parallelizable => Some(value),
                _ => None,
            },
            test_file: changed_text(&fields.test_file, &current.test_file),
            target_files: (!fields.target_files.is_empty() && fields.target_files != current.target_files)
                .then(|| fields.target_files.clone()),
        };
        let outcome = match loaded.edit_fields(&fields.name, &edits) {
            Ok(_) => FormSaveOutcome::Saved,
            Err(err @ EditFieldsError::Invalid(_)) => FormSaveOutcome::Invalid { reason: err.to_string() },
            Err(err @ EditFieldsError::Save(SaveError::Conflict)) => {
                FormSaveOutcome::Conflict { reason: err.to_string() }
            }
            Err(err) => FormSaveOutcome::Failed { reason: err.to_string() },
        };
        // As for add: a refused save keeps its snapshot, anything else
        // reloads against the file as it now is.
        self.loaded = match outcome {
            FormSaveOutcome::Invalid { .. } | FormSaveOutcome::Failed { .. } => Some(loaded),
            _ => orq_features::LoadedLedger::load(&self.path).ok(),
        };
        outcome
    }

    fn add_feature(&mut self, fields: &orq_tui::FeatureFields) -> orq_tui::FormSaveOutcome {
        use orq_features::{AppendError, SaveError};
        use orq_tui::FormSaveOutcome;
        // Appended against the snapshot the popup showed, so a file changed
        // behind its back is a conflict; an absent file is created.
        let loaded = match self.loaded.take() {
            Some(loaded) => loaded,
            None => match orq_features::LoadedLedger::load_or_new(&self.path) {
                Ok(loaded) => loaded,
                Err(err) => return FormSaveOutcome::Failed { reason: ledger_error_text(&err) },
            },
        };
        let draft = orq_features::FeatureDraft {
            feature_name: fields.name.clone(),
            description: fields.description.clone(),
            depends_on: (!fields.depends_on.is_empty()).then(|| fields.depends_on.clone()),
            parallelizable: fields.parallelizable,
            test_file: fields.test_file.clone(),
            target_files: (!fields.target_files.is_empty()).then(|| fields.target_files.clone()),
        };
        let outcome = match loaded.append(&draft) {
            Ok(_) => FormSaveOutcome::Saved,
            Err(err @ AppendError::Invalid(_)) => FormSaveOutcome::Invalid { reason: err.to_string() },
            Err(err @ AppendError::Save(SaveError::Conflict)) => {
                FormSaveOutcome::Conflict { reason: err.to_string() }
            }
            Err(err) => FormSaveOutcome::Failed { reason: err.to_string() },
        };
        // A refused save wrote nothing, so the snapshot still stands and a
        // retry is checked against it; after a write or a conflict the
        // browser reloads, so take a fresh snapshot of the file as it is.
        self.loaded = match outcome {
            FormSaveOutcome::Invalid { .. } | FormSaveOutcome::Failed { .. } => Some(loaded),
            _ => orq_features::LoadedLedger::load(&self.path).ok(),
        };
        outcome
    }

    fn generate_candidates(&mut self) -> Result<Vec<orq_tui::CandidateView>, String> {
        self.staged.lock().unwrap().clear();
        let reviewed = generate_reviewed(
            &self.path,
            self.spec_root(),
            self.settings_file().map(Path::to_path_buf),
            self.program_override.clone(),
            self.log_dir(),
            &std::sync::atomic::AtomicBool::new(false),
            None,
        )?;
        let views = candidate_views(&reviewed);
        *self.staged.lock().unwrap() = reviewed;
        Ok(views)
    }

    fn spec_files(&mut self) -> Vec<String> {
        match self.spec_root() {
            Some(root) => orq_features::spec_file_choices(&root),
            None => Vec::new(),
        }
    }

    fn start_generation_for(&mut self, spec: &str, focus: &str) -> Result<Box<dyn orq_tui::GenerationJob>, String> {
        self.spawn_generation(Some((spec.to_string(), focus.to_string())))
    }

    fn start_generation(&mut self) -> Result<Box<dyn orq_tui::GenerationJob>, String> {
        self.spawn_generation(None)
    }

    fn accept_candidates(&mut self, accepted: &[orq_tui::AcceptedCandidate]) -> orq_tui::FormSaveOutcome {
        use orq_features::{AppendError, SaveError, Verdict};
        use orq_tui::FormSaveOutcome;
        let mut drafts = Vec::new();
        for pick in accepted {
            let staged_all = self.staged.lock().unwrap();
            let staged = staged_all
                .iter()
                .find(|r| r.verdict == Verdict::New && r.candidate.draft.feature_name == pick.name);
            let Some(staged) = staged else {
                return FormSaveOutcome::Failed { reason: format!("{} is not a new generated candidate", pick.name) };
            };
            let mut draft = staged.candidate.draft.clone();
            let suggests = staged.suggests_parallel;
            drop(staged_all);
            // Only a kept suggestion is written; anything else stays absent.
            draft.parallelizable = pick.parallelizable.filter(|_| suggests);
            drafts.push(draft);
        }
        let loaded = match self.loaded.take() {
            Some(loaded) => loaded,
            None => match orq_features::LoadedLedger::load_or_new(&self.path) {
                Ok(loaded) => loaded,
                Err(err) => return FormSaveOutcome::Failed { reason: ledger_error_text(&err) },
            },
        };
        let outcome = match loaded.append_many(&drafts) {
            Ok(_) => FormSaveOutcome::Saved,
            Err(err @ AppendError::Invalid(_)) => FormSaveOutcome::Invalid { reason: err.to_string() },
            Err(err @ AppendError::Save(SaveError::Conflict)) => {
                FormSaveOutcome::Conflict { reason: err.to_string() }
            }
            Err(err) => FormSaveOutcome::Failed { reason: err.to_string() },
        };
        self.loaded = match outcome {
            FormSaveOutcome::Invalid { .. } | FormSaveOutcome::Failed { .. } => Some(loaded),
            _ => {
                self.staged.lock().unwrap().clear();
                orq_features::LoadedLedger::load(&self.path).ok()
            }
        };
        outcome
    }

    fn harness_options(&mut self) -> Vec<orq_tui::HarnessOption> {
        // Discovery can start a process, so it runs once per backend.
        if let Some(cached) = &self.harness_cache {
            return cached.clone();
        }
        let find = |name: &str| match &self.harness_programs {
            Some(programs) => programs.iter().find(|(n, _)| n == name).map(|(_, p)| p.clone()),
            None => find_program(name),
        };
        let mut options = Vec::new();
        if find("claude").is_some() {
            // claude lists no models; these two aliases are offered, free text stays.
            options.push(orq_tui::HarnessOption {
                harness: "claude".into(),
                models: vec!["opus".into(), "sonnet".into()],
            });
        }
        if let Some(program) = find("pi") {
            let models = list_pi_models(&program, std::time::Duration::from_secs(5));
            options.push(orq_tui::HarnessOption { harness: "pi".into(), models });
        }
        self.harness_cache = Some(options.clone());
        options
    }

    fn load_storage(&mut self) -> orq_tui::StorageView {
        match storage::open_ctx() {
            Ok(ctx) => storage::ui_view(&ctx),
            Err(error) => orq_tui::StorageView { error: Some(error), ..Default::default() },
        }
    }

    fn preview_storage(&mut self) -> Result<Vec<String>, String> {
        let (lines, plan) = storage::ui_preview(&storage::open_ctx()?)?;
        self.previewed_cleanup = Some(plan);
        Ok(lines)
    }

    fn prune_storage(&mut self) -> Result<Vec<String>, String> {
        // Only what was previewed may be removed; a new preview is needed to add anything.
        let plan = self.previewed_cleanup.take().ok_or_else(|| "preview the cleanup first".to_string())?;
        storage::ui_prune(&storage::open_ctx()?, &plan)
    }

    fn load_settings(&mut self) -> orq_tui::SettingsView {
        let failed = |error: String| orq_tui::SettingsView { error: Some(error), ..Default::default() };
        let path = match self.settings_file() {
            Ok(path) => path,
            Err(error) => return failed(error),
        };
        let settings = match orq_features::Settings::load(path) {
            Ok(settings) => settings,
            Err(error) => {
                self.settings_snapshot = None;
                return failed(error.to_string());
            }
        };
        let roles = (
            settings.agent(orq_features::AgentRole::Implementer),
            settings.agent(orq_features::AgentRole::Reviewer),
        );
        self.settings_snapshot = Some(settings);
        let (implementer, reviewer) = match roles {
            (Ok(implementer), Ok(reviewer)) => (implementer, reviewer),
            (Err(error), _) | (_, Err(error)) => return failed(error.to_string()),
        };
        let mut view = orq_tui::SettingsView {
            implementer: agent_setting(implementer),
            reviewer: agent_setting(reviewer.clone()),
            ..Default::default()
        };
        // .tdd-config.json stays authoritative for the reviewer; it is only read.
        let config_path = self.path.with_file_name(".tdd-config.json");
        match orq_features::load_reviewer_config(&config_path) {
            Ok(config) => {
                let resolved = orq_features::resolve_reviewer(reviewer.as_ref(), &config);
                if let (true, orq_features::EffectiveReviewer::Config(p)) =
                    (resolved.overrides_settings, &resolved.effective)
                {
                    view.reviewer_override = Some(format!("{} / {}", p.harness, p.model));
                }
                view.error = resolved.invalid;
            }
            Err(error) => view.error = Some(error.to_string()),
        }
        view
    }

    fn save_settings(
        &mut self,
        implementer: &orq_tui::AgentSetting,
        reviewer: &orq_tui::AgentSetting,
    ) -> Result<(), String> {
        // Work on the stored snapshot in place so no error path drops it: the
        // baseline it was loaded against stays, and an outside edit is still
        // a conflict on the next save.
        if self.settings_snapshot.is_none() {
            let path = self.settings_file()?;
            self.settings_snapshot = Some(orq_features::Settings::load(path).map_err(|e| e.to_string())?);
        }
        let settings = self.settings_snapshot.as_mut().expect("snapshot was just ensured");
        let roles = [
            (orq_features::AgentRole::Implementer, implementer),
            (orq_features::AgentRole::Reviewer, reviewer),
        ];
        for (role, value) in roles {
            // A role left empty stays unset rather than being written blank.
            if value.harness.trim().is_empty() && value.model.trim().is_empty() {
                continue;
            }
            settings.set_agent(role, &value.harness, &value.model).map_err(|e| e.to_string())?;
        }
        // The snapshot is kept even after a conflict: it stays stale until the
        // tab reloads, so the same stale values are never written later.
        settings.save().map_err(|error| match error {
            orq_features::SettingsError::Conflict { .. } => {
                "config.toml changed since the Settings tab was loaded; nothing was written, reopen the tab to reload"
                    .to_string()
            }
            other => other.to_string(),
        })
    }
}

/// What the popup should show for `cwd`: its project, or why there is none.
fn view_for(cwd: &Path) -> Result<PopupView, ExitCode> {
    match orq_git::resolve_project(cwd) {
        Ok(project) => Ok(PopupView::Project(PopupModel {
            project_name: project.name,
            repo_path: project.root.to_string_lossy().into_owned(),
        })),
        Err(ResolveError::NotARepository { path }) => Ok(PopupView::NotARepository {
            path: path.to_string_lossy().into_owned(),
        }),
        Err(err) => Err(fail(format!("cannot resolve project: {err:?}"))),
    }
}

fn path_str(path: &Path) -> Option<&str> {
    path.to_str()
}

#[cfg(test)]
mod ledger_backend_tests {
    use orq_tui::{BrowserBackend, PromoteOutcome};

    use super::LedgerBackend;

    fn ledger(names: &[&str]) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("orq-backend-{}-{}", std::process::id(), names.join("-")));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".tdd-features.json");
        let entries: Vec<String> =
            names.iter().map(|n| format!(r#"{{"featureName":"{n}","status":"draft"}}"#)).collect();
        std::fs::write(&path, format!("[{}]", entries.join(","))).unwrap();
        (dir, path)
    }

    #[test]
    fn pi_models_table_becomes_provider_slash_model_profiles() {
        let table = "provider  model  context  max-out  thinking  images\n\
                     claude-bridge  claude-fable-5  1M  64k  yes  yes\n\
                     openai  gpt-6-astra  400k  128k  yes  no\n\
                     broken\n";

        assert_eq!(
            super::parse_pi_models(table),
            vec!["claude-bridge/claude-fable-5".to_string(), "openai/gpt-6-astra".to_string()]
        );
    }

    const TABLE: &str = "provider model\nopenai gpt-6-astra\n";

    #[test]
    fn a_hanging_model_listing_is_bounded_and_yields_no_models() {
        let agent = orq_test_support::FakeAgent::builder().stdout(TABLE).hang().build();
        let started = std::time::Instant::now();

        let models = super::list_pi_models(agent.path(), std::time::Duration::from_millis(300));

        assert!(models.is_empty(), "{models:?}");
        assert!(started.elapsed() < std::time::Duration::from_secs(4), "{:?}", started.elapsed());
    }

    #[test]
    fn a_failed_model_listing_offers_no_models() {
        let agent = orq_test_support::FakeAgent::builder().stdout(TABLE).exit_code(2).build();

        let models = super::list_pi_models(agent.path(), std::time::Duration::from_secs(5));

        assert!(models.is_empty(), "{models:?}");
    }

    #[test]
    fn a_read_error_discards_the_partial_listing() {
        struct Broken(bool);
        impl std::io::Read for Broken {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                if self.0 {
                    return Err(std::io::Error::other("pipe broke"));
                }
                self.0 = true;
                let chunk = &TABLE.as_bytes()[..buf.len().min(TABLE.len())];
                buf[..chunk.len()].copy_from_slice(chunk);
                Ok(chunk.len())
            }
        }

        assert_eq!(super::read_capped(Broken(false), 1024), None);
    }

    #[test]
    fn a_successful_model_listing_is_returned() {
        let agent = orq_test_support::FakeAgent::builder().stdout(TABLE).build();

        let models = super::list_pi_models(agent.path(), std::time::Duration::from_secs(5));

        assert_eq!(models, vec!["openai/gpt-6-astra".to_string()]);
    }

    #[test]
    fn harness_options_are_discovered_once_per_backend() {
        let (_dir, path) = ledger(&["discovery"]);
        let agent = orq_test_support::FakeAgent::builder().stdout(TABLE).build();
        let mut backend = LedgerBackend::new(path);
        backend.harness_programs = Some(vec![("pi".into(), agent.path().to_path_buf())]);
        let first = backend.harness_options();

        backend.harness_programs = Some(vec![]);
        let second = backend.harness_options();

        assert_eq!(first.len(), 1);
        assert_eq!(first, second);
    }

    #[test]
    fn generated_candidates_are_reviewed_and_accepted_in_one_save() {
        use orq_tui::{AcceptedCandidate, CandidateKind, FormSaveOutcome};
        let (dir, path) = ledger(&["base"]);
        std::fs::write(dir.join("SPEC.md"), "# spec\n").unwrap();
        let output = r#"[{"featureName":"base"},{"featureName":"a","parallelizable":true},{"featureName":"b","dependsOn":["a"]},{"featureName":"c","dependsOn":["ghost"]}]"#;
        let agent = orq_test_support::FakeAgent::builder().stdout(output).build();
        let mut backend = LedgerBackend::new(path.clone());
        backend.settings_path = Some(dir.join("cfg").join("orq").join("config.toml"));
        backend.program_override = Some(agent.path().to_path_buf());
        let none = orq_tui::AgentSetting::default();
        let writer = orq_tui::AgentSetting { harness: "claude".into(), model: "writer".into() };
        backend.save_settings(&writer, &none).unwrap();
        // The cache is chosen from XDG_CACHE_HOME or HOME; the test only
        // needs the agent to run, so it relies on whichever is set.
        let views = match backend.generate_candidates() {
            Ok(views) => views,
            Err(reason) => panic!("generation failed: {reason}"),
        };
        assert_eq!(views.len(), 4);
        assert!(matches!(views[0].kind, CandidateKind::Duplicate(_)));
        assert_eq!(views[1].kind, CandidateKind::New);
        assert!(views[1].suggests_parallel);
        assert!(matches!(views[3].kind, CandidateKind::Invalid(_)));
        let before = std::fs::read_to_string(&path).unwrap();

        let refused = backend.accept_candidates(&[AcceptedCandidate { name: "c".into(), parallelizable: None }]);
        assert!(matches!(refused, FormSaveOutcome::Failed { .. }));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);

        let saved = backend.accept_candidates(&[
            AcceptedCandidate { name: "a".into(), parallelizable: Some(true) },
            AcceptedCandidate { name: "b".into(), parallelizable: None },
        ]);
        assert!(matches!(saved, FormSaveOutcome::Saved));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(r#""featureName":"a""#) && text.contains(r#""parallelizable":true"#));
        assert_eq!(orq_features::load_ledger(&path).unwrap().len(), 3);
    }

    #[test]
    fn a_chosen_spec_and_section_reach_the_agent_and_outside_paths_are_refused() {
        let (dir, path) = ledger(&["spec-choice"]);
        std::fs::write(dir.join("SPEC.md"), "# spec\n").unwrap();
        std::fs::create_dir_all(dir.join("docs")).unwrap();
        std::fs::write(dir.join("docs/mvp2.md"), "# mvp2\n").unwrap();
        let agent = orq_test_support::FakeAgent::builder().stdout(r#"[{"featureName":"one"}]"#).build();
        let mut backend = LedgerBackend::new(path.clone());
        backend.settings_path = Some(dir.join("cfg").join("orq").join("config.toml"));
        backend.program_override = Some(agent.path().to_path_buf());
        backend.log_dir_override = Some(dir.join("logs"));
        let none = orq_tui::AgentSetting::default();
        let writer = orq_tui::AgentSetting { harness: "claude".into(), model: "writer".into() };
        backend.save_settings(&writer, &none).unwrap();

        assert_eq!(backend.spec_files(), ["SPEC.md", "docs/mvp2.md"]);
        let mut job = backend.start_generation_for("docs/mvp2.md", "MVP2").unwrap();
        let result = loop {
            if let Some(result) = job.poll() {
                break result;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert!(result.is_ok(), "{result:?}");
        let run = agent.last_run().unwrap();
        assert!(run.args.iter().any(|a| a.contains("docs/mvp2.md") && a.contains("\"MVP2\"")), "{:?}", run.args);

        let mut refused = backend.start_generation_for("../outside.md", "").unwrap();
        let result = loop {
            if let Some(result) = refused.poll() {
                break result;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert!(result.is_err());
    }

    #[test]
    fn specs_come_from_the_working_tree_when_the_ledger_lives_elsewhere() {
        let (dir, path) = ledger(&["elsewhere"]);
        let work = dir.join("work");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join("SPEC.md"), "# spec\n").unwrap();
        let agent = orq_test_support::FakeAgent::builder().stdout(r#"[{"featureName":"one"}]"#).build();
        let mut backend = LedgerBackend::new(path);
        backend.repo_root = Some(work.clone());
        backend.settings_path = Some(dir.join("cfg").join("orq").join("config.toml"));
        backend.program_override = Some(agent.path().to_path_buf());
        backend.log_dir_override = Some(dir.join("logs"));
        let none = orq_tui::AgentSetting::default();
        let writer = orq_tui::AgentSetting { harness: "claude".into(), model: "writer".into() };
        backend.save_settings(&writer, &none).unwrap();

        assert_eq!(backend.spec_files(), ["SPEC.md"]);
        let mut job = backend.start_generation_for("SPEC.md", "").unwrap();
        let result = loop {
            if let Some(result) = job.poll() {
                break result;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(agent.last_run().unwrap().cwd, work.canonicalize().unwrap_or(work));
    }

    fn run_to_end(backend: &mut LedgerBackend) -> Result<Vec<orq_tui::CandidateView>, String> {
        let mut job = backend.start_generation_for("SPEC.md", "MVP2").unwrap();
        loop {
            if let Some(result) = job.poll() {
                return result;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    fn report_backend(
        label: &str,
        agent: &orq_test_support::FakeAgent,
    ) -> (LedgerBackend, std::path::PathBuf) {
        let (dir, path) = ledger(&[label]);
        std::fs::write(dir.join("SPEC.md"), "# spec\n").unwrap();
        let logs = dir.join("logs");
        let mut backend = LedgerBackend::new(path);
        backend.settings_path = Some(dir.join("cfg").join("orq").join("config.toml"));
        backend.program_override = Some(agent.path().to_path_buf());
        backend.log_dir_override = Some(logs.clone());
        let none = orq_tui::AgentSetting::default();
        let writer = orq_tui::AgentSetting { harness: "claude".into(), model: "writer".into() };
        backend.save_settings(&writer, &none).unwrap();
        (backend, logs)
    }

    fn reported_path(message: &str) -> std::path::PathBuf {
        let (_, tail) = message.split_once("details: ").unwrap_or_else(|| panic!("no details in {message}"));
        std::path::PathBuf::from(tail.trim_end_matches(')').trim())
    }

    #[test]
    fn generation_report_is_written_and_named_when_the_agent_fails() {
        let agent = orq_test_support::FakeAgent::builder()
            .stderr("Not logged in\nANTHROPIC_API_KEY=sk-ant-abc123def456ghi789\n")
            .exit_code(1)
            .build();
        let (mut backend, logs) = report_backend("report-fail", &agent);

        let message = run_to_end(&mut backend).unwrap_err();

        assert!(message.contains("the agent exited with status 1"), "{message}");
        let path = reported_path(&message);
        assert!(path.starts_with(&logs), "{path:?}");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("Not logged in"), "{text}");
        assert!(text.contains("MVP2"), "{text}");
        assert!(!text.contains("sk-ant-abc123def456ghi789"), "{text}");
    }

    #[test]
    fn generation_report_explains_zero_features() {
        let agent = orq_test_support::FakeAgent::builder().stdout("[]").stderr("model said nothing\n").build();
        let (mut backend, _logs) = report_backend("report-zero", &agent);

        let message = run_to_end(&mut backend).unwrap_err();

        assert!(message.contains("the agent returned 0 features"), "{message}");
        let text = std::fs::read_to_string(reported_path(&message)).unwrap();
        assert!(text.contains("model said nothing"), "{text}");
    }

    #[test]
    fn generation_report_is_written_when_the_output_is_not_json() {
        let agent = orq_test_support::FakeAgent::builder().stdout("I could not read the file").build();
        let (mut backend, _logs) = report_backend("report-garbled", &agent);

        let message = run_to_end(&mut backend).unwrap_err();

        let text = std::fs::read_to_string(reported_path(&message)).unwrap();
        assert!(text.contains("I could not read the file"), "{text}");
    }

    fn failing_settings(
        label: &str,
        log_dir: Option<std::path::PathBuf>,
        cancelled: bool,
    ) -> Result<Vec<orq_features::ReviewedCandidate>, String> {
        let (_dir, path) = ledger(&[label]);
        let cancel = std::sync::atomic::AtomicBool::new(cancelled);
        super::generate_reviewed(
            &path,
            None,
            Err("settings are unreadable".to_string()),
            None,
            log_dir,
            &cancel,
            Some(("SPEC.md", "")),
        )
    }

    #[test]
    fn a_failure_without_a_log_location_keeps_the_error_and_says_so() {
        let message = failing_settings("report-no-dir", None, false).unwrap_err();

        assert!(message.contains("no report location"), "{message}");
        assert!(!message.starts_with(" ("), "the original error must lead: {message}");
        assert!(!message.contains("details:"), "{message}");
    }

    #[test]
    fn an_unwritable_report_location_keeps_the_original_error() {
        let (dir, _) = ledger(&["report-occupied"]);
        let occupied = dir.join("logs");
        std::fs::write(&occupied, "a regular file, not a directory").unwrap();

        let message = failing_settings("report-occupied-run", Some(occupied.clone()), false).unwrap_err();

        let original = failing_settings("report-original", None, false).unwrap_err();
        let original = original.split(" (no report location").next().unwrap();
        assert!(message.starts_with(original), "{message} vs {original}");
        assert!(message.contains("could not write a report"), "{message}");
        assert!(occupied.is_file());
    }

    #[test]
    fn a_cancelled_generation_writes_no_report() {
        let (dir, _) = ledger(&["report-cancelled"]);
        let logs = dir.join("logs");

        let message = failing_settings("report-cancelled-run", Some(logs.clone()), true).unwrap_err();

        assert!(!message.contains("details:"), "{message}");
        assert!(!logs.exists(), "a cancelled run must not create the report directory");
    }

    fn worker(
        receiver: std::sync::mpsc::Receiver<Result<Vec<orq_features::ReviewedCandidate>, String>>,
        cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
        handle: Option<std::thread::JoinHandle<()>>,
    ) -> super::WorkerJob {
        super::WorkerJob { receiver, cancel, staged: Default::default(), handle, done: false }
    }

    #[test]
    fn a_finished_worker_job_yields_its_result_once() {
        use orq_tui::GenerationJob;
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut job = worker(receiver, Default::default(), None);
        sender.send(Err("agent failed".to_string())).unwrap();
        drop(sender);

        assert!(matches!(job.poll(), Some(Err(reason)) if reason == "agent failed"));
        assert!(job.poll().is_none());
    }

    #[test]
    fn dropping_a_worker_job_cancels_and_waits_for_the_worker() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        let cancel = Arc::new(AtomicBool::new(false));
        let finished = Arc::new(AtomicBool::new(false));
        let (flag, done) = (Arc::clone(&cancel), Arc::clone(&finished));
        let handle = std::thread::spawn(move || {
            while !flag.load(Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            // Stands in for the agent's process group being killed.
            std::thread::sleep(std::time::Duration::from_millis(50));
            done.store(true, Ordering::SeqCst);
        });
        let (_sender, receiver) = std::sync::mpsc::channel();

        drop(worker(receiver, cancel, Some(handle)));

        assert!(finished.load(Ordering::SeqCst), "the worker was still running after the job was dropped");
    }

    #[test]
    fn settings_round_trip_through_the_backend_and_a_half_filled_role_is_refused() {
        let (dir, path) = ledger(&["s"]);
        let mut backend = LedgerBackend::new(path);
        backend.settings_path = Some(dir.join("cfg").join("orq").join("config.toml"));
        let implementer = orq_tui::AgentSetting { harness: "claude".into(), model: "opus".into() };
        let none = orq_tui::AgentSetting::default();

        backend.save_settings(&implementer, &none).unwrap();
        let view = backend.load_settings();
        assert_eq!(view.implementer, implementer);
        assert_eq!(view.reviewer, none);
        assert_eq!(view.error, None);

        let half = orq_tui::AgentSetting { harness: "pi".into(), model: String::new() };
        assert!(backend.save_settings(&implementer, &half).is_err());
    }

    #[test]
    fn tdd_config_overrides_the_settings_reviewer_and_is_never_written() {
        let (dir, path) = ledger(&["override"]);
        let config = dir.join(".tdd-config.json");
        let text = r#"{"reviewers":{"default":{"reviewer":"pi","model":"gpt-6"},"allowedModels":["gpt-6"]}}"#;
        std::fs::write(&config, text).unwrap();
        let mut backend = LedgerBackend::new(path);
        backend.settings_path = Some(dir.join("cfg").join("orq").join("config.toml"));
        let none = orq_tui::AgentSetting::default();
        let reviewer = orq_tui::AgentSetting { harness: "claude".into(), model: "opus".into() };
        backend.save_settings(&none, &reviewer).unwrap();

        let view = backend.load_settings();

        assert_eq!(view.reviewer_override.as_deref(), Some("pi / gpt-6"));
        assert_eq!(view.error, None);
        assert_eq!(std::fs::read_to_string(&config).unwrap(), text);
    }

    #[test]
    fn saving_after_an_outside_edit_is_a_conflict_not_an_overwrite() {
        let (dir, path) = ledger(&["stale"]);
        let mut backend = LedgerBackend::new(path);
        let settings = dir.join("cfg").join("orq").join("config.toml");
        backend.settings_path = Some(settings.clone());
        let none = orq_tui::AgentSetting::default();
        let opus = orq_tui::AgentSetting { harness: "claude".into(), model: "opus".into() };
        backend.save_settings(&opus, &none).unwrap();
        backend.load_settings();
        let outside = "[implementer]\nharness = \"claude\"\nprofile = \"opus\"\n[reviewer]\nharness = \"pi\"\nprofile = \"gpt\"\n";
        std::fs::write(&settings, outside).unwrap();

        let sonnet = orq_tui::AgentSetting { harness: "claude".into(), model: "sonnet".into() };
        let result = backend.save_settings(&sonnet, &none);

        assert!(result.is_err());
        assert_eq!(std::fs::read_to_string(&settings).unwrap(), outside);
    }

    #[test]
    fn a_rejected_value_does_not_let_a_later_save_overwrite_an_outside_edit() {
        let (dir, path) = ledger(&["rejected-then-stale"]);
        let mut backend = LedgerBackend::new(path);
        let settings = dir.join("cfg").join("orq").join("config.toml");
        backend.settings_path = Some(settings.clone());
        let none = orq_tui::AgentSetting::default();
        let opus = orq_tui::AgentSetting { harness: "claude".into(), model: "opus".into() };
        backend.save_settings(&opus, &none).unwrap();
        backend.load_settings();
        let bad = orq_tui::AgentSetting { harness: "bad\nname".into(), model: "x".into() };
        assert!(backend.save_settings(&bad, &none).is_err());
        let outside = "[implementer]\nharness = \"pi\"\nprofile = \"gpt\"\n";
        std::fs::write(&settings, outside).unwrap();

        let sonnet = orq_tui::AgentSetting { harness: "claude".into(), model: "sonnet".into() };
        let result = backend.save_settings(&sonnet, &none);

        assert!(result.is_err());
        assert_eq!(std::fs::read_to_string(&settings).unwrap(), outside);
    }

    #[test]
    fn promoting_several_features_in_a_row_saves_each_one() {
        let (dir, path) = ledger(&["a", "b", "c"]);
        let mut backend = LedgerBackend::new(path.clone());
        backend.load().unwrap();

        let outcomes: Vec<bool> =
            ["a", "b", "c"].iter().map(|n| matches!(backend.promote(n), PromoteOutcome::Saved)).collect();

        assert_eq!(outcomes, vec![true, true, true]);
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.matches(r#""status":"pending""#).count(), 3, "{text}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_external_edit_after_the_last_load_is_still_a_conflict() {
        let (dir, path) = ledger(&["a", "b"]);
        let mut backend = LedgerBackend::new(path.clone());
        backend.load().unwrap();
        std::fs::write(&path, r#"[{"featureName":"a","status":"draft"},{"featureName":"b","status":"draft"} ]"#).unwrap();

        let outcome = backend.promote("a");

        assert!(matches!(outcome, PromoteOutcome::Failed { .. }));
        let _ = std::fs::remove_dir_all(dir);
    }

    fn fields(name: &str) -> orq_tui::FeatureFields {
        orq_tui::FeatureFields { name: name.into(), ..Default::default() }
    }

    #[test]
    fn a_file_created_after_an_absent_load_is_a_conflict_not_an_append() {
        let (dir, path) = ledger(&["x"]);
        std::fs::remove_file(&path).unwrap();
        let mut backend = LedgerBackend::new(path.clone());
        assert!(backend.load().is_err());
        let outside = r#"[{"featureName":"theirs","status":"draft"}]"#;
        std::fs::write(&path, outside).unwrap();

        let outcome = backend.add_feature(&fields("mine"));

        assert!(matches!(outcome, orq_tui::FormSaveOutcome::Conflict { .. }), "{outcome:?}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), outside);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_failed_write_leaves_the_bytes_alone_and_a_retry_succeeds() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, path) = ledger(&["a"]);
        let mut backend = LedgerBackend::new(path.clone());
        backend.load().unwrap();
        let before = std::fs::read_to_string(&path).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        // Root and DAC-override environments can still write into a 0555
        // directory; the permission fault cannot be injected there.
        let probe = dir.join(".probe");
        if std::fs::File::create(&probe).is_ok() {
            let _ = std::fs::remove_file(&probe);
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
            let _ = std::fs::remove_dir_all(dir);
            return;
        }

        let first = backend.add_feature(&fields("b"));

        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(first, orq_tui::FormSaveOutcome::Failed { .. }), "{first:?}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        let retry = backend.add_feature(&fields("b"));
        assert!(matches!(retry, orq_tui::FormSaveOutcome::Saved), "{retry:?}");
        assert!(std::fs::read_to_string(&path).unwrap().contains(r#""b""#));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn load_runs_lists_this_projects_runs_only() {
        use std::collections::BTreeMap;
        let fixture = orq_test_support::GitFixture::new();
        let root = std::env::temp_dir().join(format!("orq-backend-runs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let store = orq_orchestrator::RunStore::new(&root);
        let project = orq_git::resolve_project(fixture.root()).unwrap();
        let mine = crate::pipeline::project_id(&project);
        let seed = |project_id: &str, run_id: &str, feature: &str| {
            let profile = |model: &str| orq_orchestrator::RoleProfile {
                harness: "claude".into(),
                model: model.into(),
                thinking: None,
            };
            let mut profiles = BTreeMap::new();
            profiles.insert("implementer".to_string(), profile("opus"));
            profiles.insert("reviewer".to_string(), profile("kimi-k3"));
            store
                .create(&orq_orchestrator::RunRecord {
                    run_id: run_id.into(),
                    project_id: project_id.into(),
                    feature_id: feature.into(),
                    remote: "origin".into(),
                    provider: "github".into(),
                    target_branch: "main".into(),
                    base_sha: "abc".into(),
                    branch: format!("orq/{run_id}"),
                    worktree: std::path::PathBuf::from("/tmp/w"),
                    repository: String::new(),
                    profiles,
                })
                .unwrap();
        };
        seed(&mine, "run-1", "COM-01");
        seed("other-project", "run-2", "COM-02");
        let (dir, path) = ledger(&["a"]);
        let mut backend = LedgerBackend::new(path);
        backend.repo_root = Some(fixture.root().to_path_buf());
        backend.state_root_override = Some(root.clone());

        let rows = orq_tui::BrowserBackend::load_runs(&mut backend).unwrap();

        assert_eq!(
            rows,
            vec![orq_tui::RunRow {
                run_id: "run-1".into(),
                feature: "COM-01".into(),
                status: "running".into(),
                stage: None,
                implementer_model: Some("opus".into()),
                reviewer_model: Some("kimi-k3".into()),
                has_review_link: false,
            }]
        );
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A run in `root` for the fixture's project whose recorded process is long gone.
    fn seed_run_with_dead_process(root: &std::path::Path, repo: &std::path::Path, run_id: &str) {
        seed_run_with_process(root, repo, run_id, 4_294_967_294);
    }

    /// A run in `root` for the fixture's project that was launched by process `pid`.
    fn seed_run_with_process(root: &std::path::Path, repo: &std::path::Path, run_id: &str, pid: u64) {
        let store = orq_orchestrator::RunStore::new(root);
        let project = orq_git::resolve_project(repo).unwrap();
        let project_id = crate::pipeline::project_id(&project);
        store
            .create(&orq_orchestrator::RunRecord {
                run_id: run_id.into(),
                project_id: project_id.clone(),
                feature_id: "COM-01".into(),
                remote: "origin".into(),
                provider: "github".into(),
                target_branch: "main".into(),
                base_sha: "abc".into(),
                branch: format!("orq/{run_id}"),
                worktree: std::path::PathBuf::from("/tmp/w"),
                repository: String::new(),
                profiles: std::collections::BTreeMap::new(),
            })
            .unwrap();
        store
            .append_event(&orq_orchestrator::Event {
                schema_version: 1,
                event_id: "e-launched".into(),
                run_id: run_id.into(),
                project_id,
                feature_id: "COM-01".into(),
                kind: "run.launched".into(),
                stage: None,
                attempt: 1,
                occurred_at: "2026-11-02T00:00:00Z".into(),
                payload: serde_json::json!({"pid": pid}),
            })
            .unwrap();
    }

    #[test]
    fn load_runs_shows_a_run_whose_process_is_gone_as_failed() {
        let fixture = orq_test_support::GitFixture::new();
        let root = std::env::temp_dir().join(format!("orq-backend-dead-run-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        seed_run_with_dead_process(&root, fixture.root(), "run-dead");
        let (dir, path) = ledger(&["a"]);
        let mut backend = LedgerBackend::new(path);
        backend.repo_root = Some(fixture.root().to_path_buf());
        backend.state_root_override = Some(root.clone());

        let rows = orq_tui::BrowserBackend::load_runs(&mut backend).unwrap();

        assert_eq!(rows.iter().map(|r| r.status.as_str()).collect::<Vec<_>>(), ["failed"]);
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Load the runs of a backend whose state lives in `root`, for the fixture's repository.
    fn load_rows(root: &std::path::Path, repo: &std::path::Path) -> Vec<String> {
        let (dir, path) = ledger(&["a"]);
        let mut backend = LedgerBackend::new(path);
        backend.repo_root = Some(repo.to_path_buf());
        backend.state_root_override = Some(root.to_path_buf());
        let rows = orq_tui::BrowserBackend::load_runs(&mut backend).unwrap();
        let _ = std::fs::remove_dir_all(dir);
        rows.into_iter().map(|r| r.status).collect()
    }

    #[test]
    fn load_runs_leaves_a_run_with_a_live_process_running_and_records_nothing() {
        let fixture = orq_test_support::GitFixture::new();
        let root = std::env::temp_dir().join(format!("orq-backend-live-run-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        seed_run_with_process(&root, fixture.root(), "run-live", u64::from(std::process::id()));

        assert_eq!(load_rows(&root, fixture.root()), ["running"]);

        let events = orq_orchestrator::RunStore::new(&root).events("run-live").unwrap();
        assert!(events.iter().all(|e| e.kind != "run.failed"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn load_runs_shows_a_pause_requested_run_by_liveness_of_its_process() {
        let fixture = orq_test_support::GitFixture::new();
        let root = std::env::temp_dir().join(format!("orq-backend-pause-requested-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        seed_run_with_process(&root, fixture.root(), "run-live", u64::from(std::process::id()));
        seed_run_with_dead_process(&root, fixture.root(), "run-dead");
        let store = orq_orchestrator::RunStore::new(&root);
        orq_orchestrator::request_pause(&store, "run-live").unwrap();
        orq_orchestrator::request_pause(&store, "run-dead").unwrap();

        let mut rows = load_rows(&root, fixture.root());
        rows.sort();

        assert_eq!(rows, ["failed", "pause_requested"]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn load_runs_still_lists_a_dead_run_when_recording_its_failure_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = orq_test_support::GitFixture::new();
        let root = std::env::temp_dir().join(format!("orq-backend-unwritable-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        seed_run_with_dead_process(&root, fixture.root(), "run-dead");
        let log = root.join("runs").join("run-dead").join("events.jsonl");
        std::fs::set_permissions(&log, std::fs::Permissions::from_mode(0o444)).unwrap();
        if std::fs::OpenOptions::new().append(true).open(&log).is_ok() {
            eprintln!("skipped: the event log is still writable (running with write privileges)");
            let _ = std::fs::remove_dir_all(root);
            return;
        }

        // The row is kept (as the run is on record) rather than dropped or failing the listing.
        assert_eq!(load_rows(&root, fixture.root()).len(), 1);

        let _ = std::fs::set_permissions(&log, std::fs::Permissions::from_mode(0o644));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn resume_check_treats_a_run_whose_process_died_as_stopped() {
        let fixture = orq_test_support::GitFixture::new();
        let root = std::env::temp_dir().join(format!("orq-backend-resume-check-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        seed_run_with_dead_process(&root, fixture.root(), "run-dead");
        let (dir, path) = ledger(&["a"]);
        let mut backend = LedgerBackend::new(path);
        backend.repo_root = Some(fixture.root().to_path_buf());
        backend.state_root_override = Some(root.clone());

        let checked = orq_tui::BrowserBackend::resume_check(&mut backend, "run-dead");

        // Not "did not stop": the dead process was recorded as a failure first.
        assert!(!checked.unwrap_err().contains("may still be running"));
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn resume_check_refuses_a_failed_run_the_group_resume_would_refuse() {
        let fixture = orq_test_support::GitFixture::new();
        let root = std::env::temp_dir().join(format!("orq-backend-resume-refuse-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        // The seeded run's worktree is outside the repository's worktrees directory.
        seed_run_with_dead_process(&root, fixture.root(), "run-dead");
        let (dir, path) = ledger(&["a"]);
        let mut backend = LedgerBackend::new(path);
        backend.repo_root = Some(fixture.root().to_path_buf());
        backend.state_root_override = Some(root.clone());

        let checked = orq_tui::BrowserBackend::resume_check(&mut backend, "run-dead");

        let message = checked.unwrap_err();
        assert!(message.contains("worktrees directory"), "{message}");
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_mcp_catalog_names_are_read_from_the_list_output() {
        let text = "outside-in-tdd-mcp@0.2.3 (python, scaffold)\n  source: /a\nproject-mcp@1.2.1 (python, scaffold)\n  source: /b\n\nproject-mcp@1.3.0 (python)\n";
        assert_eq!(super::parse_mcp_catalog(text), vec!["outside-in-tdd-mcp".to_string(), "project-mcp".to_string()]);
        assert!(super::parse_mcp_catalog("").is_empty());
    }

    #[test]
    fn a_catalog_listing_that_hangs_is_given_up_on_and_its_process_is_stopped() {
        use std::os::unix::fs::PermissionsExt;
        let dir = orq_test_support::process_temp_dir().join(format!("orq-mcpctl-hang-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let marker = dir.join("still-running");
        let shim = dir.join("mcpctl");
        std::fs::write(&shim, format!("#!/bin/sh\nsleep 2\ntouch {}\n", marker.display())).unwrap();
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();

        let started = std::time::Instant::now();
        let result = super::mcpctl_catalog_with(&shim, std::time::Duration::from_millis(200));

        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        assert_eq!(result, Ok(Vec::new()));
        std::thread::sleep(std::time::Duration::from_millis(2300));
        assert!(!marker.exists(), "the listing process kept running");
    }

    #[test]
    fn a_catalog_listing_whose_child_keeps_the_output_open_is_still_given_up_on() {
        use std::os::unix::fs::PermissionsExt;
        let dir = orq_test_support::process_temp_dir().join(format!("orq-mcpctl-orphan-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let shim = dir.join("mcpctl");
        // The listing itself ends at once, but a background process keeps its output open.
        std::fs::write(&shim, "#!/bin/sh\nsleep 2 &\necho 'one@1.0 (x)'\n").unwrap();
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();

        let started = std::time::Instant::now();
        let result = super::mcpctl_catalog_with(&shim, std::time::Duration::from_millis(300));

        assert!(started.elapsed() < std::time::Duration::from_secs(1), "took {:?}", started.elapsed());
        assert_eq!(result, Ok(Vec::new()));
    }

    #[test]
    fn a_catalog_listing_that_answers_in_time_is_parsed() {
        use std::os::unix::fs::PermissionsExt;
        let dir = orq_test_support::process_temp_dir().join(format!("orq-mcpctl-ok-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let shim = dir.join("mcpctl");
        std::fs::write(&shim, "#!/bin/sh\necho 'one@1.0 (x)'\necho '  source: /s'\n").unwrap();
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert_eq!(super::mcpctl_catalog_with(&shim, std::time::Duration::from_secs(5)), Ok(vec!["one".to_string()]));
    }

    #[test]
    fn launch_profiles_lists_the_repos_profiles_and_an_unknown_one_is_refused_before_spawning() {
        let fixture = orq_test_support::GitFixture::new();
        std::fs::write(
            fixture.root().join(".orq.toml"),
            "[models.zz-launch-a]\nharness = \"claude\"\nmodel = \"opus\"\n\n[models.zz-launch-b]\nharness = \"claude\"\nmodel = \"sonnet\"\n",
        )
        .unwrap();
        let (dir, path) = ledger(&["a"]);
        let mut backend = LedgerBackend::new(path);
        backend.repo_root = Some(fixture.root().to_path_buf());

        let names = orq_tui::BrowserBackend::launch_profiles(&mut backend).unwrap();
        let refused = orq_tui::BrowserBackend::launch(&mut backend, "a", "no-such-profile", "zz-launch-b");

        assert!(names.contains(&"zz-launch-a".to_string()) && names.contains(&"zz-launch-b".to_string()), "{names:?}");
        let reason = refused.unwrap_err();
        assert!(reason.contains("no-such-profile"), "{reason}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn launch_output_gives_the_run_id_or_the_first_reason_line() {
        assert_eq!(crate::launch_outcome(true, "run: run-9\nlog: /x\nstate: launched\n", ""), Ok("run-9".to_string()));
        assert!(crate::launch_outcome(true, "state: launched\n", "").unwrap_err().contains("no run id"));
        assert_eq!(
            crate::launch_outcome(false, "", "\norq: feature a already has an active run: run-1\nmore\n"),
            Err("feature a already has an active run: run-1".to_string())
        );
        assert_eq!(crate::launch_outcome(false, "", "  \n"), Err("orq run failed".to_string()));
    }

    #[test]
    fn the_target_branch_is_the_remotes_default_or_main() {
        let fixture = orq_test_support::GitFixture::new();
        assert_eq!(crate::default_target_branch(fixture.root()), "main");

        let git = |args: &[&str]| {
            let ok = std::process::Command::new("git").arg("-C").arg(fixture.root()).args(args).status().unwrap();
            assert!(ok.success(), "{args:?}");
        };
        git(&["update-ref", "refs/remotes/origin/trunk", "HEAD"]);
        git(&["symbolic-ref", "refs/remotes/origin/HEAD", "refs/remotes/origin/trunk"]);

        assert_eq!(crate::default_target_branch(fixture.root()), "trunk");
    }
    #[test]
    fn a_configured_base_branch_beats_the_remotes_default() {
        let fixture = orq_test_support::GitFixture::new();
        std::fs::create_dir_all(fixture.root().join(".orq")).unwrap();
        std::fs::write(fixture.root().join(".orq/run.toml"), "base_branch = \"develop\"\n").unwrap();
        assert_eq!(crate::configured_target_branch(fixture.root()), Ok("develop".to_string()));

        std::fs::write(fixture.root().join(".orq/run.toml"), "base_branch = \"-x\"\n").unwrap();
        assert!(crate::configured_target_branch(fixture.root()).unwrap_err().contains("base_branch"));
    }

    struct FakeHerdr {
        calls: std::cell::RefCell<Vec<Vec<String>>>,
        fail: bool,
    }

    impl orq_herdr::HerdrCli for FakeHerdr {
        fn run(&self, args: &[String]) -> Result<String, String> {
            self.calls.borrow_mut().push(args.to_vec());
            if self.fail {
                return Err("herdr is not running".to_string());
            }
            Ok(r#"{"result":{"root_pane":{"pane_id":"w1:p5","tab_id":"w1:t2","workspace_id":"w1"}}}"#.to_string())
        }
    }

    #[test]
    fn a_launched_run_gets_a_pane_that_is_recorded_and_a_herdr_failure_leaves_the_run_alone() {
        let root = std::env::temp_dir().join(format!("orq-attach-pane-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let store = orq_orchestrator::RunStore::new(&root);
        let record = orq_orchestrator::RunRecord {
            run_id: "run-9".into(),
            project_id: "p".into(),
            feature_id: "COM-01".into(),
            remote: "origin".into(),
            provider: "github".into(),
            target_branch: "main".into(),
            base_sha: "abc".into(),
            branch: "fix--COM-01".into(),
            worktree: "/tmp/w".into(),
            repository: String::new(),
            profiles: Default::default(),
        };
        store.create(&record).unwrap();
        let repo = std::path::Path::new("/repo");
        let log = std::path::Path::new("/state/logs/run-9/runner.log");

        let herdr = FakeHerdr { calls: Default::default(), fail: false };
        crate::attach_pane(&herdr, &store, Some("w1"), repo, "run-9", log, std::time::Duration::ZERO).unwrap();
        let pane = orq_orchestrator::run_pane(&store, "run-9").unwrap().unwrap();
        assert_eq!((pane.workspace_id.as_str(), pane.tab_id.as_str(), pane.pane_id.as_str()), ("w1", "w1:t2", "w1:p5"));
        assert!(herdr.calls.borrow()[0].contains(&"orq run-9".to_string()));

        let down = FakeHerdr { calls: Default::default(), fail: true };
        let err = crate::attach_pane(&down, &store, Some("w1"), repo, "run-9", log, std::time::Duration::ZERO).unwrap_err();
        assert!(err.contains("herdr is not running"), "{err}");

        assert_eq!(crate::launch_log("run: r\nlog: /state/logs/r/runner.log\n"), Some("/state/logs/r/runner.log".into()));
        assert_eq!(crate::launch_log("run: r\n"), None);
    }

    #[test]
    fn the_pane_waits_for_the_runner_to_register_the_run_and_says_when_it_never_does() {
        let root = std::env::temp_dir().join(format!("orq-attach-late-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let store = orq_orchestrator::RunStore::new(&root);
        let repo = std::path::Path::new("/repo");
        let log = std::path::Path::new("/state/logs/run-3/runner.log");
        let herdr = FakeHerdr { calls: Default::default(), fail: false };

        let late = std::thread::spawn({
            let root = root.clone();
            move || {
                std::thread::sleep(std::time::Duration::from_millis(300));
                let record = orq_orchestrator::RunRecord {
                    run_id: "run-3".into(),
                    project_id: "p".into(),
                    feature_id: "COM-01".into(),
                    remote: "origin".into(),
                    provider: "github".into(),
                    target_branch: "main".into(),
                    base_sha: "abc".into(),
                    branch: "fix--COM-01".into(),
                    worktree: "/tmp/w".into(),
                    repository: String::new(),
                    profiles: Default::default(),
                };
                orq_orchestrator::RunStore::new(&root).create(&record).unwrap();
            }
        });
        crate::attach_pane(&herdr, &store, Some("w1"), repo, "run-3", log, std::time::Duration::from_secs(10)).unwrap();
        late.join().unwrap();
        assert!(orq_orchestrator::run_pane(&store, "run-3").unwrap().is_some());

        let err = crate::attach_pane(&herdr, &store, Some("w1"), repo, "run-never", log, std::time::Duration::from_millis(200))
            .unwrap_err();
        assert!(err.contains("opened") && err.contains("did not register within 200ms"), "{err}");
    }

    #[test]
    fn a_registration_wait_that_runs_out_tells_the_user_the_bound_and_that_the_run_continues() {
        let root = std::env::temp_dir().join(format!("orq-attach-bound-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let store = orq_orchestrator::RunStore::new(&root);
        let repo = std::path::Path::new("/repo");
        let log = std::path::Path::new("/state/logs/run-x/runner.log");
        let herdr = FakeHerdr { calls: Default::default(), fail: false };

        let err = crate::attach_pane(&herdr, &store, Some("w1"), repo, "run-x", log, std::time::Duration::from_millis(200))
            .unwrap_err();

        assert!(err.contains("within 200ms"), "the bound is named: {err}");
        assert!(err.contains("run continues"), "{err}");
        assert!(err.contains("run-x"), "{err}");
        assert_eq!(crate::describe_wait(std::time::Duration::from_secs(10)), "10s");
        assert_eq!(crate::describe_wait(std::time::Duration::from_millis(1500)), "1.5s");
    }

    #[test]
    fn a_thread_launch_reports_nothing_while_it_works_and_its_result_once_it_is_done() {
        use super::ThreadLaunch;
        use orq_tui::LaunchJob;
        let (release, gate) = std::sync::mpsc::channel::<()>();
        let mut job = ThreadLaunch::spawn(move || {
            gate.recv().unwrap();
            Ok("run-1".to_string())
        });
        assert_eq!(job.poll(), None, "still working");
        release.send(()).unwrap();
        let mut result = None;
        for _ in 0..200 {
            result = job.poll();
            if result.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(result, Some(Ok("run-1".to_string())));
    }

    #[test]
    fn a_thread_launch_that_panics_reports_a_failure_instead_of_hanging() {
        use super::ThreadLaunch;
        use orq_tui::LaunchJob;
        let mut job = ThreadLaunch::spawn(|| panic!("boom"));
        let mut result = None;
        for _ in 0..200 {
            result = job.poll();
            if result.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(matches!(result, Some(Err(_))), "{result:?}");
    }

    #[test]
    fn launches_in_sequence_run_one_after_another_and_a_failure_does_not_stop_the_rest() {
        use super::ThreadLaunch;
        use orq_tui::LaunchJob;
        use std::sync::{Arc, Mutex};
        let log = Arc::new(Mutex::new(Vec::<String>::new()));
        let first = {
            let log = Arc::clone(&log);
            move || {
                log.lock().unwrap().push("1 start".into());
                std::thread::sleep(std::time::Duration::from_millis(60));
                log.lock().unwrap().push("1 end".into());
                Err("cannot start".to_string())
            }
        };
        let second = {
            let log = Arc::clone(&log);
            move || {
                log.lock().unwrap().push("2 start".into());
                Ok("run-2".to_string())
            }
        };
        let works: Vec<Box<dyn FnOnce() -> Result<String, String> + Send>> = vec![Box::new(first), Box::new(second)];

        let mut jobs = ThreadLaunch::spawn_in_sequence(works);

        let mut results = vec![None, None];
        for _ in 0..300 {
            for (slot, job) in results.iter_mut().zip(jobs.iter_mut()) {
                if slot.is_none() {
                    *slot = job.poll();
                }
            }
            if results.iter().all(Option::is_some) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(*log.lock().unwrap(), ["1 start", "1 end", "2 start"]);
        assert_eq!(results, vec![Some(Err("cannot start".to_string())), Some(Ok("run-2".to_string()))]);
    }

    struct ScriptHerdr(std::cell::RefCell<Vec<Result<String, String>>>);

    impl orq_herdr::HerdrCli for ScriptHerdr {
        fn run(&self, _args: &[String]) -> Result<String, String> {
            self.0.borrow_mut().remove(0)
        }
    }

    #[test]
    fn focusing_a_run_needs_a_recorded_pane_and_tells_a_gone_pane_from_a_broken_herdr() {
        let root = std::env::temp_dir().join(format!("orq-focus-run-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let store = orq_orchestrator::RunStore::new(&root);
        let record = orq_orchestrator::RunRecord {
            run_id: "run-1".into(),
            project_id: "p".into(),
            feature_id: "COM-01".into(),
            remote: "origin".into(),
            provider: "github".into(),
            target_branch: "main".into(),
            base_sha: "abc".into(),
            branch: "fix--COM-01".into(),
            worktree: "/tmp/w".into(),
            repository: String::new(),
            profiles: Default::default(),
        };
        store.create(&record).unwrap();
        let herdr = |replies: Vec<Result<String, String>>| ScriptHerdr(std::cell::RefCell::new(replies));

        let none = crate::focus_run(&herdr(vec![]), &store, "run-1");
        assert!(matches!(none, orq_tui::PaneFocus::Missing(reason) if reason.contains("no Herdr pane")));

        let pane = orq_orchestrator::PaneRef { workspace_id: "w1".into(), tab_id: "w1:t2".into(), pane_id: "w1:p5".into() };
        orq_orchestrator::record_pane(&store, "run-1", &pane).unwrap();

        assert_eq!(crate::focus_run(&herdr(vec![Ok("{}".into()), Ok("{}".into())]), &store, "run-1"), orq_tui::PaneFocus::Focused);
        let gone = crate::focus_run(&herdr(vec![Err("tab_not_found: tab w1:t2 not found".into())]), &store, "run-1");
        assert!(matches!(gone, orq_tui::PaneFocus::Missing(reason) if reason.contains("no longer exists")));
        let down = crate::focus_run(&herdr(vec![Err("server_not_running: no herdr server".into())]), &store, "run-1");
        assert!(matches!(down, orq_tui::PaneFocus::Failed(reason) if reason.contains("no herdr server")));
        let unknown = crate::focus_run(&herdr(vec![]), &store, "no-such-run");
        assert!(matches!(unknown, orq_tui::PaneFocus::Failed(_)));
    }

    #[test]
    fn reopening_a_run_pane_follows_its_runner_log_and_records_the_new_pane() {
        let root = std::env::temp_dir().join(format!("orq-reopen-run-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let store = orq_orchestrator::RunStore::new(&root);
        let record = orq_orchestrator::RunRecord {
            run_id: "run-1".into(),
            project_id: "p".into(),
            feature_id: "COM-01".into(),
            remote: "origin".into(),
            provider: "github".into(),
            target_branch: "main".into(),
            base_sha: "abc".into(),
            branch: "fix--COM-01".into(),
            worktree: "/tmp/w".into(),
            repository: String::new(),
            profiles: Default::default(),
        };
        store.create(&record).unwrap();
        std::fs::create_dir_all(root.join("logs/run-1")).unwrap();
        std::fs::write(root.join("logs/run-1/runner.log"), "log\n").unwrap();
        let created = r#"{"result":{"root_pane":{"pane_id":"w1:p9","tab_id":"w1:t8","workspace_id":"w1"}}}"#;
        let herdr = ScriptHerdr(std::cell::RefCell::new(vec![Ok(created.into()), Ok("{}".into())]));

        let outcome = crate::reopen_run(&herdr, &store, None, std::path::Path::new("/repo"), &root, "run-1");

        assert_eq!(outcome, orq_tui::PaneFocus::Focused);
        let pane = orq_orchestrator::run_pane(&store, "run-1").unwrap().unwrap();
        assert_eq!(pane.pane_id, "w1:p9");

        let down = ScriptHerdr(std::cell::RefCell::new(vec![Err("server_not_running: no herdr server".into())]));
        let failed = crate::reopen_run(&down, &store, None, std::path::Path::new("/repo"), &root, "run-1");
        assert!(matches!(failed, orq_tui::PaneFocus::Failed(reason) if reason.contains("no herdr server")));
    }

    #[test]
    fn reopening_a_run_without_a_runner_log_opens_nothing_and_says_why() {
        let root = std::env::temp_dir().join(format!("orq-reopen-nolog-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let store = orq_orchestrator::RunStore::new(&root);
        let herdr = ScriptHerdr(std::cell::RefCell::new(vec![]));

        let outcome = crate::reopen_run(&herdr, &store, None, std::path::Path::new("/repo"), &root, "run-1");

        assert!(matches!(outcome, orq_tui::PaneFocus::Failed(reason) if reason.contains("no log")));
    }

    #[test]
    fn reopening_a_foreground_run_without_a_runner_log_follows_its_newest_stage_log() {
        let root = std::env::temp_dir().join(format!("orq-reopen-foreground-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let logs = root.join("logs/run-1");
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(logs.join("implement.log"), "a\n").unwrap();
        std::fs::write(logs.join("gate.log"), "b\n").unwrap();
        std::fs::write(logs.join("implement.done"), "").unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
        std::fs::File::options().write(true).open(logs.join("implement.log")).unwrap().set_modified(old).unwrap();
        let store = orq_orchestrator::RunStore::new(&root);
        let record = orq_orchestrator::RunRecord {
            run_id: "run-1".into(),
            project_id: "p".into(),
            feature_id: "COM-01".into(),
            remote: "origin".into(),
            provider: "github".into(),
            target_branch: "main".into(),
            base_sha: "abc".into(),
            branch: "fix--COM-01".into(),
            worktree: "/tmp/w".into(),
            repository: String::new(),
            profiles: Default::default(),
        };
        store.create(&record).unwrap();
        let herdr = RecordHerdr(std::cell::RefCell::new(vec![]));

        let outcome = crate::reopen_run(&herdr, &store, None, std::path::Path::new("/repo"), &root, "run-1");

        assert_eq!(outcome, orq_tui::PaneFocus::Focused);
        let pane = orq_orchestrator::run_pane(&store, "run-1").unwrap().unwrap();
        assert_eq!(pane.pane_id, "w1:p9");
        let calls = herdr.0.borrow().iter().flatten().cloned().collect::<Vec<_>>().join(" ");
        assert!(calls.contains("gate.log"), "{calls}");
        assert!(!calls.contains("implement.log"), "{calls}");
    }

    struct RecordHerdr(std::cell::RefCell<Vec<Vec<String>>>);

    impl orq_herdr::HerdrCli for RecordHerdr {
        fn run(&self, args: &[String]) -> Result<String, String> {
            let mut calls = self.0.borrow_mut();
            calls.push(args.to_vec());
            if calls.len() == 1 {
                Ok(r#"{"result":{"root_pane":{"pane_id":"w1:p9","tab_id":"w1:t8","workspace_id":"w1"}}}"#.into())
            } else {
                Ok("{}".into())
            }
        }
    }

    #[test]
    fn reopening_opens_a_tab_on_the_project_that_tails_the_runner_log_and_reports_an_unrecorded_pane() {
        let root = std::env::temp_dir().join(format!("orq-reopen-cmds-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("logs/run-1")).unwrap();
        std::fs::write(root.join("logs/run-1/runner.log"), "log\n").unwrap();
        let store = orq_orchestrator::RunStore::new(&root);
        let herdr = RecordHerdr(Default::default());

        // The run was never registered, so the new pane cannot be recorded.
        let outcome = crate::reopen_run(&herdr, &store, Some("w1"), std::path::Path::new("/repo"), &root, "run-1");

        assert!(matches!(outcome, orq_tui::PaneFocus::Failed(reason) if reason.contains("not recorded")));
        let calls = herdr.0.borrow();
        assert_eq!(calls[0][..2], ["tab".to_string(), "create".to_string()]);
        assert!(calls[0].iter().any(|a| a == "/repo") && calls[0].iter().any(|a| a == "orq run-1"), "{calls:?}");
        assert_eq!(calls[1][..3], ["pane".to_string(), "run".to_string(), "w1:p9".to_string()]);
        let log = root.join("logs/run-1/runner.log").display().to_string();
        assert!(calls[1][3].contains(&log), "{calls:?}");
    }

    #[test]
    fn a_runs_review_link_goes_to_the_action_and_an_absent_link_says_why() {
        let root = std::env::temp_dir().join(format!("orq-review-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let store = orq_orchestrator::RunStore::new(&root);
        let record = orq_orchestrator::RunRecord {
            run_id: "run-1".into(),
            project_id: "p".into(),
            feature_id: "COM-01".into(),
            remote: "origin".into(),
            provider: "github".into(),
            target_branch: "main".into(),
            base_sha: "abc".into(),
            branch: "fix--COM-01".into(),
            worktree: "/tmp/w".into(),
            repository: String::new(),
            profiles: Default::default(),
        };
        store.create(&record).unwrap();
        let act = |url: &str| -> Result<String, String> { Ok(format!("did {url}")) };

        let none = crate::with_review_link(&store, "p", "run-1", &act);
        assert!(matches!(none, Err(reason) if reason.contains("no review request yet")));
        let other_project = crate::with_review_link(&store, "q", "run-1", &act);
        assert!(other_project.is_err());

        store
            .append_event(&orq_orchestrator::Event {
                schema_version: 1,
                event_id: "e-review".into(),
                run_id: "run-1".into(),
                project_id: "p".into(),
                feature_id: "COM-01".into(),
                kind: "review.created".into(),
                stage: None,
                attempt: 1,
                occurred_at: "2026-11-02T00:00:00Z".into(),
                payload: serde_json::json!({"url": "https://example.test/pr/7"}),
            })
            .unwrap();

        assert_eq!(crate::with_review_link(&store, "p", "run-1", &act), Ok("did https://example.test/pr/7".to_string()));
        let failing = |_: &str| -> Result<String, String> { Err("no clipboard tool found".into()) };
        assert_eq!(crate::with_review_link(&store, "p", "run-1", &failing), Err("no clipboard tool found".to_string()));
    }

    #[test]
    fn only_an_absolute_http_or_https_url_with_a_host_is_acted_on() {
        let act = |url: &str| -> Result<String, String> { Ok(format!("did {url}")) };
        for good in ["https://example.test/pr/7", "http://host/x", "HTTPS://Host:8443/x?y#z", "https://user@host/x", "https://[::1]:80/x"] {
            assert_eq!(crate::with_checked_url(good, &act), Ok(format!("did {good}")));
        }
        for bad in ["", "/etc/passwd", "file:///etc/passwd", "--help", "-x", "javascript:alert(1)", "https://", "https:///nohost", "ssh://host/x", "https://a b/c", "https://host/\nx", "https://user@/x", "https://:443/x", "https://[invalid]/x", "https://host:port/x", "https://@/x"] {
            let got = crate::with_checked_url(bad, &act);
            assert!(matches!(&got, Err(reason) if reason.contains("not a web link")), "{bad:?} gave {got:?}");
        }
    }
}

#[cfg(test)]
mod forwarding_tests {
    use std::ffi::OsString;

    fn os(words: &[&str]) -> Vec<OsString> {
        words.iter().map(OsString::from).collect()
    }

    #[test]
    fn every_option_value_survives_forwarding_even_when_spelled_like_the_detach_flag() {
        for flag in ["--repo", "--feature", "--group", "--remote", "--target", "--implementer", "--reviewer", "--context", "--issue", "--mr-assignee", "--mr-reviewer", "--run-id"] {
            let args = os(&["run", flag, "--detach", "--detach"]);
            assert_eq!(super::without_detach_flag(&args), os(&["run", flag, "--detach"]), "{flag}");
        }
    }
}

/// `orq run pause <run>`: ask a running run to stop at its next stage boundary.
fn run_pause(args: &[OsString]) -> ExitCode {
    let [run_id] = args else { return usage_error() };
    let Some(run_id) = run_id.to_str() else { return usage_error() };
    let Some(root) = pipeline::state_root() else {
        return fail("cannot locate the Orq state directory: neither XDG_STATE_HOME nor HOME is set");
    };
    match orq_orchestrator::request_pause(&orq_orchestrator::RunStore::new(&root), run_id) {
        Ok(()) => {
            println!("Pause requested for run {run_id}; it stops at the next stage boundary.");
            ExitCode::SUCCESS
        }
        Err(message) => fail(message),
    }
}

/// `orq run stop <run>`: end a live run's owner process and record the run failed, so it can be resumed.
fn run_stop(args: &[OsString]) -> ExitCode {
    let [run_id] = args else { return usage_error() };
    let Some(run_id) = run_id.to_str() else { return usage_error() };
    let Some(root) = pipeline::state_root() else {
        return fail("cannot locate the Orq state directory: neither XDG_STATE_HOME nor HOME is set");
    };
    let store = orq_orchestrator::RunStore::new(&root);
    match orq_orchestrator::stop_run(&store, run_id, &orq_orchestrator::owner_state, &orq_orchestrator::terminate_process, &std::thread::sleep) {
        Ok(()) => {
            println!("Stopped run {}; resume it with `orq run resume {}`.", printable(run_id), printable(run_id));
            ExitCode::SUCCESS
        }
        Err(message) => fail(printable(&message)),
    }
}

/// `orq run discard <run> --yes [--repo <path>]`: remove a failed run's worktree (with any uncommitted
/// work), branch, feature locks, logs and record. Nothing is removed without `--yes`.
fn run_discard(args: &[OsString]) -> ExitCode {
    let (mut run_id, mut yes, mut repo) = (None, false, PathBuf::from("."));
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.to_str() {
            Some("--yes") => yes = true,
            Some("--repo") => match iter.next() {
                Some(value) => repo = PathBuf::from(value),
                None => return usage_error(),
            },
            Some(id) if !id.starts_with('-') && run_id.is_none() => run_id = Some(id),
            _ => return usage_error(),
        }
    }
    let Some(run_id) = run_id else { return usage_error() };
    if !yes {
        return fail(format!(
            "discarding {} deletes its worktree (including uncommitted work), branch and logs for good; re-run with --yes to confirm",
            printable(run_id)
        ));
    }
    let Some(root) = pipeline::state_root() else {
        return fail("cannot locate the Orq state directory: neither XDG_STATE_HOME nor HOME is set");
    };
    match orq_orchestrator::discard_run(&orq_orchestrator::RunStore::new(&root), run_id, &repo) {
        Ok(()) => {
            println!("Discarded run {}.", printable(run_id));
            ExitCode::SUCCESS
        }
        Err(message) => fail(printable(&message)),
    }
}

/// `orq run switch <run> --role implementer|reviewer --profile <name> [--repo <path>]`: record that a
/// paused run continues its role on another model profile. Nothing is recorded for a run that is not
/// paused, an unknown role or a profile the model configuration does not define.
fn run_switch(args: &[OsString]) -> ExitCode {
    let (mut run_id, mut role, mut profile, mut repo) = (None, None, None, PathBuf::from("."));
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.to_str() {
            Some("--role") => role = iter.next().and_then(|v| v.to_str()),
            Some("--profile") => profile = iter.next().and_then(|v| v.to_str()),
            Some("--repo") => match iter.next() {
                Some(value) => repo = PathBuf::from(value),
                None => return usage_error(),
            },
            Some(id) if !id.starts_with('-') && run_id.is_none() => run_id = Some(id),
            _ => return usage_error(),
        }
    }
    let (Some(run_id), Some(role), Some(profile)) = (run_id, role, profile) else { return usage_error() };
    if !matches!(role, "implementer" | "reviewer") {
        return fail(format!("unknown role {}: use implementer or reviewer", printable(role)));
    }
    let (profiles, _) = match load_model_config(&repo) {
        Ok(config) => config,
        Err(message) => return fail(message),
    };
    if profiles.get(profile).is_none() {
        return fail(format!("the profile {} is not defined in the model profiles", printable(profile)));
    }
    let Some(root) = pipeline::state_root() else {
        return fail("cannot locate the Orq state directory: neither XDG_STATE_HOME nor HOME is set");
    };
    match orq_orchestrator::request_model_switch(&orq_orchestrator::RunStore::new(&root), run_id, role, profile) {
        Ok(()) => {
            println!("Switch requested for run {}: {role} continues on {} when the run resumes.", printable(run_id), printable(profile));
            ExitCode::SUCCESS
        }
        Err(message) => fail(message),
    }
}

/// This project's runs, after recording as failed every run that still reads as running but whose
/// process is gone. A run that cannot be recorded is reported on stderr and listed as it reads.
fn list_runs_reconciled(store: &orq_orchestrator::RunStore, project_id: &str) -> Result<orq_orchestrator::RunList, String> {
    let mut list = orq_orchestrator::list_runs_report(store, project_id).map_err(|err| err.to_string())?;
    let mut reconciled = false;
    for run in &list.runs {
        if matches!(run.phase, orq_orchestrator::RunPhase::Running | orq_orchestrator::RunPhase::PauseRequested) {
            match orq_orchestrator::reconcile_interrupted(store, &run.run_id, &orq_orchestrator::process_is_alive) {
                Ok(changed) => reconciled |= changed,
                Err(err) => eprintln!(
                    "orq: {} has no running process, but it could not be recorded as failed: {err:?}",
                    printable(&run.run_id)
                ),
            }
        }
    }
    if reconciled {
        match orq_orchestrator::list_runs_report(store, project_id) {
            Ok(fresh) => list = fresh,
            Err(err) => eprintln!("orq: the listing could not be refreshed after recording failed runs: {err}"),
        }
    }
    Ok(list)
}

/// The pipeline continues the latest run of a run's features: `Ok` when the group resume is valid
/// and starts from `run_id`, the reason otherwise. Nothing is launched.
fn check_group_resume(store: &orq_orchestrator::RunStore, repo: &std::path::Path, run_id: &str) -> Result<(), String> {
    let record = store.load(run_id).map_err(|err| format!("cannot read the run: {err:?}"))?;
    let members = orq_orchestrator::run_members(store, run_id)?;
    let plan = orq_orchestrator::plan_group_resume(store, repo, &record.project_id, &members)?;
    if plan.from.run_id == run_id {
        Ok(())
    } else {
        Err(format!("{} is a later run of the same features; resume that one", plan.from.run_id))
    }
}

/// `orq run resume <run-id> [--yes] [--repo <path>]`: continue a paused run from its checkpoint as a
/// new run on the same branch and worktree. Nothing changes without confirmation, for a run that is
/// not paused, or when the checkpoint no longer matches the checkout.
fn run_resume(args: &[OsString]) -> ExitCode {
    let mut run_id = None;
    let mut yes = false;
    let mut no_ci_check = false;
    let mut detach = false;
    let mut repo = PathBuf::from(".");
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.to_str() {
            Some("--yes") => yes = true,
            Some("--detach") => detach = true,
            Some("--no-ci-check") => no_ci_check = true,
            Some("--repo") => match iter.next() {
                Some(value) => repo = PathBuf::from(value),
                None => return usage_error(),
            },
            Some(id) if !id.starts_with('-') && run_id.is_none() => run_id = Some(id.to_string()),
            _ => return usage_error(),
        }
    }
    let Some(run_id) = run_id else { return usage_error() };
    let Some(root) = pipeline::state_root() else {
        return fail("cannot locate the Orq state directory: neither XDG_STATE_HOME nor HOME is set");
    };
    let store = orq_orchestrator::RunStore::new(&root);
    // A run killed mid-flight never recorded an ending: when its process is gone it is a failed run.
    if let Err(err) = orq_orchestrator::reconcile_interrupted(&store, &run_id, &orq_orchestrator::process_is_alive) {
        return fail(format!("cannot resume {}: {err:?}", printable(&run_id)));
    }
    let stopped = store
        .events(&run_id)
        .map(|events| {
            matches!(
                orq_orchestrator::summarize_run(&events).phase,
                orq_orchestrator::RunPhase::Failed | orq_orchestrator::RunPhase::Blocked
            )
        })
        .unwrap_or(false);
    let next = if stopped {
        // A failed or blocked run has no checkpoint; the pipeline picks it up through the group resume below.
        "where it stopped".to_string()
    } else {
        match orq_orchestrator::validate_checkpoint(&store, &repo, &run_id) {
            Ok(checkpoint) => checkpoint.next_stage.unwrap_or_else(|| "the end".to_string()),
            Err(reason) => return fail(format!("cannot resume {}: {}", printable(&run_id), printable(&reason))),
        }
    };
    let next = next.as_str();
    if !yes {
        use std::io::{BufRead, IsTerminal, Write};
        if !std::io::stdin().is_terminal() {
            return fail("resuming needs confirmation: pass --yes");
        }
        eprint!("Resume {} from {}? [y/N] ", printable(&run_id), printable(next));
        let _ = std::io::stderr().flush();
        let mut answer = String::new();
        if std::io::stdin().lock().read_line(&mut answer).is_err() || !matches!(answer.trim(), "y" | "Y" | "yes") {
            eprintln!("orq: not resumed");
            return ExitCode::FAILURE;
        }
    }
    let (record, members) = match (store.load(&run_id), orq_orchestrator::run_members(&store, &run_id)) {
        (Ok(record), Ok(members)) => (record, members),
        (Err(err), _) => return fail(format!("cannot read the run: {err:?}")),
        (_, Err(reason)) => return fail(reason),
    };
    if let Err(reason) = check_group_resume(&store, &repo, &run_id) {
        return fail(format!("cannot resume {}: {}", printable(&run_id), printable(&reason)));
    }
    let repo_root = repo.clone();
    let mut forwarded: Vec<OsString> = vec!["--repo".into(), repo.into_os_string(), "--remote".into(), record.remote.into(), "--target".into(), record.target_branch.into(), "--resume".into()];
    if members.len() > 1 {
        forwarded.extend(["--group".into(), members.join(",").into()]);
    } else {
        forwarded.extend(["--feature".into(), members[0].clone().into()]);
    }
    // A role with a switch request continues on the profile last requested for it; the others keep the
    // paused run's stored profile exactly (the resumed run restores it, whatever the configuration says now).
    let events = match store.events(&run_id) {
        Ok(events) => events,
        Err(err) => return fail(format!("cannot resume {}: cannot read its events: {err}", printable(&run_id))),
    };
    let (known, _) = match load_model_config(&repo_root) {
        Ok(config) => config,
        Err(message) => return fail(message),
    };
    for role in ["implementer", "reviewer"] {
        let requested = events
            .iter()
            .rev()
            .find(|e| e.kind == "run.model_switch_requested" && e.payload["role"] == role)
            .and_then(|e| e.payload["profile"].as_str().map(str::to_string));
        if let Some(name) = requested {
            if known.get(&name).is_none() {
                return fail(format!("cannot resume {}: the requested profile {} is no longer configured", printable(&run_id), printable(&name)));
            }
            forwarded.extend([format!("--{role}").into(), name.into(), format!("--switch-{role}").into()]);
        }
    }
    if no_ci_check {
        forwarded.push("--no-ci-check".into());
    }
    if detach {
        forwarded.push("--detach".into());
    }
    run(&forwarded)
}
