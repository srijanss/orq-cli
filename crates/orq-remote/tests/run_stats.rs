//! `/api/stats`: how orchestration is going, computed from the runs' events over 7, 30 or 90 days,
//! for every project or one. Each figure names the runs it counts, and links to them on the Runs list.

mod web_deps;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use orq_core::remote_inbox::{handle_with, RemoteConfig, Request};
use orq_core::storage::StorageRoots;
use orq_orchestrator::{Event, RunRecord, RunStore};
use orq_remote::projects::StoreProjects;
use serde_json::{json, Value};

fn git_repo(root: &Path, name: &str) -> PathBuf {
    let path = root.join(name);
    std::fs::create_dir_all(&path).unwrap();
    assert!(Command::new("git")
        .args(["init", "-q"])
        .current_dir(&path)
        .status()
        .unwrap()
        .success());
    path
}

struct Setup {
    scan: PathBuf,
    roots: StorageRoots,
}

fn setup(label: &str) -> Setup {
    let base = web_deps::private_temp_dir(label);
    let roots = StorageRoots {
        data: base.join("data"),
        config: base.join("config"),
        state: base.join("state"),
        cache: base.join("cache"),
    };
    roots.create_all().unwrap();
    Setup {
        scan: base.join("code"),
        roots,
    }
}

fn get_with(provider: &StoreProjects, s: &Setup, path: &str) -> (u16, Value) {
    let config = RemoteConfig {
        token: "tok".into(),
    };
    let request = Request {
        method: "GET",
        path,
        peer: "127.0.0.1".parse().unwrap(),
        authorization: Some("Bearer tok"),
    };
    let r = handle_with(
        &s.roots,
        &config,
        "server-project",
        "server",
        &request,
        provider,
    );
    (r.status, r.body)
}

fn get(s: &Setup, path: &str) -> (u16, Value) {
    get_with(
        &StoreProjects::new(s.roots.clone(), s.scan.clone()),
        s,
        path,
    )
}

fn project_id(repo: &Path) -> String {
    orq_git::project_id(&orq_git::resolve_project(repo).unwrap())
}

/// An RFC 3339 time `secs` seconds before now. "Now" is read once, so a fixture's offsets stay exact
/// when writing it crosses a second.
fn ago(secs: u64) -> String {
    static NOW: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    let now = *NOW.get_or_init(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
    });
    rfc3339(now - secs)
}

/// Like `ago`, but from the clock as it is now, for lines that must come after something that just happened.
fn ago_now(secs: u64) -> String {
    rfc3339(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            - secs,
    )
}

fn rfc3339(secs: u64) -> String {
    let secs = secs as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3_600,
        rem % 3_600 / 60,
        rem % 60
    )
}

/// One fixture event: its kind, stage and payload, `at` seconds after the run started.
struct E(&'static str, Option<&'static str>, u64, Value);

fn e(kind: &'static str, at: u64) -> E {
    E(kind, None, at, json!({}))
}

fn stage(kind: &'static str, stage: &'static str, at: u64) -> E {
    E(kind, Some(stage), at, json!({}))
}

fn with(kind: &'static str, at: u64, payload: Value) -> E {
    E(kind, None, at, payload)
}

/// A run of `project` that started `started_ago` seconds ago, with `events` at their offsets.
fn add_run(s: &Setup, run: &str, project: &str, started_ago: u64, events: Vec<E>) {
    let store = RunStore::new(&s.roots.state);
    store
        .create(&RunRecord {
            run_id: run.into(),
            project_id: project.into(),
            feature_id: format!("f-{run}"),
            remote: String::new(),
            provider: String::new(),
            target_branch: "main".into(),
            base_sha: "a".repeat(40),
            branch: format!("orq/{run}"),
            worktree: s.scan.join(run),
            repository: String::new(),
            profiles: Default::default(),
            delivery: Default::default(),
        })
        .unwrap();
    let mut all = vec![e("run.started", 0)];
    all.extend(events);
    for (n, E(kind, stage, at, payload)) in all.into_iter().enumerate() {
        let event = Event {
            schema_version: 1,
            event_id: format!("{run}-{n}"),
            run_id: run.into(),
            project_id: project.into(),
            feature_id: format!("f-{run}"),
            kind: kind.into(),
            stage: stage.map(String::from),
            attempt: 1,
            occurred_at: ago(started_ago - at),
            payload,
        };
        store.append_event(&event).unwrap();
    }
}

