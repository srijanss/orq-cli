//! Starts runs for the web server: the same `orq run` the command line uses, checked first and then detached.

use std::path::{Path, PathBuf};

use orq_remote::projects::{LaunchError, RunLauncher};

pub struct CliLauncher {
    exe: PathBuf,
    user_config: Option<PathBuf>,
}

impl CliLauncher {
    pub fn new(exe: PathBuf, user_config: Option<PathBuf>) -> Self {
        CliLauncher { exe, user_config }
    }

    fn run(&self, args: &[std::ffi::OsString]) -> Result<std::process::Output, LaunchError> {
        std::process::Command::new(&self.exe)
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|err| LaunchError::Failed(format!("cannot start orq run: {err}")))
    }
}

impl RunLauncher for CliLauncher {
    fn start(&self, repo: &Path, feature: &str) -> Result<String, LaunchError> {
        let refused = |reason: String| LaunchError::Refused(reason);
        let project = orq_git::resolve_project(repo).map_err(|err| refused(format!("cannot resolve project: {err:?}")))?;
        let user_config = self.user_config.as_deref().unwrap_or(Path::new(""));
        let profiles = orq_features::load_model_profiles(user_config, &project.root.join(".orq.toml")).map_err(|err| refused(err.to_string()))?;
        let reviewers = orq_features::load_reviewer_config(&project.root.join(".tdd-config.json")).map_err(|err| refused(err.to_string()))?;
        let settings = orq_features::Settings::load(user_config).ok();
        let agent = |role| settings.as_ref().and_then(|settings| settings.agent(role).ok().flatten());
        let (implementer, reviewer) = orq_features::default_role_profiles(
            &profiles,
            &reviewers,
            agent(orq_features::AgentRole::Implementer).as_ref(),
            agent(orq_features::AgentRole::Reviewer).as_ref(),
        );
        let implementer = implementer.ok_or_else(|| refused("no default implementer model is set".to_string()))?;
        let reviewer = reviewer.ok_or_else(|| refused("no default reviewer model is set".to_string()))?;
        let request = orq_features::LaunchRequest {
            repo: project.root.clone(),
            feature: feature.to_string(),
            remote: "origin".to_string(),
            target: crate::configured_target_branch(&project.root).map_err(refused)?,
            implementer,
            reviewer,
            context: None,
            issue: None,
        };
        request.validate(&profiles, &reviewers).map_err(|err| refused(err.to_string()))?;
        let args = crate::without_detach_flag(&request.args());
        let mut check = args.clone();
        check.push("--check".into());
        let checked = self.run(&check)?;
        if !checked.status.success() {
            let reason = crate::launch_outcome(false, "", &String::from_utf8_lossy(&checked.stderr)).unwrap_or_else(|reason| reason);
            return Err(refused(reason));
        }
        let mut detached = args;
        detached.push("--detach".into());
        let started = self.run(&detached)?;
        crate::launch_outcome(started.status.success(), &String::from_utf8_lossy(&started.stdout), &String::from_utf8_lossy(&started.stderr)).map_err(LaunchError::Failed)
    }
}

#[cfg(test)]
mod tests;
