use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use orq_orchestrator::{load_run_settings, SettingsError};

fn repo(tag: &str) -> PathBuf {
    let dir = orq_test_support::process_temp_dir().join(format!("orq-run-settings-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let out = std::process::Command::new("git").arg("-C").arg(&dir).args(["init", "-q", "-b", "main"]).output().unwrap();
    assert!(out.status.success());
    dir
}

fn shared(repo: &Path, text: &str) {
    fs::create_dir_all(repo.join(".orq")).unwrap();
    fs::write(repo.join(".orq/run.toml"), text).unwrap();
}

fn private(repo: &Path, text: &str) {
    fs::create_dir_all(repo.join(".git/orq")).unwrap();
    fs::write(repo.join(".git/orq/run.toml"), text).unwrap();
}

fn no_env(_: &str) -> Option<String> {
    None
}

#[test]
fn a_repo_without_settings_gets_none() {
    let r = repo("none");
    let s = load_run_settings(&r, &no_env).unwrap();
    assert_eq!((s.forge, s.base_branch, s.gate, s.ci_timeout), (None, None, None, None));
    assert!(s.setup.is_empty());
}

#[test]
fn the_checked_in_file_is_read() {
    let r = repo("shared");
    shared(
        &r,
        "forge = \"glab\"\nbase_branch = \"develop\"\ngate = [\"make\", \"check\"]\nsetup = [[\"npm\", \"ci\"], [\"make\", \"deps\"]]\nci_timeout_secs = 600\n",
    );
    let s = load_run_settings(&r, &no_env).unwrap();
    assert_eq!(s.forge.as_deref(), Some("glab"));
    assert_eq!(s.base_branch.as_deref(), Some("develop"));
    assert_eq!(s.gate, Some(vec!["make".to_string(), "check".to_string()]));
    assert_eq!(s.setup, vec![vec!["npm".to_string(), "ci".to_string()], vec!["make".to_string(), "deps".to_string()]]);
    assert_eq!(s.ci_timeout, Some(Duration::from_secs(600)));
}

#[test]
fn the_private_file_wins_key_by_key_and_the_environment_wins_over_both() {
    let r = repo("precedence");
    shared(&r, "forge = \"gh\"\nbase_branch = \"develop\"\n");
    private(&r, "base_branch = \"release\"\n");
    let s = load_run_settings(&r, &no_env).unwrap();
    assert_eq!((s.forge.as_deref(), s.base_branch.as_deref()), (Some("gh"), Some("release")));

    let env = |key: &str| match key {
        "ORQ_BASE_BRANCH" => Some("hotfix".to_string()),
        "ORQ_GATE" => Some("[\"just\",\"ci\"]".to_string()),
        "ORQ_CI_TIMEOUT_SECS" => Some("90".to_string()),
        "ORQ_FORGE" => Some("glab".to_string()),
        _ => None,
    };
    let s = load_run_settings(&r, &env).unwrap();
    assert_eq!((s.forge.as_deref(), s.base_branch.as_deref()), (Some("glab"), Some("hotfix")));
    assert_eq!(s.gate, Some(vec!["just".to_string(), "ci".to_string()]));
    assert_eq!(s.ci_timeout, Some(Duration::from_secs(90)));
}

fn invalid(text: &str, tag: &str) -> String {
    let r = repo(tag);
    shared(&r, text);
    match load_run_settings(&r, &no_env) {
        Err(SettingsError::Invalid { source, reason }) => format!("{source}: {reason}"),
        other => panic!("{text:?} should be invalid, got {other:?}"),
    }
}

#[test]
fn bad_values_are_refused_and_say_where_they_came_from() {
    assert!(invalid("forge = \"bitbucket\"\n", "forge").contains(".orq/run.toml"));
    assert!(invalid("base_branch = \"-x\"\n", "dash").contains("base_branch"));
    assert!(invalid("base_branch = \"a b\"\n", "space").contains("base_branch"));
    assert!(invalid("base_branch = \"release//next\"\n", "doubleslash").contains("base_branch"));
    assert!(invalid("base_branch = \"release/.hidden\"\n", "dotcomponent").contains("base_branch"));
    assert!(invalid("base_branch = \"release.lock\"\n", "lock").contains("base_branch"));
    assert!(invalid("gate = []\n", "emptygate").contains("gate"));
    assert!(invalid("gate = [\"\"]\n", "blankgate").contains("gate"));
    assert!(invalid("gate = \"make check\"\n", "stringgate").contains("gate"));
    assert!(invalid("setup = [[]]\n", "emptysetup").contains("setup"));
    assert!(invalid("ci_timeout_secs = 0\n", "zero").contains("ci_timeout_secs"));
    assert!(invalid("ci_timeout_secs = 999999999\n", "huge").contains("ci_timeout_secs"));
    assert!(invalid("colour = \"red\"\n", "unknown").contains("colour"));
    assert!(invalid("forge = ", "syntax").contains(".orq/run.toml"));
}

#[test]
fn a_bad_environment_value_is_refused_by_name() {
    let r = repo("badenv");
    let env = |key: &str| (key == "ORQ_GATE").then(|| "make check".to_string());
    match load_run_settings(&r, &env) {
        Err(SettingsError::Invalid { source, .. }) => assert_eq!(source, "ORQ_GATE"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn branch_names_git_accepts_are_accepted() {
    for name in ["release+fix", "feature/ünï", "v1.2", "team/x_y"] {
        let r = repo(&format!("ok-{}", name.len()));
        shared(&r, &format!("base_branch = \"{name}\"\n"));
        let s = load_run_settings(&r, &no_env).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(s.base_branch.as_deref(), Some(name));
    }
}

#[test]
fn the_people_for_a_review_request_come_from_the_files_and_the_environment() {
    let r = repo("people");
    shared(&r, "mr_assignee = \"alice\"\nmr_reviewer = \"bob\"\n");
    let s = load_run_settings(&r, &no_env).unwrap();
    assert_eq!((s.mr_assignee.as_deref(), s.mr_reviewer.as_deref()), (Some("alice"), Some("bob")));

    let env = |key: &str| (key == "ORQ_MR_REVIEWER").then(|| "carol".to_string());
    let s = load_run_settings(&r, &env).unwrap();
    assert_eq!((s.mr_assignee.as_deref(), s.mr_reviewer.as_deref()), (Some("alice"), Some("carol")));

    assert!(invalid("mr_assignee = \"-x\"\n", "badassignee").contains("mr_assignee"));
    assert!(invalid("mr_reviewer = \"a b\"\n", "badreviewer").contains("mr_reviewer"));
    assert!(invalid("mr_reviewer = \" \"\n", "blankreviewer").contains("mr_reviewer"));
    let bad = |key: &str| (key == "ORQ_MR_ASSIGNEE").then(|| "a,b".to_string());
    match load_run_settings(&r, &bad) {
        Err(SettingsError::Invalid { source, .. }) => assert_eq!(source, "ORQ_MR_ASSIGNEE"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_notification_command_and_mode_are_settings() {
    let r = repo("notify");
    shared(&r, "notify_cmd = [\"notify-send\", \"orq\"]\nnotify_on = \"failures\"\n");
    let s = load_run_settings(&r, &no_env).unwrap();
    assert_eq!(s.notify_cmd, Some(vec!["notify-send".to_string(), "orq".to_string()]));
    assert_eq!(s.notify_on.as_deref(), Some("failures"));

    let env = |key: &str| match key {
        "ORQ_NOTIFY_CMD" => Some("[\"say\"]".to_string()),
        "ORQ_NOTIFY_ON" => Some("all".to_string()),
        _ => None,
    };
    let s = load_run_settings(&r, &env).unwrap();
    assert_eq!((s.notify_cmd, s.notify_on.as_deref()), (Some(vec!["say".to_string()]), Some("all")));

    assert!(invalid("notify_cmd = \"say hi\"\n", "notifystring").contains("notify_cmd"));
    assert!(invalid("notify_cmd = []\n", "notifyempty").contains("notify_cmd"));
    assert!(invalid("notify_on = \"sometimes\"\n", "notifymode").contains("notify_on"));
}

#[test]
fn the_variables_a_notifier_may_keep_are_a_setting() {
    let r = repo("notifyenv");
    shared(&r, "notify_env = [\"SLACK_WEBHOOK_URL\", \"TOKEN_2\"]\n");
    let s = load_run_settings(&r, &no_env).unwrap();
    assert_eq!(s.notify_env, Some(vec!["SLACK_WEBHOOK_URL".to_string(), "TOKEN_2".to_string()]));

    let env = |key: &str| (key == "ORQ_NOTIFY_ENV").then(|| "[\"HOOK\"]".to_string());
    assert_eq!(load_run_settings(&r, &env).unwrap().notify_env, Some(vec!["HOOK".to_string()]));

    assert!(invalid("notify_env = \"HOOK\"\n", "envstring").contains("notify_env"));
    assert!(invalid("notify_env = [\"A=B\"]\n", "envequals").contains("notify_env"));
    assert!(invalid("notify_env = [\"1X\"]\n", "envdigit").contains("notify_env"));
    assert!(invalid("notify_env = [\"NOTIFY_MESSAGE\"]\n", "envmessage").contains("notify_env"));
}

#[test]
fn the_variables_that_name_test_databases_come_from_the_files_or_the_environment_and_must_be_variable_names() {
    let r = repo("test-db-vars");
    assert_eq!(load_run_settings(&r, &no_env).unwrap().test_db_vars, None);

    shared(&r, "test_db_vars = [\"DB_NAME\"]\n");
    private(&r, "test_db_vars = [\"DB_NAME\", \"OTHER_DB\"]\n");
    let s = load_run_settings(&r, &no_env).unwrap();
    assert_eq!(s.test_db_vars, Some(vec!["DB_NAME".to_string(), "OTHER_DB".to_string()]));

    let env = |name: &str| (name == "ORQ_TEST_DB_VARS").then(|| "[\"ENV_DB\"]".to_string());
    assert_eq!(load_run_settings(&r, &env).unwrap().test_db_vars, Some(vec!["ENV_DB".to_string()]));

    shared(&r, "test_db_vars = [\"not a name\"]\n");
    private(&r, "");
    assert!(matches!(load_run_settings(&r, &no_env), Err(SettingsError::Invalid { .. })));
}

#[test]
fn the_test_database_drop_command_and_keep_flag_come_from_the_files_or_the_environment() {
    let r = repo("dropcmd");
    shared(&r, "test_db_drop_cmd = [\"mysql-drop\", \"--yes\"]\nkeep_test_db = false\n");
    let s = load_run_settings(&r, &no_env).unwrap();
    assert_eq!(s.test_db_drop_cmd, Some(vec!["mysql-drop".to_string(), "--yes".to_string()]));
    assert_eq!(s.keep_test_db, Some(false));

    let env = |key: &str| match key {
        "ORQ_TEST_DB_DROP_CMD" => Some("[\"other\"]".to_string()),
        "ORQ_KEEP_TEST_DB" => Some("1".to_string()),
        _ => None,
    };
    let s = load_run_settings(&r, &env).unwrap();
    assert_eq!((s.test_db_drop_cmd, s.keep_test_db), (Some(vec!["other".to_string()]), Some(true)));

    assert!(invalid("test_db_drop_cmd = \"drop it\"\n", "dropstring").contains("test_db_drop_cmd"));
    assert!(invalid("keep_test_db = \"yes\"\n", "keepstring").contains("keep_test_db"));
    let bad = |key: &str| (key == "ORQ_KEEP_TEST_DB").then(|| "maybe".to_string());
    assert!(matches!(load_run_settings(&r, &bad), Err(SettingsError::Invalid { source, .. }) if source == "ORQ_KEEP_TEST_DB"));
}

#[test]
fn the_database_name_template_comes_from_the_files_or_the_environment() {
    let r = repo("dbtemplate");
    shared(&r, "db_name_template = \"wt_{branch}__{name}\"\n");
    assert_eq!(load_run_settings(&r, &no_env).unwrap().db_name_template.as_deref(), Some("wt_{branch}__{name}"));

    let env = |key: &str| (key == "ORQ_DB_NAME_TEMPLATE").then(|| "{name}_x_{branch}".to_string());
    assert_eq!(load_run_settings(&r, &env).unwrap().db_name_template.as_deref(), Some("{name}_x_{branch}"));

    assert!(invalid("db_name_template = 3\n", "dbtemplatenum").contains("db_name_template"));
}

#[test]
fn the_run_model_comes_from_the_files_or_the_environment_and_must_be_a_plain_name() {
    let r = repo("model");
    assert_eq!(load_run_settings(&r, &no_env).unwrap().model, None);
    shared(&r, "model = \"sonnet\"\n");
    assert_eq!(load_run_settings(&r, &no_env).unwrap().model.as_deref(), Some("sonnet"));

    let env = |key: &str| (key == "ORQ_MODEL").then(|| "opus".to_string());
    assert_eq!(load_run_settings(&r, &env).unwrap().model.as_deref(), Some("opus"));

    assert!(invalid("model = 3\n", "modelnum").contains("model"));
    assert!(invalid("model = \"--dangerous\"\n", "modelopt").contains("model"));
    assert!(invalid("model = \"two words\"\n", "modelspace").contains("model"));
    assert!(invalid("model = \"\"\n", "modelblank").contains("model"));
    // The agent adapter's own rule applies, so nothing passes here that fails after the run is created.
    assert!(invalid("model = \"foo$bar\"\n", "modeldollar").contains("model"));
    assert!(invalid("model = \"a\\u0000b\"\n", "modelnul").contains("model"));
    assert!(invalid("model = \"opus;rm\"\n", "modelsemi").contains("model"));
    shared(&r, "model = \"anthropic/claude-sonnet-5-5:1m@x+y\"\n");
    assert_eq!(load_run_settings(&r, &no_env).unwrap().model.as_deref(), Some("anthropic/claude-sonnet-5-5:1m@x+y"));
}

#[test]
fn the_compact_window_comes_from_the_files_or_the_environment_and_must_be_a_sane_token_count() {
    let r = repo("compact");
    assert_eq!(load_run_settings(&r, &no_env).unwrap().compact_window, None);
    shared(&r, "compact_window = 150000\n");
    assert_eq!(load_run_settings(&r, &no_env).unwrap().compact_window, Some(150_000));

    let env = |key: &str| (key == "ORQ_COMPACT_WINDOW").then(|| "200000".to_string());
    assert_eq!(load_run_settings(&r, &env).unwrap().compact_window, Some(200_000));

    assert!(invalid("compact_window = \"big\"\n", "compactstr").contains("compact_window"));
    assert!(invalid("compact_window = 0\n", "compactzero").contains("compact_window"));
    assert!(invalid("compact_window = -5\n", "compactneg").contains("compact_window"));
    assert!(invalid("compact_window = 99999999999\n", "compacthuge").contains("compact_window"));
    let bad_env = |key: &str| (key == "ORQ_COMPACT_WINDOW").then(|| "lots".to_string());
    assert!(load_run_settings(&r, &bad_env).unwrap_err().to_string().contains("ORQ_COMPACT_WINDOW"));
}

#[test]
fn the_branch_prefix_comes_from_the_files_or_the_environment_and_must_be_usable() {
    let r = repo("prefix");
    assert_eq!(load_run_settings(&r, &no_env).unwrap().branch_prefix, None);
    shared(&r, "branch_prefix = \"feat/\"\n");
    assert_eq!(load_run_settings(&r, &no_env).unwrap().branch_prefix.as_deref(), Some("feat/"));

    let env = |key: &str| (key == "ORQ_BRANCH_PREFIX").then(|| "bug-".to_string());
    assert_eq!(load_run_settings(&r, &env).unwrap().branch_prefix.as_deref(), Some("bug-"));

    assert!(invalid("branch_prefix = 3\n", "prefixnum").contains("branch_prefix"));
    assert!(invalid("branch_prefix = \"../x\"\n", "prefixdots").contains("branch_prefix"));
    assert!(invalid("branch_prefix = \"-x\"\n", "prefixdash").contains("branch_prefix"));
    assert!(invalid("branch_prefix = \"a b\"\n", "prefixspace").contains("branch_prefix"));

    // A bad value from the environment names the variable; an empty prefix is a choice, not "unset".
    let bad_env = |key: &str| (key == "ORQ_BRANCH_PREFIX").then(|| "../x".to_string());
    assert!(load_run_settings(&r, &bad_env).unwrap_err().to_string().contains("ORQ_BRANCH_PREFIX"));
    let r = repo("prefix-empty");
    shared(&r, "branch_prefix = \"\"\n");
    assert_eq!(load_run_settings(&r, &no_env).unwrap().branch_prefix.as_deref(), Some(""));
}

#[test]
fn saving_mcp_servers_writes_the_private_file_keeping_its_other_keys_and_comments() {
    let r = repo("save-mcp");
    private(&r, "# mine\nbase_branch = \"release\"\n");

    orq_orchestrator::save_mcp_servers_setting(&r, &["project-mcp".to_string(), "outside-in-tdd-mcp".to_string()]).unwrap();

    let text = fs::read_to_string(r.join(".git/orq/run.toml")).unwrap();
    assert!(text.contains("# mine") && text.contains("base_branch"), "{text}");
    let s = load_run_settings(&r, &no_env).unwrap();
    assert_eq!(s.mcp_servers, Some(vec!["project-mcp".to_string(), "outside-in-tdd-mcp".to_string()]));
    assert_eq!(s.base_branch.as_deref(), Some("release"));
}

#[test]
fn saving_mcp_servers_creates_the_private_file_and_replaces_an_earlier_choice() {
    let r = repo("save-mcp-new");
    orq_orchestrator::save_mcp_servers_setting(&r, &["a".to_string()]).unwrap();
    orq_orchestrator::save_mcp_servers_setting(&r, &["b".to_string()]).unwrap();

    assert_eq!(load_run_settings(&r, &no_env).unwrap().mcp_servers, Some(vec!["b".to_string()]));
}

#[test]
fn a_malformed_private_file_is_left_untouched_when_saving_mcp_servers() {
    let r = repo("save-mcp-bad");
    private(&r, "not = [toml");

    let err = orq_orchestrator::save_mcp_servers_setting(&r, &["a".to_string()]).unwrap_err();

    assert!(err.contains("run.toml"), "{err}");
    assert_eq!(fs::read_to_string(r.join(".git/orq/run.toml")).unwrap(), "not = [toml");
}

#[test]
fn saving_mcp_servers_outside_a_repository_fails() {
    let dir = orq_test_support::process_temp_dir().join(format!("orq-run-settings-{}-nogit", std::process::id()));
    fs::create_dir_all(&dir).unwrap();

    assert!(orq_orchestrator::save_mcp_servers_setting(&dir, &["a".to_string()]).is_err());
}

#[test]
fn saving_mcp_servers_never_writes_through_a_planted_temp_link() {
    use std::os::unix::fs::symlink;
    let r = repo("save-mcp-link");
    private(&r, "base_branch = \"release\"\n");
    let victim = r.join("victim.txt");
    fs::write(&victim, "keep\n").unwrap();
    let planted = r.join(format!(".git/orq/run.toml.{}.0.tmp", std::process::id()));
    symlink(&victim, &planted).unwrap();

    orq_orchestrator::save_mcp_servers_setting(&r, &["a".to_string()]).unwrap();

    assert_eq!(fs::read_to_string(&victim).unwrap(), "keep\n");
    assert!(fs::symlink_metadata(&planted).unwrap().file_type().is_symlink(), "the planted link was replaced");
    assert_eq!(load_run_settings(&r, &no_env).unwrap().mcp_servers, Some(vec!["a".to_string()]));
}

#[test]
fn saving_mcp_servers_keeps_the_files_permissions_and_makes_a_new_file_private() {
    use std::os::unix::fs::PermissionsExt;
    let mode = |r: &Path| fs::metadata(r.join(".git/orq/run.toml")).unwrap().permissions().mode() & 0o777;

    let existing = repo("save-mcp-mode");
    private(&existing, "base_branch = \"release\"\n");
    fs::set_permissions(existing.join(".git/orq/run.toml"), fs::Permissions::from_mode(0o640)).unwrap();
    orq_orchestrator::save_mcp_servers_setting(&existing, &["a".to_string()]).unwrap();
    assert_eq!(mode(&existing), 0o640);

    let fresh = repo("save-mcp-mode-new");
    orq_orchestrator::save_mcp_servers_setting(&fresh, &["a".to_string()]).unwrap();
    assert_eq!(mode(&fresh), 0o600);
}

#[test]
fn saving_mcp_servers_leaves_no_temp_file_behind() {
    let r = repo("save-mcp-clean");
    orq_orchestrator::save_mcp_servers_setting(&r, &["a".to_string()]).unwrap();

    let names: Vec<String> = fs::read_dir(r.join(".git/orq")).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(names, vec!["run.toml".to_string()]);
}

/// Run by `saving_mcp_servers_keeps_the_mode_under_a_restrictive_umask` in a child process, since a umask
/// belongs to the whole process.
#[test]
fn restrictive_umask_child() {
    if std::env::var_os("ORQ_UMASK_CHILD").is_none() {
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    unsafe { libc::umask(0o077) };
    let r = repo("save-mcp-umask");
    private(&r, "base_branch = \"release\"\n");
    let path = r.join(".git/orq/run.toml");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
    orq_orchestrator::save_mcp_servers_setting(&r, &["a".to_string()]).unwrap();
    assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o640);
}

#[test]
fn saving_mcp_servers_keeps_the_mode_under_a_restrictive_umask() {
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["restrictive_umask_child", "--exact", "--nocapture"])
        .env("ORQ_UMASK_CHILD", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
}

#[test]
fn the_session_auto_exit_switch_comes_from_the_files_or_the_environment() {
    let r = repo("autoexit");
    assert_eq!(load_run_settings(&r, &no_env).unwrap().auto_exit, None);
    shared(&r, "auto_exit = false\n");
    assert_eq!(load_run_settings(&r, &no_env).unwrap().auto_exit, Some(false));

    let env = |key: &str| (key == "ORQ_AUTO_EXIT").then(|| "1".to_string());
    assert_eq!(load_run_settings(&r, &env).unwrap().auto_exit, Some(true));

    assert!(invalid("auto_exit = \"no\"\n", "autoexitstr").contains("auto_exit"));
    let bad = |key: &str| (key == "ORQ_AUTO_EXIT").then(|| "maybe".to_string());
    assert!(matches!(load_run_settings(&r, &bad), Err(SettingsError::Invalid { source, .. }) if source == "ORQ_AUTO_EXIT"));
}

#[test]
fn the_remote_control_and_failure_explainer_switches_come_from_the_files_or_the_environment() {
    let r = repo("remotectl");
    let s = load_run_settings(&r, &no_env).unwrap();
    assert_eq!((s.remote_control, s.explain_failures), (None, None));
    shared(&r, "remote_control = false\nexplain_failures = true\n");
    let s = load_run_settings(&r, &no_env).unwrap();
    assert_eq!((s.remote_control, s.explain_failures), (Some(false), Some(true)));

    let env = |key: &str| match key {
        "ORQ_REMOTE_CONTROL" => Some("1".to_string()),
        "ORQ_EXPLAIN_FAILURES" => Some("0".to_string()),
        _ => None,
    };
    let s = load_run_settings(&r, &env).unwrap();
    assert_eq!((s.remote_control, s.explain_failures), (Some(true), Some(false)));

    assert!(invalid("remote_control = \"yes\"\n", "rcstr").contains("remote_control"));
    assert!(invalid("explain_failures = 3\n", "efint").contains("explain_failures"));
    let bad = |key: &str| (key == "ORQ_EXPLAIN_FAILURES").then(|| "maybe".to_string());
    assert!(matches!(load_run_settings(&r, &bad), Err(SettingsError::Invalid { source, .. }) if source == "ORQ_EXPLAIN_FAILURES"));
}

#[test]
fn the_claude_push_switch_comes_from_the_files_or_the_environment() {
    let r = repo("claudepush");
    assert_eq!(load_run_settings(&r, &no_env).unwrap().claude_push, None);
    shared(&r, "claude_push = true\n");
    assert_eq!(load_run_settings(&r, &no_env).unwrap().claude_push, Some(true));
    let env = |key: &str| (key == "ORQ_CLAUDE_PUSH").then(|| "0".to_string());
    assert_eq!(load_run_settings(&r, &env).unwrap().claude_push, Some(false));

    assert!(invalid("claude_push = \"yes\"\n", "cpstr").contains("claude_push"));
    let bad = |key: &str| (key == "ORQ_CLAUDE_PUSH").then(|| "maybe".to_string());
    assert!(matches!(load_run_settings(&r, &bad), Err(SettingsError::Invalid { source, .. }) if source == "ORQ_CLAUDE_PUSH"));
}