fn delivered(at: u64) -> E {
    with("run.completed", at, json!({"ending": "pushed"}))
}

const DAY: u64 = 86_400;

/// alpha: d1 delivered first try, d2 delivered after a gate fix, f1 failed at the gate, f2 failed in
/// implement, rr1 waited on a review and was continued as rr2 (review-again), which delivered. old is
/// ten days old. beta: b1 delivered.
fn seeded(label: &str) -> (Setup, String, String) {
    let s = setup(label);
    let alpha = project_id(&git_repo(&s.scan, "alpha"));
    let beta = project_id(&git_repo(&s.scan, "beta"));
    let implement = "implement_and_review";
    add_run(
        &s,
        "d1",
        &alpha,
        DAY,
        vec![
            stage("stage.started", implement, 10),
            stage("stage.completed", implement, 610),
            delivered(700),
        ],
    );
    add_run(
        &s,
        "d2",
        &alpha,
        DAY,
        vec![
            stage("stage.started", implement, 10),
            stage("stage.completed", implement, 1210),
            stage("stage.started", "gate", 1220),
            stage("stage.failed", "gate", 1300),
            stage("stage.started", "gate_fix", 1310),
            stage("stage.completed", "gate_fix", 1610),
            delivered(1700),
        ],
    );
    add_run(
        &s,
        "f1",
        &alpha,
        DAY,
        vec![
            stage("stage.started", implement, 10),
            stage("stage.completed", implement, 910),
            stage("stage.started", "gate", 920),
            stage("stage.failed", "gate", 1000),
            e("run.failed", 1001),
        ],
    );
    add_run(
        &s,
        "f2",
        &alpha,
        DAY,
        vec![
            stage("stage.started", implement, 10),
            stage("stage.failed", implement, 50),
            e("run.failed", 51),
        ],
    );
    add_run(
        &s,
        "rr1",
        &alpha,
        2 * DAY,
        vec![
            with("run.completed", 100, json!({"ending": "needs_review"})),
            with("run.resumed", 200, json!({"resumed_as": "rr2"})),
            with("run.completed", 201, json!({"ending": "resumed"})),
        ],
    );
    add_run(
        &s,
        "rr2",
        &alpha,
        2 * DAY - 300,
        vec![
            with("run.sessions_carried", 1, json!({"from": "rr1"})),
            delivered(100),
        ],
    );
    add_run(&s, "old", &alpha, 10 * DAY, vec![delivered(100)]);
    add_run(&s, "b1", &beta, DAY, vec![delivered(100)]);
    (s, alpha, beta)
}

fn runs(figure: &Value) -> Vec<String> {
    let mut ids: Vec<String> = figure["runs"]
        .as_array()
        .unwrap_or_else(|| panic!("no runs in {figure}"))
        .iter()
        .map(|r| r.as_str().unwrap().to_string())
        .collect();
    ids.sort();
    ids
}

fn by_stage<'a>(list: &'a Value, name: &str) -> &'a Value {
    list.as_array()
        .unwrap()
        .iter()
        .find(|row| row["stage"] == name)
        .unwrap_or_else(|| panic!("no {name} in {list}"))
}

#[test]
fn runs_are_counted_by_outcome_over_the_window_for_one_project() {
    let (s, _, _) = seeded("stats-outcomes");
    let (status, body) = get(&s, "/api/stats?project=alpha&days=7");
    assert_eq!(status, 200, "{body}");
    let outcomes = &body["outcomes"];
    assert_eq!(outcomes["started"]["count"], 6, "{body}");
    assert_eq!(
        runs(&outcomes["started"]),
        ["d1", "d2", "f1", "f2", "rr1", "rr2"],
        "{body}"
    );
    assert_eq!(runs(&outcomes["delivered"]), ["d1", "d2", "rr2"], "{body}");
    assert_eq!(runs(&outcomes["failed"]), ["f1", "f2"], "{body}");
    assert_eq!(outcomes["discarded"]["count"], 0, "{body}");
}

#[test]
fn first_try_success_leaves_out_runs_with_a_gate_fix_or_a_review_again() {
    let (s, _, _) = seeded("stats-first-try");
    let (_, body) = get(&s, "/api/stats?project=alpha&days=7");
    assert_eq!(runs(&body["first_try"]), ["d1"], "{body}");
    assert_eq!(body["first_try"]["of"], 3, "{body}");
}

