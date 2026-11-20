//! The web's Push of a project's target to origin: the state the Push button and its confirmation
//! show, and the push the person confirmed. The checks and the push are the orchestrator's
//! `target_push`; this only finds the project and words the answer.

use std::path::Path;

use orq_core::remote_inbox::TargetPushResult;
use orq_orchestrator::{RunStore, TargetPush, TargetPushOutcome};
use serde_json::{json, Value};

use super::queue::project_target;
use super::StoreProjects;

/// Most commits the confirmation lists; the rest are counted as "and N more".
pub const LISTED_COMMITS: usize = 50;

/// `github` or `gitlab` for the project whose main checkout is `root`, from its forge setting or
/// else origin's push address; `None` when orq cannot tell. Reads local config only: no network.
pub(super) fn provider_of(root: &Path) -> Option<&'static str> {
    let forge = orq_orchestrator::load_run_settings(root, &|name| std::env::var(name).ok()).ok().and_then(|settings| settings.forge);
    // The address as configured, not as insteadOf rewrites it, as the push's own check reads it.
    let configured = |key: &str| {
        let out = std::process::Command::new("git").arg("-C").arg(root).args(["config", "--get", key]).output().ok().filter(|out| out.status.success())?;
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string()).filter(|url| !url.is_empty())
    };
    let url = configured("remote.origin.pushurl").or_else(|| configured("remote.origin.url"))?;
    match orq_git::detect_provider(&url, &orq_git::ProviderHosts::default(), forge.as_deref()) {
        Ok(orq_git::Provider::GitHub) => Some("github"),
        Ok(orq_git::Provider::GitLab) => Some("gitlab"),
        Err(_) => None,
    }
}

impl StoreProjects {
    /// Runs `with` on the push request for project `id`; `None` when there is no such project.
    fn with_push<T>(&self, id: &str, with: impl FnOnce(&TargetPush) -> T) -> Result<Option<T>, String> {
        let Some((git, root)) = self.settings_key(id)? else { return Ok(None) };
        let project_id = orq_git::project_id(&git);
        let target = project_target(&root);
        let forge = orq_orchestrator::load_run_settings(&root, &|name| std::env::var(name).ok()).ok().and_then(|settings| settings.forge);
        let store = RunStore::new(&self.roots.state);
        let push = TargetPush {
            root: &root,
            project_id: &project_id,
            target: &target,
            forge: forge.as_deref(),
            store: &store,
            gh: self.gh.clone().unwrap_or_else(|| "gh".into()),
            fetch_limit: orq_orchestrator::TARGET_FETCH_LIMIT,
        };
        Ok(Some(with(&push)))
    }

    /// Project `id`'s push state as the web shows it, with at most [`LISTED_COMMITS`] commits.
    pub(super) fn push_target_state_of(&self, id: &str) -> Result<Option<Value>, String> {
        self.with_push(id, |push| {
            let mut state = orq_orchestrator::target_push_state(push);
            let more = state.commits.len().saturating_sub(LISTED_COMMITS);
            state.commits.truncate(LISTED_COMMITS);
            let mut shown = serde_json::to_value(&state).unwrap_or_else(|_| json!({}));
            shown["more"] = json!(more);
            shown
        })
    }

    /// Pushes project `id`'s target at `sha`, the commit the person confirmed.
    pub(super) fn push_target_of(&self, id: &str, sha: &str) -> Result<TargetPushResult, String> {
        let pushed = self.with_push(id, |push| match orq_orchestrator::push_target(push, sha) {
            TargetPushOutcome::Pushed { from, to, count, runs } => TargetPushResult::Pushed { target: push.target.to_string(), from, to, count, runs },
            TargetPushOutcome::Refused(why) => TargetPushResult::Refused(why),
        })?;
        Ok(pushed.unwrap_or(TargetPushResult::Unknown))
    }
}
