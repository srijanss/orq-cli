//! The `[design]` table of `.orq/run.toml`: the frontend run's browser and loop settings.

use std::fs;
use std::time::Duration;

use orq_orchestrator::{load_run_settings, DesignMode, DesignSettings, Viewport};
use orq_test_support::GitFixture;

fn shared(repo: &GitFixture, text: &str) {
    fs::create_dir_all(repo.root().join(".orq")).unwrap();
    fs::write(repo.root().join(".orq/run.toml"), text).unwrap();
}

fn private(repo: &GitFixture, text: &str) {
    fs::create_dir_all(repo.root().join(".git/orq")).unwrap();
    fs::write(repo.root().join(".git/orq/run.toml"), text).unwrap();
}

fn design(repo: &GitFixture, env: &dyn Fn(&str) -> Option<String>) -> DesignSettings {
    load_run_settings(repo.root(), env)
        .map_err(|e| e.to_string())
        .unwrap()
        .design
        .expect("a [design] table gives design settings")
}

fn refusal(text: &str) -> String {
    let repo = GitFixture::new();
    shared(&repo, text);
    match load_run_settings(repo.root(), &|_| None) {
        Ok(settings) => panic!("accepted: {:?}", settings.design),
        Err(err) => err.to_string(),
    }
}

fn env_refusal(text: &str, name: &'static str, value: &'static str) -> String {
    let repo = GitFixture::new();
    shared(&repo, text);
    let env = move |key: &str| (key == name).then(|| value.to_string());
    match load_run_settings(repo.root(), &env) {
        Ok(settings) => panic!("accepted: {:?}", settings.design),
        Err(err) => err.to_string(),
    }
}

fn vp(width: u32, height: u32) -> Viewport {
    Viewport { width, height }
}

#[test]
fn an_empty_design_table_gets_the_defaults_and_no_start_command() {
    let repo = GitFixture::new();
    shared(&repo, "[design]\n");

    let got = design(&repo, &|_| None);

    assert_eq!(got.start_command, None);
    assert_eq!(got.ready_path, "/");
    assert_eq!(got.ready_timeout, Duration::from_secs(60));
    assert_eq!(got.max_correction_rounds, 3);
    assert_eq!(got.verify_after_rounds, 2);
    assert_eq!(got.mode, DesignMode::Prepared);
    assert_eq!(got.viewports.len(), 3);
    assert_eq!(got.viewports["desktop"], vp(1440, 900));
    assert_eq!(got.viewports["tablet"], vp(768, 1024));
    assert_eq!(got.viewports["mobile"], vp(390, 844));
}

#[test]
fn a_project_without_a_design_table_loads_as_today_even_with_design_variables_set() {
    let repo = GitFixture::new();
    shared(&repo, "gate = [\"cargo\", \"test\"]\n");
    let env = |key: &str| (key == "ORQ_DESIGN_MODE").then(|| "infer".to_string());

    let settings = load_run_settings(repo.root(), &env).unwrap();

    assert_eq!(settings.design, None);
    assert_eq!(
        settings.gate,
        Some(vec!["cargo".to_string(), "test".to_string()])
    );
}

#[test]
fn every_key_is_read_from_the_shared_file() {
    let repo = GitFixture::new();
    shared(
        &repo,
        r#"[design]
start_command = ["npm", "run", "dev", "--", "--port", "{port}"]
ready_path = "/health"
ready_timeout_secs = 30
max_correction_rounds = 1
verify_after_rounds = 1
mode = "infer"
[design.viewports]
desktop = "1280x800"
wide = "1920x1080"
"#,
    );

    let got = design(&repo, &|_| None);

    assert_eq!(
        got.start_command,
        Some(
            ["npm", "run", "dev", "--", "--port", "{port}"]
                .map(String::from)
                .to_vec()
        )
    );
    assert_eq!(got.ready_path, "/health");
    assert_eq!(got.ready_timeout, Duration::from_secs(30));
    assert_eq!(got.max_correction_rounds, 1);
    assert_eq!(got.verify_after_rounds, 1);
    assert_eq!(got.mode, DesignMode::Infer);
    assert_eq!(got.viewports["desktop"], vp(1280, 800));
    assert_eq!(got.viewports["wide"], vp(1920, 1080));
    assert_eq!(got.viewports["mobile"], vp(390, 844), "built-ins stay");
}