#[test]
fn failures_are_grouped_by_the_stage_the_run_failed_in() {
    let (s, _, _) = seeded("stats-failures");
    let (_, body) = get(&s, "/api/stats?project=alpha&days=7");
    let failures = &body["failures_by_stage"];
    assert_eq!(failures.as_array().unwrap().len(), 2, "{body}");
    assert_eq!(runs(by_stage(failures, "gate")), ["f1"], "{body}");
    assert_eq!(
        runs(by_stage(failures, "implement_and_review")),
        ["f2"],
        "{body}"
    );
}

#[test]
fn stage_times_give_the_median_and_the_90th_percentile() {
    let (s, _, _) = seeded("stats-percentiles");
    let (_, body) = get(&s, "/api/stats?project=alpha&days=7");
    let implement = by_stage(&body["stage_times"], "implement_and_review");
    // 600, 1200, 900 and 40 seconds.
    assert_eq!(implement["count"], 4, "{body}");
    assert_eq!(implement["median_secs"], 600, "{body}");
    assert_eq!(implement["p90_secs"], 1200, "{body}");
    assert_eq!(runs(implement), ["d1", "d2", "f1", "f2"], "{body}");
    assert_eq!(
        by_stage(&body["stage_times"], "gate_fix")["median_secs"],
        300,
        "{body}"
    );
}

#[test]
fn gate_fix_attempts_are_counted_per_run() {
    let (s, _, _) = seeded("stats-gate-fix");
    let (_, body) = get(&s, "/api/stats?project=alpha&days=7");
    let attempts = body["gate_fix_attempts"].as_array().unwrap();
    let row = |n: u64| {
        attempts
            .iter()
            .find(|row| row["attempts"] == n)
            .unwrap_or_else(|| panic!("no {n} in {body}"))
    };
    assert_eq!(runs(row(0)), ["d1", "f1", "f2", "rr1", "rr2"], "{body}");
    assert_eq!(runs(row(1)), ["d2"], "{body}");
}

#[test]
fn review_wait_runs_from_review_ready_to_resolved_or_merged() {
    let s = setup("stats-review-wait");
    let alpha = project_id(&git_repo(&s.scan, "alpha"));
    let ready = || with("run.completed", 100, json!({"ending": "review_ready"}));
    // Resolved by a person an hour after it was ready.
    add_run(
        &s,
        "rv1",
        &alpha,
        DAY,
        vec![ready(), with("review.resolved", 3_700, json!({}))],
    );
    // Merged: its lifecycle says so, as last checked (now), about two hours after it was ready.
    add_run(&s, "rv2", &alpha, 7_300, vec![ready()]);
    let store = orq_core::store::Store::open(&s.roots.database()).unwrap();
    store
        .save_lifecycle(
            "rv2",
            &orq_core::review_lifecycle::ReviewLifecycle::merged_locally(
                &"b".repeat(40),
                &"c".repeat(40),
            ),
        )
        .unwrap();
    // Still waiting: no wait to count yet.
    add_run(&s, "rv3", &alpha, DAY, vec![ready()]);
    let (_, body) = get(&s, "/api/stats?project=alpha&days=7");
    let wait = &body["review_wait"];
    assert_eq!(runs(wait), ["rv1", "rv2"], "{body}");
    assert_eq!(wait["median_secs"], 3_600, "{body}");
    let p90 = wait["p90_secs"].as_u64().unwrap();
    assert!((7_195..=7_210).contains(&p90), "{body}");
    assert_eq!(runs(&body["review_waiting"]), ["rv3"], "{body}");
}

fn usage(at: u64, input: u64, output: u64, model: &str) -> E {
    E(
        "session.usage",
        Some("implement_and_review"),
        at,
        json!({"input_tokens": input, "output_tokens": output, "cache_read_tokens": 10, "cache_write_tokens": 1, "turns": 3, "duration_secs": 60, "model": model}),
    )
}

