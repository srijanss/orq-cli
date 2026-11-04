use std::ffi::OsString;
use std::path::PathBuf;

use orq_features::{
    load_model_profiles, AgentProfile, LaunchRequest, ModelProfiles, RunModelsError, TddReviewerConfig,
};

const USER: &str = r#"
[models.opus]
harness = "claude"
model = "opus"

[models.sonnet]
harness = "claude"
model = "sonnet"
thinking = "high"

[models.gpt-6-astra]
harness = "pi"
model = "openai/gpt-6-astra"

[models.kimi]
harness = "pi"
model = "kimi-k3"
"#;

fn profiles(tag: &str) -> ModelProfiles {
    let dir: PathBuf = orq_test_support::process_temp_dir().join(format!("orq-launch-request-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("config.toml"), USER).unwrap();
    load_model_profiles(&dir.join("config.toml"), &dir.join("absent.toml")).unwrap()
}

fn request(implementer: &str, reviewer: &str) -> LaunchRequest {
    LaunchRequest {
        repo: PathBuf::from("/work/app"),
        feature: "COM-01".to_string(),
        remote: "origin".to_string(),
        target: "main".to_string(),
        implementer: implementer.to_string(),
        reviewer: reviewer.to_string(),
        context: None,
        issue: None,
    }
}

fn open_policy() -> TddReviewerConfig {
    TddReviewerConfig { default_reviewer: None, allowed_models: None }
}

#[test]
fn a_request_with_allowed_profiles_resolves_to_the_chosen_models() {
    let resolved = request("sonnet", "kimi").validate(&profiles("ok"), &open_policy()).unwrap();

    assert_eq!(resolved.implementer.model, "sonnet");
    assert_eq!(resolved.implementer.thinking.as_deref(), Some("high"));
    assert_eq!(resolved.reviewer.model, "kimi-k3");
}

#[test]
fn the_chosen_profiles_are_checked_by_the_run_model_rules() {
    let configured = profiles("rules");

    assert_eq!(
        request("gpt-6-astra", "kimi").validate(&configured, &open_policy()).unwrap().implementer.harness,
        "pi"
    );
    assert_eq!(
        request("opus", "nope").validate(&configured, &open_policy()).unwrap_err(),
        RunModelsError::UnknownProfile { role: "reviewer", name: "nope".into() }
    );
    let strict = TddReviewerConfig {
        default_reviewer: Some(AgentProfile { harness: "pi".into(), model: "openai/gpt-6-astra".into() }),
        allowed_models: Some(vec!["openai/gpt-6-astra".to_string()]),
    };
    assert!(matches!(
        request("opus", "kimi").validate(&configured, &strict).unwrap_err(),
        RunModelsError::ReviewerNotAllowed { .. }
    ));
}

#[test]
fn the_launch_arguments_are_the_detached_run_command_with_every_value_kept_whole() {
    let mut launch = request("sonnet", "kimi");
    launch.feature = "--detach".to_string();

    let args: Vec<OsString> = launch.args();

    let expected: Vec<OsString> = [
        "run", "--repo", "/work/app", "--feature", "--detach", "--remote", "origin", "--target", "main",
        "--implementer", "sonnet", "--reviewer", "kimi", "--detach",
    ]
    .iter()
    .map(OsString::from)
    .collect();
    assert_eq!(args, expected);
}

#[test]
fn extra_context_is_passed_whole_as_one_argument_before_the_detach_flag() {
    let mut launch = request("sonnet", "kimi");
    launch.context = Some("--feature x; echo hi".to_string());
    let args = launch.args();
    let at = args.iter().position(|a| a == "--context").expect("a context flag");
    assert_eq!(args[at + 1], OsString::from("--feature x; echo hi"));
    assert_eq!(args.last().unwrap(), &OsString::from("--detach"));

    launch.context = Some("  ".to_string());
    assert!(!launch.args().iter().any(|a| a == "--context"), "blank context adds nothing");
}

#[test]
fn an_issue_is_passed_as_its_own_argument_before_the_detach_flag() {
    let mut launch = request("sonnet", "kimi");
    launch.issue = Some("#42".to_string());
    let args = launch.args();
    let at = args.iter().position(|a| a == "--issue").expect("an issue flag");
    assert_eq!(args[at + 1], OsString::from("#42"));
    assert_eq!(args.last().unwrap(), &OsString::from("--detach"));

    launch.issue = Some(" ".to_string());
    assert!(!launch.args().iter().any(|a| a == "--issue"), "blank issue adds nothing");
}