const SHARED_DESIGN: &str = r#"[design]
start_command = ["npm", "start"]
ready_path = "/shared"
ready_timeout_secs = 10
max_correction_rounds = 3
verify_after_rounds = 2
mode = "prepared"
[design.viewports]
desktop = "1000x700"
"#;

#[test]
fn the_private_file_overrides_each_design_key() {
    let repo = GitFixture::new();
    shared(&repo, SHARED_DESIGN);
    private(
        &repo,
        r#"[design]
start_command = ["pnpm", "dev", "--port={port}"]
ready_path = "/private"
ready_timeout_secs = 20
max_correction_rounds = 2
verify_after_rounds = 1
mode = "infer"
[design.viewports]
tablet = "800x1000"
"#,
    );

    let got = design(&repo, &|_| None);

    assert_eq!(
        got.start_command,
        Some(["pnpm", "dev", "--port={port}"].map(String::from).to_vec())
    );
    assert_eq!(got.ready_path, "/private");
    assert_eq!(got.ready_timeout, Duration::from_secs(20));
    assert_eq!(got.max_correction_rounds, 2);
    assert_eq!(got.verify_after_rounds, 1);
    assert_eq!(got.mode, DesignMode::Infer);
    assert_eq!(got.viewports["tablet"], vp(800, 1000));
    assert_eq!(
        got.viewports["desktop"],
        vp(1000, 700),
        "the shared size stays for other names"
    );
}

#[test]
fn a_design_table_in_the_private_file_alone_is_enough() {
    let repo = GitFixture::new();
    private(&repo, "[design]\nmode = \"infer\"\n");

    assert_eq!(design(&repo, &|_| None).mode, DesignMode::Infer);
}