#[test]
fn tokens_are_summed_by_project_and_model_and_runs_without_usage_are_no_data() {
    let s = setup("stats-tokens");
    let alpha = project_id(&git_repo(&s.scan, "alpha"));
    let beta = project_id(&git_repo(&s.scan, "beta"));
    add_run(
        &s,
        "t1",
        &alpha,
        DAY,
        vec![
            usage(10, 100, 20, "opus"),
            usage(20, 50, 5, "sonnet"),
            delivered(30),
        ],
    );
    add_run(
        &s,
        "t2",
        &beta,
        DAY,
        vec![usage(10, 1_000, 200, "opus"), delivered(30)],
    );
    add_run(&s, "t3", &alpha, DAY, vec![delivered(30)]);
    let (_, body) = get(&s, "/api/stats?days=7");
    let tokens = &body["tokens"];
    let row = |list: &str, key: &str, name: &str| {
        tokens[list]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row[key] == name)
            .cloned()
            .unwrap_or_else(|| panic!("no {name} in {body}"))
    };
    let alpha_row = row("by_project", "project", "alpha");
    assert_eq!(alpha_row["input_tokens"], 150, "{body}");
    assert_eq!(alpha_row["output_tokens"], 25, "{body}");
    assert_eq!(alpha_row["cache_read_tokens"], 20, "{body}");
    assert_eq!(alpha_row["cache_write_tokens"], 2, "{body}");
    assert_eq!(runs(&alpha_row), ["t1"], "{body}");
    let opus = row("by_model", "model", "opus");
    assert_eq!(opus["input_tokens"], 1_100, "{body}");
    assert_eq!(runs(&opus), ["t1", "t2"], "{body}");
    assert_eq!(
        row("by_model", "model", "sonnet")["output_tokens"],
        25 - 20,
        "{body}"
    );
    // A run with no usage recorded is no data, not zero tokens.
    assert_eq!(runs(&tokens["no_data"]), ["t3"], "{body}");
    assert!(
        !body.to_string().contains("cost"),
        "no money figure: {body}"
    );
}

fn listed(body: &Value) -> Vec<String> {
    let mut ids: Vec<String> = body["runs"]
        .as_array()
        .unwrap_or_else(|| panic!("no runs in {body}"))
        .iter()
        .map(|r| r["run_id"].as_str().unwrap().to_string())
        .collect();
    ids.sort();
    ids
}

#[test]
fn each_figure_links_to_the_runs_list_filtered_to_the_runs_it_counts() {
    let (s, _, _) = seeded("stats-links");
    let provider = StoreProjects::new(s.roots.clone(), s.scan.clone());
    let (_, body) = get_with(&provider, &s, "/api/stats?project=alpha&days=7");
    for figure in [
        &body["outcomes"]["failed"],
        &body["first_try"],
        by_stage(&body["stage_times"], "gate_fix"),
    ] {
        let link = figure["link"]
            .as_str()
            .unwrap_or_else(|| panic!("no link in {figure}"));
        let query = link
            .strip_prefix("/projects/alpha/orchestrations?")
            .unwrap_or_else(|| panic!("{link}"));
        let (status, list) = get_with(&provider, &s, &format!("/api/projects/alpha/runs?{query}"));
        assert_eq!(status, 200, "{list}");
        assert_eq!(listed(&list), runs(figure), "{link}: {list}");
    }
    // Across every project the link goes to the all-projects Runs list, which asks each project.
    let (_, all) = get_with(&provider, &s, "/api/stats?days=7");
    let link = all["outcomes"]["delivered"]["link"].as_str().unwrap();
    let query = link
        .strip_prefix("/orchestrations?")
        .unwrap_or_else(|| panic!("{link}"));
    let (_, beta) = get_with(&provider, &s, &format!("/api/projects/beta/runs?{query}"));
    assert_eq!(listed(&beta), ["b1"], "{beta}");
}

#[test]
fn one_refresh_is_reused_for_every_window_and_project_until_it_is_five_minutes_old() {
    let (s, alpha, _) = seeded("stats-cache");
    let provider = StoreProjects::new(s.roots.clone(), s.scan.clone());
    let (_, first) = get_with(&provider, &s, "/api/stats?project=alpha&days=7");
    add_run(&s, "late", &alpha, 60, vec![delivered(10)]);
    let (_, again) = get_with(&provider, &s, "/api/stats?days=30");
    assert_eq!(
        again["outcomes"]["started"]["count"], 8,
        "the cached refresh is reused: {again}"
    );
    assert_eq!(first["outcomes"]["started"]["count"], 6, "{first}");
    assert_eq!(
        orq_remote::projects::STATS_FRESH_FOR,
        std::time::Duration::from_secs(300)
    );
    // A new server reads the events again.
    let (_, fresh) = get(&s, "/api/stats?days=30");
    assert_eq!(fresh["outcomes"]["started"]["count"], 9, "{fresh}");
}

