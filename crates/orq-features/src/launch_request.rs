//! A request to start a run for one feature with chosen implementer and
//! reviewer profiles: checked by the run model rules, then turned into the
//! `orq run --detach` command that starts it.

use std::ffi::OsString;
use std::path::PathBuf;

use crate::model_profiles::ModelProfiles;
use crate::reviewer_config::TddReviewerConfig;
use crate::run_models::{resolve_run_models, ResolvedModels, RunModelsError, RunSelection};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchRequest {
    pub repo: PathBuf,
    pub feature: String,
    pub remote: String,
    pub target: String,
    /// Model profile names, as `--implementer` and `--reviewer` take them.
    pub implementer: String,
    pub reviewer: String,
    /// Free text for the implementer, passed whole as `--context`.
    pub context: Option<String>,
    /// An issue the review closes, passed as `--issue`.
    pub issue: Option<String>,
}

impl LaunchRequest {
    /// The models the chosen profiles resolve to, or the rule that refuses them.
    pub fn validate(
        &self,
        profiles: &ModelProfiles,
        reviewer_config: &TddReviewerConfig,
    ) -> Result<ResolvedModels, RunModelsError> {
        let selection = RunSelection { implementer: Some(&self.implementer), reviewer: Some(&self.reviewer) };
        resolve_run_models(profiles, reviewer_config, &selection)
    }

    /// The arguments to `orq` that start this request detached. Each value is
    /// one argument, so none can be read as a flag by being spelled like one.
    pub fn args(&self) -> Vec<OsString> {
        let mut args: Vec<OsString> = vec!["run".into()];
        args.push("--repo".into());
        args.push(self.repo.clone().into_os_string());
        for (flag, value) in [
            ("--feature", &self.feature),
            ("--remote", &self.remote),
            ("--target", &self.target),
            ("--implementer", &self.implementer),
            ("--reviewer", &self.reviewer),
        ] {
            args.push(flag.into());
            args.push(value.into());
        }
        if let Some(context) = self.context.as_ref().filter(|c| !c.trim().is_empty()) {
            args.push("--context".into());
            args.push(context.into());
        }
        if let Some(issue) = self.issue.as_ref().filter(|i| !i.trim().is_empty()) {
            args.push("--issue".into());
            args.push(issue.into());
        }
        args.push("--detach".into());
        args
    }
}
