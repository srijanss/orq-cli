//! `orq push --repo <path> [--branch <target>] [--yes]`: pushes the project's target branch to
//! origin, the way the web's Push does, after listing the commits and asking `yes` (unless `--yes`).
//! The checks and the push are the orchestrator's `target_push`; every attempt that reaches the push
//! is audited as `project.push_target`, from the cli.

use std::ffi::OsString;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use orq_orchestrator::{RunStore, TargetPush, TargetPushOutcome};

use crate::{fail, printable, usage_error};

/// Most commits listed before "and N more".
const LISTED_COMMITS: usize = 50;

struct PushArgs {
    repo: PathBuf,
    branch: Option<String>,
    yes: bool,
}

fn parse(args: &[OsString]) -> Option<PushArgs> {
    let mut repo = None;
    let mut branch = None;
    let mut yes = false;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.to_str()? {
            "--repo" => repo = Some(PathBuf::from(args.next()?)),
            "--branch" => branch = Some(args.next()?.to_str()?.to_string()),
            "--yes" => yes = true,
            _ => return None,
        }
    }
    Some(PushArgs { repo: repo?, branch, yes })
}

pub(crate) fn push(args: &[OsString]) -> ExitCode {
    let Some(args) = parse(args) else { return usage_error() };
    let project = match orq_git::resolve_project(&args.repo) {
        Ok(project) => project,
        Err(err) => return fail(format!("cannot resolve project: {err:?}")),
    };
    let roots = match orq_core::storage::StorageRoots::resolve(&|name| std::env::var(name).ok()) {
        Ok(roots) => roots,
        Err(err) => return fail(format!("cannot locate the Orq data directory: {err}")),
    };
    let root = orq_git::main_checkout_root(&project).unwrap_or_else(|| project.root.clone());
    let settings = orq_orchestrator::load_run_settings(&root, &|name| std::env::var(name).ok()).ok();
    let target = args.branch.clone().or_else(|| settings.as_ref().and_then(|settings| settings.base_branch.clone())).unwrap_or_else(|| origin_head(&root));
    let project_id = crate::pipeline::project_id(&project);
    let store = RunStore::new(&roots.state);
    let push = TargetPush {
        root: &root,
        project_id: &project_id,
        target: &target,
        forge: settings.as_ref().and_then(|settings| settings.forge.as_deref()),
        store: &store,
        gh: crate::pipeline::find_program("gh").unwrap_or_else(|| "gh".into()),
        fetch_limit: orq_orchestrator::TARGET_FETCH_LIMIT,
    };

    let state = orq_orchestrator::target_push_state(&push);
    if let Some(reason) = state.reason {
        return fail(reason);
    }
    let Some(sha) = state.sha.clone() else { return fail("the target could not be read") };
    let noun = if state.count == 1 { "commit" } else { "commits" };
    println!("Push {} {noun} to {}/{target} ({}/{target}..{target}):", state.count, state.remote, state.remote);
    for commit in state.commits.iter().take(LISTED_COMMITS) {
        let from = commit.run.as_deref().map_or_else(|| "not from orq".to_string(), |run| format!("run {run}"));
        println!("  {} {}  ({})", commit.short, printable(&commit.subject), printable(&from));
    }
    if state.commits.len() > LISTED_COMMITS {
        println!("  and {} more", state.commits.len() - LISTED_COMMITS);
    }
    if !args.yes {
        print!("Push {} {noun}? Type yes to push: ", state.count);
        let _ = std::io::stdout().flush();
        let mut answer = String::new();
        let _ = std::io::stdin().lock().read_line(&mut answer);
        if answer.trim() != "yes" {
            return fail("nothing was pushed");
        }
    }

    use orq_core::remote_audit::{record, Attempt, Outcome};
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs());
    let audit = |outcome, reason: &str| {
        let _ = std::fs::create_dir_all(&roots.state);
        let attempt = Attempt { identity: "cli", action: "project.push_target", project: &project_id, run: None, outcome, reason: Some(reason) };
        if let Err(why) = record(&roots, &attempt, now) {
            eprintln!("orq: cannot audit the push: {}", printable(&why));
        }
    };
    match orq_orchestrator::push_target(&push, &sha) {
        TargetPushOutcome::Pushed { from, to, count, runs } => {
            let noun = if count == 1 { "commit" } else { "commits" };
            audit(Outcome::Allowed, &format!("pushed {target} from {from} to {to}, {count} {noun}, from cli"));
            println!("Pushed {count} {noun} to {}/{target}.", state.remote);
            if !runs.is_empty() {
                println!("Recorded on {}.", runs.join(", "));
            }
            ExitCode::SUCCESS
        }
        TargetPushOutcome::Refused(why) => {
            audit(Outcome::Refused, &why);
            fail(why)
        }
    }
}

/// The branch origin's HEAD names, else `main`.
fn origin_head(root: &std::path::Path) -> String {
    std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["symbolic-ref", "--short", "refs/remotes/origin/HEAD"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .and_then(|text| text.trim().strip_prefix("origin/").map(str::to_string))
        .filter(|branch| !branch.is_empty())
        .unwrap_or_else(|| "main".to_string())
}