#[test]
fn the_window_leaves_out_older_runs_and_all_projects_are_counted_together() {
    let (s, _, _) = seeded("stats-window");
    let (_, week) = get(&s, "/api/stats?days=7");
    assert_eq!(
        runs(&week["outcomes"]["delivered"]),
        ["b1", "d1", "d2", "rr2"],
        "{week}"
    );
    let (_, month) = get(&s, "/api/stats?days=30");
    assert_eq!(
        runs(&month["outcomes"]["delivered"]),
        ["b1", "d1", "d2", "old", "rr2"],
        "{month}"
    );
    assert_eq!(get(&s, "/api/stats?days=12").0, 400);
    assert_eq!(get(&s, "/api/stats?project=nope&days=7").0, 404);
}

#[test]
fn a_discarded_run_is_still_counted_as_started_and_as_discarded() {
    let (s, _, _) = seeded("stats-discarded");
    let store = RunStore::new(&s.roots.state);
    orq_orchestrator::discard_run(&store, "f1", &s.scan.join("alpha")).unwrap();
    assert!(store.load("f1").is_err(), "the run's records are gone");

    let (status, stats) = get(&s, "/api/stats?project=alpha&days=7");

    assert_eq!(status, 200, "{stats}");
    assert_eq!(runs(&stats["outcomes"]["discarded"]), ["f1"], "{stats}");
    assert_eq!(stats["outcomes"]["discarded"]["count"], 1);
    assert_eq!(runs(&stats["outcomes"]["failed"]), ["f2"], "{stats}");
    assert!(
        runs(&stats["outcomes"]["started"]).contains(&"f1".to_string()),
        "{stats}"
    );
    let (_, other) = get(&s, "/api/stats?project=beta&days=7");
    assert_eq!(other["outcomes"]["discarded"]["count"], 0, "{other}");
}

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";

/// The built web app at `url`, answering its requests with `responses` in turn, signed in.
fn page(url: &str, responses: Value, phone: bool) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scenario = json!({"url": url, "responses": responses, "steps": [{"submit": TOKEN}], "phone": phone, "selectors": [".stats-grid > .card", ".stat-bar"]});
    let output = Command::new("node")
        .arg(dir.join("tests/web_react_harness.mjs"))
        .arg(dir.join("web/app.react.js"))
        .arg(scenario.to_string())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn requested(out: &Value) -> Vec<String> {
    out["requests"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["url"].as_str())
        .map(String::from)
        .collect()
}

fn web_projects() -> Value {
    let project = |id: &str| json!({"id": id, "name": id, "available": true, "features": 1, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24});
    json!({"status": 200, "json": {"projects": [project("alpha"), project("beta")]}})
}

/// What `/api/stats` answers, from the seeded runs.
fn web_stats(path: &str) -> Value {
    let (s, _, _) = seeded(&format!(
        "stats-web-{}",
        path.replace(['/', '?', '=', '&'], "-")
    ));
    let (status, body) = get(&s, path);
    assert_eq!(status, 200, "{body}");
    json!({"status": 200, "json": body})
}

fn link_to<'a>(out: &'a Value, href: &str) -> &'a Value {
    let links = out["frames"][1]["settled"]["links"].as_array().unwrap();
    links
        .iter()
        .find(|link| link["href"] == href)
        .unwrap_or_else(|| panic!("no link to {href}: {links:?}"))
}