#[test]
fn the_environment_overrides_each_design_key() {
    let repo = GitFixture::new();
    shared(&repo, SHARED_DESIGN);
    private(&repo, "[design]\nready_path = \"/private\"\n");
    let env = |key: &str| {
        match key {
            "ORQ_DESIGN_START_COMMAND" => Some(r#"["yarn", "dev", "{port}"]"#),
            "ORQ_DESIGN_READY_PATH" => Some("/env"),
            "ORQ_DESIGN_READY_TIMEOUT_SECS" => Some("45"),
            "ORQ_DESIGN_MAX_CORRECTION_ROUNDS" => Some("0"),
            "ORQ_DESIGN_VERIFY_AFTER_ROUNDS" => Some("3"),
            "ORQ_DESIGN_MODE" => Some("infer"),
            "ORQ_DESIGN_VIEWPORTS" => Some(r#"{"mobile": "400x900", "watch": "200x200"}"#),
            _ => None,
        }
        .map(String::from)
    };

    let got = design(&repo, &env);

    assert_eq!(
        got.start_command,
        Some(["yarn", "dev", "{port}"].map(String::from).to_vec())
    );
    assert_eq!(got.ready_path, "/env");
    assert_eq!(got.ready_timeout, Duration::from_secs(45));
    assert_eq!(got.max_correction_rounds, 0);
    assert_eq!(got.verify_after_rounds, 3);
    assert_eq!(got.mode, DesignMode::Infer);
    assert_eq!(got.viewports["mobile"], vp(400, 900));
    assert_eq!(got.viewports["watch"], vp(200, 200));
    assert_eq!(got.viewports["desktop"], vp(1000, 700));
}

#[test]
fn the_start_command_gets_the_run_port_in_place_of_the_placeholder() {
    let repo = GitFixture::new();
    shared(
        &repo,
        "[design]\nstart_command = [\"npm\", \"run\", \"dev\", \"--port={port}\", \"{port}\"]\n",
    );

    let argv = design(&repo, &|_| None).start_argv(5173).unwrap();

    assert_eq!(
        argv,
        ["npm", "run", "dev", "--port=5173", "5173"]
            .map(String::from)
            .to_vec()
    );
}

#[test]
fn a_frontend_run_without_a_start_command_gets_a_clear_error_naming_the_key() {
    let repo = GitFixture::new();
    shared(&repo, "[design]\n");

    let error = design(&repo, &|_| None).start_argv(5173).unwrap_err();

    assert!(error.contains("design.start_command"), "{error}");
}

#[test]
fn an_unknown_design_key_is_refused_by_name() {
    let error = refusal("[design]\nstart_cmd = [\"npm\"]\n");
    assert!(error.contains("design.start_cmd"), "{error}");
    assert!(error.contains(".orq/run.toml"), "{error}");
}

#[test]
fn a_design_value_that_is_not_a_table_is_refused() {
    let error = refusal("design = \"on\"\n");
    assert!(error.contains("design must be a table"), "{error}");
}

#[test]
fn a_round_count_above_three_is_refused_by_name() {
    let error = refusal("[design]\nmax_correction_rounds = 4\n");
    assert!(error.contains("design.max_correction_rounds"), "{error}");
    let error = refusal("[design]\nverify_after_rounds = 4\n");
    assert!(error.contains("design.verify_after_rounds"), "{error}");
    let error = refusal("[design]\nmax_correction_rounds = -1\n");
    assert!(error.contains("design.max_correction_rounds"), "{error}");
    let error = env_refusal("[design]\n", "ORQ_DESIGN_MAX_CORRECTION_ROUNDS", "9");
    assert!(
        error.contains("ORQ_DESIGN_MAX_CORRECTION_ROUNDS"),
        "{error}"
    );
}

#[test]
fn an_empty_start_command_or_a_shell_string_is_refused_by_name() {
    let error = refusal("[design]\nstart_command = []\n");
    assert!(error.contains("design.start_command"), "{error}");
    let error = refusal("[design]\nstart_command = \"npm run dev\"\n");
    assert!(error.contains("design.start_command"), "{error}");
    let error = env_refusal("[design]\n", "ORQ_DESIGN_START_COMMAND", "[]");
    assert!(error.contains("ORQ_DESIGN_START_COMMAND"), "{error}");
}

#[test]
fn a_ready_path_that_is_not_a_path_is_refused_by_name() {
    for bad in [
        "health",
        "http://example.com/",
        "//evil.example/",
        "/a b",
        "",
    ] {
        let error = refusal(&format!("[design]\nready_path = {bad:?}\n"));
        assert!(error.contains("design.ready_path"), "{bad}: {error}");
    }
    let error = env_refusal("[design]\n", "ORQ_DESIGN_READY_PATH", "health");
    assert!(error.contains("ORQ_DESIGN_READY_PATH"), "{error}");
}

#[test]
fn a_placeholder_other_than_port_is_refused_by_name() {
    let error = refusal("[design]\nstart_command = [\"npm\", \"--host={host}\"]\n");
    assert!(error.contains("design.start_command"), "{error}");
    assert!(error.contains("{host}"), "{error}");
    let error = env_refusal(
        "[design]\n",
        "ORQ_DESIGN_START_COMMAND",
        r#"["npm", "{worktree}"]"#,
    );
    assert!(error.contains("ORQ_DESIGN_START_COMMAND"), "{error}");
}

#[test]
fn a_viewport_size_outside_the_bounds_or_badly_shaped_is_refused_by_name() {
    for bad in ["199x900", "1440x4001", "1440", "1440X900", "wide", "0x0"] {
        let error = refusal(&format!("[design.viewports]\ndesktop = {bad:?}\n"));
        assert!(error.contains("design.viewports.desktop"), "{bad}: {error}");
    }
    let error = refusal("[design.viewports]\n\"bad name\" = \"800x600\"\n");
    assert!(error.contains("design.viewports"), "{error}");
    let error = env_refusal(
        "[design]\n",
        "ORQ_DESIGN_VIEWPORTS",
        r#"{"desktop": "10x10"}"#,
    );
    assert!(error.contains("ORQ_DESIGN_VIEWPORTS"), "{error}");
}

#[test]
fn other_bad_design_values_are_refused_by_name() {
    let error = refusal("[design]\nmode = \"guess\"\n");
    assert!(error.contains("design.mode"), "{error}");
    let error = refusal("[design]\nready_timeout_secs = 0\n");
    assert!(error.contains("design.ready_timeout_secs"), "{error}");
    let error = env_refusal("[design]\n", "ORQ_DESIGN_MODE", "guess");
    assert!(error.contains("ORQ_DESIGN_MODE"), "{error}");
}