#[test]
fn the_stats_page_shows_every_figure_as_a_link_to_the_runs_it_counts() {
    let Some(out) = page(
        "/stats",
        json!([web_projects(), web_stats("/api/stats?days=7")]),
        false,
    ) else {
        return;
    };
    assert_eq!(
        requested(&out)[..2],
        ["/api/projects", "/api/stats?days=7"],
        "{out}"
    );
    let texts: Vec<&str> = out["frames"][1]["settled"]["texts"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    for heading in [
        "Stats",
        "Runs",
        "First-try success",
        "Failures by stage",
        "Time per stage",
        "Gate-fix attempts per run",
        "Review wait",
        "Tokens by project",
        "Tokens by model",
    ] {
        assert!(texts.contains(&heading), "{heading} missing: {texts:?}");
    }
    assert_eq!(link_to(&out, "/orchestrations?runs=f1,f2")["text"], "2");
    // A chart's figure links too: the one run that failed at the gate.
    assert_eq!(link_to(&out, "/orchestrations?runs=f1")["text"], "1");
    assert!(
        !texts.iter().any(|text| text.contains('$')),
        "no money figure: {texts:?}"
    );
}

#[test]
fn the_stats_page_reads_the_window_and_the_project_from_its_address() {
    let Some(out) = page(
        "/projects/alpha/stats?days=30",
        json!([
            web_projects(),
            web_stats("/api/stats?days=30&project=alpha")
        ]),
        false,
    ) else {
        return;
    };
    assert!(
        requested(&out).contains(&"/api/stats?days=30&project=alpha".to_string()),
        "{out}"
    );
    link_to(&out, "/projects/alpha/stats?days=7");
    link_to(&out, "/projects/alpha/stats?days=90");
    assert_eq!(
        out["frames"][1]["settled"]["path"],
        "/projects/alpha/stats?days=30"
    );
}

#[test]
fn the_stats_page_opens_from_a_reload_or_a_shared_address() {
    let served = |path: &str| {
        orq_remote::page_asset("127.0.0.1".parse().unwrap(), "GET", path).map(|asset| asset.body)
    };
    let page = served("/").unwrap();
    for path in ["/stats", "/projects/alpha/stats"] {
        assert_eq!(served(path), Some(page), "{path}");
    }
    for path in [
        "/stats/",
        "/stats/x",
        "/projects/alpha/stats/x",
        "/projects/../stats",
    ] {
        assert_eq!(served(path), None, "{path}");
    }
}

#[test]
fn on_a_phone_the_stats_cards_stack_and_the_charts_are_bars_with_their_figures() {
    let Some(out) = page(
        "/stats",
        json!([web_projects(), web_stats("/api/stats?days=7")]),
        true,
    ) else {
        return;
    };
    let settled = &out["frames"][1]["settled"];
    assert!(
        settled["matches"][".stats-grid > .card"].as_u64().unwrap() >= 6,
        "{settled}"
    );
    assert!(
        settled["matches"][".stat-bar"].as_u64().unwrap() >= 3,
        "{settled}"
    );
    let css =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("web/app.css")).unwrap();
    let phone = css
        .split("@media (max-width: 700px)")
        .skip(1)
        .collect::<String>();
    assert!(
        phone.contains(".stats-grid"),
        "the phone layout stacks the stats cards"
    );
}

#[test]
fn a_runs_list_opened_from_a_figure_lists_only_the_runs_it_counts() {
    let runs = json!({"status": 200, "json": {"runs": [{"run_id": "f1", "feature": "f", "state": "failed", "stage": null, "started_at": null, "updated_at": null, "target_branch": null}]}});
    let Some(one) = page(
        "/projects/alpha/orchestrations?runs=f1,f2",
        json!([web_projects(), runs]),
        false,
    ) else {
        return;
    };
    assert!(
        requested(&one).contains(&"/api/projects/alpha/runs?runs=f1,f2".to_string()),
        "{one}"
    );
    let Some(all) = page(
        "/orchestrations?runs=f1",
        json!([web_projects(), runs, runs]),
        false,
    ) else {
        return;
    };
    let urls = requested(&all);
    assert!(
        urls.contains(&"/api/projects/alpha/runs?runs=f1".to_string())
            && urls.contains(&"/api/projects/beta/runs?runs=f1".to_string()),
        "{all}"
    );
}

/// A fake agent CLI's closing JSON result, as `claude -p --output-format json` prints it.
const JSON_RESULT: &str = r#"{"type":"result","subtype":"success","num_turns":7,"duration_ms":65000,"usage":{"input_tokens":120,"output_tokens":3400,"cache_read_input_tokens":50000,"cache_creation_input_tokens":2000},"modelUsage":{"claude-opus-5-5":{"inputTokens":120,"outputTokens":3400}}}"#;

/// Runs `script` under `sh` as an unattended implement session of `run`, and returns its events.
fn run_session(s: &Setup, run: &str, script: &str, terminal: bool) -> Vec<Event> {
    let store = RunStore::new(&s.roots.state);
    std::fs::create_dir_all(&s.scan).unwrap();
    let args = vec!["-c".to_string(), script.to_string()];
    let log = s.roots.state.join(format!("{run}.log"));
    let session = orq_orchestrator::ImplementSession {
        program: "sh",
        args: &args,
        env: &[],
        cwd: &s.scan,
        session_id: None,
        log: &log,
        terminal,
        exit_marker: None,
    };
    assert!(
        orq_orchestrator::run_implement_stage(&store, run, &session)
            .unwrap()
            .success
    );
    store.events(run).unwrap()
}

fn session_usage(events: &[Event]) -> Vec<&Value> {
    events
        .iter()
        .filter(|event| event.kind == "session.usage")
        .map(|event| &event.payload)
        .collect()
}

#[test]
fn an_unattended_session_records_its_usage_from_the_agent_clis_json_result() {
    let s = setup("stats-usage-result");
    add_run(&s, "u1", "p", 0, vec![]);

    let events = run_session(
        &s,
        "u1",
        &format!("echo working; echo '{JSON_RESULT}'"),
        false,
    );

    let usage = session_usage(&events);
    assert_eq!(usage.len(), 1, "{events:?}");
    let usage = usage[0];
    assert_eq!(usage["input_tokens"], 120);
    assert_eq!(usage["output_tokens"], 3400);
    assert_eq!(usage["cache_read_tokens"], 50000);
    assert_eq!(usage["cache_write_tokens"], 2000);
    assert_eq!(usage["turns"], 7);
    assert_eq!(usage["model"], "claude-opus-5-5");
    assert!(usage["duration_secs"].is_u64(), "{usage}");
    assert!(
        usage.get("cost").is_none() && usage.get("total_cost_usd").is_none(),
        "{usage}"
    );
}

/// One assistant line of a Claude transcript, `secs_ago` seconds old.
fn assistant(
    id: &str,
    model: &str,
    secs_ago: u64,
    input: u64,
    output: u64,
    read: u64,
    write: u64,
) -> String {
    let at = ago_now(secs_ago).replace('Z', ".250Z");
    json!({"type": "assistant", "timestamp": at, "message": {"id": id, "model": model, "role": "assistant", "usage": {"input_tokens": input, "output_tokens": output, "cache_read_input_tokens": read, "cache_creation_input_tokens": write}}}).to_string()
}

#[test]
fn an_attended_session_sums_the_usage_of_its_transcript_from_when_it_started() {
    let s = setup("stats-usage-transcript");
    add_run(&s, "t1", "p", 0, vec![]);
    let transcript = s.roots.state.join("transcript.jsonl");
    let hook =
        json!({"hook_event_name": "Stop", "session_id": "abc", "transcript_path": transcript})
            .to_string();
    let store = RunStore::new(&s.roots.state);
    // While the session runs it writes its transcript, and its hooks say where it is.
    let told = std::thread::spawn(move || {
        while !store
            .events("t1")
            .unwrap()
            .iter()
            .any(|event| event.kind == "stage.started")
        {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        // Into the next second, so this session's lines are clearly after it started.
        std::thread::sleep(std::time::Duration::from_millis(1_100));
        let lines = [
            // From an earlier session the transcript was resumed from: not this one's.
            assistant("m0", "claude-opus-5-5", 3_600, 9_000, 9_000, 9_000, 9_000),
            json!({"type": "user", "timestamp": ago_now(0), "message": {"role": "user", "content": "go"}}).to_string(),
            assistant("m1", "claude-opus-5-5", 0, 10, 200, 1_000, 50),
            // The same message again, one line per content block: counted once.
            assistant("m1", "claude-opus-5-5", 0, 10, 200, 1_000, 50),
            assistant("m2", "claude-sonnet-5-5", 0, 5, 30, 400, 0),
            assistant("m3", "claude-opus-5-5", 0, 1, 70, 600, 10),
            "not json".to_string(),
        ];
        std::fs::write(&transcript, lines.join("\n") + "\n").unwrap();
        orq_orchestrator::remember_transcript(&store, "t1", &hook).unwrap();
    });

    let events = run_session(&s, "t1", "sleep 3", true);
    told.join().unwrap();

    let usage = session_usage(&events);
    assert_eq!(usage.len(), 1, "{events:?}");
    let usage = usage[0];
    assert_eq!(usage["input_tokens"], 16);
    assert_eq!(usage["output_tokens"], 300);
    assert_eq!(usage["cache_read_tokens"], 2_000);
    assert_eq!(usage["cache_write_tokens"], 60);
    assert_eq!(usage["turns"], 3);
    assert_eq!(usage["model"], "claude-opus-5-5");
    assert!(
        usage["duration_secs"]
            .as_u64()
            .is_some_and(|secs| secs >= 2),
        "{usage}"
    );
}
