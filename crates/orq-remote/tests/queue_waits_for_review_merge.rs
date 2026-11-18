//! A queued run that waits for a person's review holds its project's queue: a run delivered for review
//! whose MR is open, or a run that ended needing review, keeps every later entry from starting until the
//! MR merges (or the run is accepted), and a closed MR counts as a failed run. The project setting "Wait
//! for each run's review (and MR) before the next", on by default, turns this off. Each round is driven
//! here with a fake launcher, and the MR's state is what the review refresh saved for the run.

mod web_deps;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use orq_core::project_settings::{Delivery, ProjectSettings};
use orq_core::remote_inbox::{handle_with_body, RemoteConfig, Request};
use orq_core::review_lifecycle::{ProviderState, ReviewLifecycle};
use orq_core::storage::StorageRoots;
use orq_core::store::Store;
use orq_orchestrator::{Event, RunRecord, RunStore};
use orq_remote::projects::{LaunchError, QueueRound, RunLauncher, StoreProjects};
use serde_json::{json, Value};

/// Starts a run the way `orq run` records one.
struct Launcher {
    state: PathBuf,
    started: Mutex<Vec<String>>,
}

impl RunLauncher for Launcher {
    fn start(&self, repo: &Path, feature: &str) -> Result<String, LaunchError> {
        let mut started = self.started.lock().unwrap();
        started.push(feature.to_string());
        let run = format!("run-{}-{feature}", started.len());
        seed_run(&self.state, repo, &run, feature);
        Ok(run)
    }
}

struct Setup {
    scan: PathBuf,
    roots: StorageRoots,
    launcher: Arc<Launcher>,
    repo: PathBuf,
}

/// A project `alpha` with two independent approved features, COM-01 and COM-02, queued in that order,
/// delivered by `delivery`, at most one run active, and `settings` changed further by `change`.
fn setup(label: &str, delivery: Delivery, change: impl FnOnce(&mut ProjectSettings)) -> Setup {
    let base = web_deps::private_temp_dir(label);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    std::fs::write(roots.config_file(), "remote_actions = true\n").unwrap();
    let launcher = Arc::new(Launcher { state: roots.state.clone(), started: Mutex::default() });
    let scan = base.join("code");
    let path = scan.join("alpha");
    std::fs::create_dir_all(&path).unwrap();
    for args in [&["init", "-q", "-b", "main"][..], &["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "commit", "-q", "--allow-empty", "-m", "start"]] {
        let out = Command::new("git").args(args).current_dir(&path).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }
    let repo = std::fs::canonicalize(path).unwrap();
    let ledger = json!([{"featureName": "COM-01", "dependsOn": [], "status": "pending"}, {"featureName": "COM-02", "dependsOn": [], "status": "pending"}]);
    std::fs::write(repo.join(".tdd-features.json"), ledger.to_string()).unwrap();
    let mut settings = ProjectSettings { delivery, max_active_runs: Some(1), ..ProjectSettings::default() };
    change(&mut settings);
    let mut store = Store::open(&roots.database()).unwrap();
    store.save_project_settings(&repo, 0, &settings).unwrap();
    let s = Setup { scan, roots, launcher, repo };
    let (status, body) = send(&s, "POST", "/api/projects/alpha/queue", json!({"features": ["COM-01", "COM-02"]}).to_string().as_bytes());
    assert_eq!(status, 200, "{body}");
    s
}

fn provider(s: &Setup) -> StoreProjects {
    StoreProjects::new(s.roots.clone(), s.scan.clone()).with_launcher(s.launcher.clone())
}

fn send(s: &Setup, method: &str, path: &str, body: &[u8]) -> (u16, Value) {
    let config = RemoteConfig { token: "tok".into() };
    let request = Request { method, path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let r = handle_with_body(&s.roots, &config, "server-project", "server", &request, body, &provider(s));
    (r.status, r.body)
}

/// The queue as the page reads it.
fn queue(s: &Setup) -> Value {
    let (status, body) = send(s, "GET", "/api/projects/alpha/queue", b"");
    assert_eq!(status, 200, "{body}");
    body["queue"].clone()
}

/// Each entry's feature and state, in queue order.
fn states(s: &Setup) -> Vec<(String, String)> {
    queue(s)["entries"].as_array().unwrap().iter().map(|e| (e["feature"].as_str().unwrap().to_string(), e["state"].as_str().unwrap().to_string())).collect()
}

fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
}

/// One round of the queue at noon, with no restart pending.
fn round(s: &Setup) {
    provider(s).queue_round(&QueueRound { minute_of_day: 12 * 60, restart_pending: false });
}

fn started(s: &Setup) -> Vec<String> {
    s.launcher.started.lock().unwrap().clone()
}

fn event(store: &RunStore, run: &str, kind: &str, payload: Value) {
    let record = store.load(run).unwrap();
    let n = store.events(run).unwrap().len();
    store
        .append_event(&Event {
            schema_version: 1,
            event_id: format!("{run}-{n}"),
            run_id: run.into(),
            project_id: record.project_id.clone(),
            feature_id: record.feature_id.clone(),
            kind: kind.into(),
            stage: Some("implement".into()),
            attempt: 1,
            occurred_at: "2026-11-18T10:00:00Z".into(),
            payload,
        })
        .unwrap();
}

/// A run of `feature` in the project at `repo`, started and still running.
fn seed_run(state: &Path, repo: &Path, run: &str, feature: &str) {
    let store = RunStore::new(state);
    let record = RunRecord {
        delivery: Default::default(),
        run_id: run.into(),
        project_id: orq_git::project_id(&orq_git::resolve_project(repo).unwrap()),
        feature_id: feature.into(),
        remote: "origin".into(),
        provider: "gitlab".into(),
        target_branch: "main".into(),
        base_sha: git(repo, &["rev-parse", "HEAD"]),
        branch: format!("orq/{run}"),
        worktree: repo.join(".claude/worktrees").join(run),
        repository: String::new(),
        profiles: Default::default(),
    };
    store.create(&record).unwrap();
    event(&store, run, "stage.started", json!({}));
}

/// Runs git in `repo` and gives back what it printed, trimmed.
fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git").args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"]).args(args).current_dir(repo).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Ends `run` with `ending` (`review_ready`, `needs_review`, `merged`, ...).
fn end_run(s: &Setup, run: &str, ending: &str) {
    event(&RunStore::new(&s.roots.state), run, "run.completed", json!({"ending": ending}));
}

const MR: &str = "https://gitlab.example/alpha/-/merge_requests/12";

/// What the review refresh saved about `run`'s MR: `state` at the provider.
fn mr(s: &Setup, run: &str, state: ProviderState) {
    let mut lifecycle = ReviewLifecycle::associate("gitlab", "12", MR, &"b".repeat(40));
    lifecycle.state = state;
    lifecycle.observed = true;
    Store::open(&s.roots.database()).unwrap().save_lifecycle(run, &lifecycle).unwrap();
}

#[test]
fn a_queued_run_whose_mr_is_open_holds_the_queue_until_the_mr_merges() {
    let s = setup("wait-mr-merge", Delivery::Review, |_| {});
    round(&s);
    assert_eq!(started(&s), ["COM-01"]);
    end_run(&s, "run-1-COM-01", "review_ready");
    mr(&s, "run-1-COM-01", ProviderState::Open);

    round(&s);
    round(&s);
    assert_eq!(started(&s), ["COM-01"], "COM-02 waits while COM-01's MR is open");
    assert_eq!(states(&s), pairs(&[("COM-01", "review"), ("COM-02", "next")]));

    mr(&s, "run-1-COM-01", ProviderState::Merged);
    round(&s);
    assert_eq!(started(&s), ["COM-01", "COM-02"], "COM-02 starts once COM-01's MR merged");
    assert_eq!(states(&s), pairs(&[("COM-02", "running")]), "the merged run's entry leaves the queue");
}

#[test]
fn a_queued_run_whose_mr_was_closed_pauses_the_queue_or_counts_as_failed() {
    let s = setup("wait-mr-closed-stop", Delivery::Review, |settings| settings.queue_stop_on_failure = true);
    round(&s);
    end_run(&s, "run-1-COM-01", "review_ready");
    mr(&s, "run-1-COM-01", ProviderState::Closed);
    round(&s);
    assert_eq!(started(&s), ["COM-01"], "a closed MR pauses the queue when a failed run does");
    assert_eq!(queue(&s)["paused"], "COM-01's MR was closed without merging");
    assert_eq!(states(&s), pairs(&[("COM-01", "failed"), ("COM-02", "next")]));

    let s = setup("wait-mr-closed-go", Delivery::Review, |_| {});
    round(&s);
    end_run(&s, "run-1-COM-01", "review_ready");
    mr(&s, "run-1-COM-01", ProviderState::Closed);
    round(&s);
    assert_eq!(started(&s), ["COM-01", "COM-02"], "otherwise the queue goes on");
    assert_eq!(queue(&s)["paused"], Value::Null);
    assert_eq!(states(&s), pairs(&[("COM-01", "failed"), ("COM-02", "running")]));
}

#[test]
fn a_queued_run_that_needs_a_persons_review_holds_the_queue_until_it_is_accepted_for_any_delivery() {
    let s = setup("wait-needs-review", Delivery::MergeLocal, |_| {});
    round(&s);
    end_run(&s, "run-1-COM-01", "needs_review");
    round(&s);
    assert_eq!(started(&s), ["COM-01"], "COM-02 waits while COM-01 needs review");
    assert_eq!(states(&s), pairs(&[("COM-01", "review"), ("COM-02", "next")]));

    // Accepted: the run is review ready, merged here with no MR.
    end_run(&s, "run-1-COM-01", "review_ready");
    round(&s);
    assert_eq!(started(&s), ["COM-01", "COM-02"]);
    assert_eq!(states(&s), pairs(&[("COM-02", "running")]));
}

#[test]
fn a_queued_run_that_needs_review_and_was_merged_by_hand_lets_the_queue_go_on() {
    let s = setup("wait-needs-review-merged", Delivery::Review, |_| {});
    round(&s);
    end_run(&s, "run-1-COM-01", "needs_review");
    round(&s);
    assert_eq!(started(&s), ["COM-01"]);

    // A person merged the run's work into main by hand.
    git(&s.repo, &["commit", "-q", "--allow-empty", "-m", "COM-01 work"]);
    git(&s.repo, &["branch", "orq/run-1-COM-01"]);
    round(&s);
    assert_eq!(started(&s), ["COM-01", "COM-02"], "the merged run no longer holds the queue");
    assert_eq!(states(&s), pairs(&[("COM-02", "running")]));
}

#[test]
fn the_queue_says_which_review_it_waits_for_with_the_mr_link() {
    let s = setup("wait-mr-json", Delivery::Review, |_| {});
    round(&s);
    end_run(&s, "run-1-COM-01", "review_ready");
    mr(&s, "run-1-COM-01", ProviderState::Open);
    let shown = queue(&s);
    assert_eq!(shown["entries"][0]["mr_url"], MR);
    assert_eq!(shown["waiting_for_review"], json!({"feature": "COM-01", "mr_url": MR}));

    end_run(&s, "run-1-COM-01", "needs_review");
    let shown = queue(&s);
    assert_eq!(shown["entries"][0]["mr_url"], Value::Null);
    assert_eq!(shown["waiting_for_review"], json!({"feature": "COM-01", "mr_url": null}));

    mr(&s, "run-1-COM-01", ProviderState::Merged);
    end_run(&s, "run-1-COM-01", "review_ready");
    assert_eq!(queue(&s)["waiting_for_review"], Value::Null);
}

#[test]
fn with_the_setting_off_the_next_feature_starts_while_the_mr_is_open() {
    let s = setup("wait-off", Delivery::Review, |settings| settings.queue_wait_for_review = false);
    round(&s);
    end_run(&s, "run-1-COM-01", "review_ready");
    mr(&s, "run-1-COM-01", ProviderState::Open);
    round(&s);
    assert_eq!(started(&s), ["COM-01", "COM-02"], "only the run limit counts");
    assert_eq!(queue(&s)["waiting_for_review"], Value::Null);
}

#[test]
fn the_wait_for_review_setting_is_read_and_saved_through_the_settings_api() {
    let s = setup("wait-setting-api", Delivery::Review, |_| {});
    let settings = |s: &Setup| send(s, "GET", "/api/projects/alpha/settings", b"").1["settings"].clone();
    assert_eq!(settings(&s)["queue_wait_for_review"], true, "on by default");
    let save = |s: &Setup, extra: Value| {
        let mut body = json!({"implementer": null, "reviewer": null, "delivery": "review", "max_active_runs": 1, "min_free_disk_gb": null, "revision": settings(s)["revision"]});
        for (key, value) in extra.as_object().unwrap() {
            body[key] = value.clone();
        }
        send(s, "POST", "/api/projects/alpha/settings", body.to_string().as_bytes())
    };
    assert_eq!(save(&s, json!({"queue_wait_for_review": false})).0, 200);
    assert_eq!(settings(&s)["queue_wait_for_review"], false);
    assert_eq!(save(&s, json!({})).0, 200, "a save that leaves it out");
    assert_eq!(settings(&s)["queue_wait_for_review"], true, "leaving it out waits, as the default does");
    assert_eq!(save(&s, json!({"queue_wait_for_review": "no"})).0, 400);
    assert_eq!(settings(&s)["queue_wait_for_review"], true, "a refused save writes nothing");
}

#[test]
fn the_dashboard_card_says_when_the_queue_waits_for_review() {
    let s = setup("wait-card", Delivery::Review, |_| {});
    let card = |s: &Setup| {
        let (status, body) = send(s, "GET", "/api/projects", b"");
        assert_eq!(status, 200, "{body}");
        body["projects"].as_array().unwrap().iter().find(|p| p["id"] == "alpha").unwrap()["queue"].clone()
    };
    round(&s);
    assert_eq!(card(&s)["waiting_for_review"], Value::Null, "a run still going");
    end_run(&s, "run-1-COM-01", "review_ready");
    mr(&s, "run-1-COM-01", ProviderState::Open);
    assert_eq!(card(&s)["waiting_for_review"], "COM-01");
    mr(&s, "run-1-COM-01", ProviderState::Merged);
    assert_eq!(card(&s)["waiting_for_review"], Value::Null);
}


/// The queue's wait for review on the page.
mod page {
    //! On the page, a queue that waits for a run's review says so: the entry shows "Waiting for review"
    //! with its MR linked, the queue's head says "Waiting for <feature>'s MR to merge" (or its review, with
    //! no MR), on the project's queue page and the all-projects Queue page alike; the dashboard card's
    //! queue line says it waits; and the project's settings show and save whether the queue waits.

    use super::web_deps;
    use serde_json::{json, Value};
    use std::process::Command;

    const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
    const MR: &str = "https://gitlab.example/alpha/-/merge_requests/12";
    const SELECTORS: [&str; 7] = [
        "[data-queue-entry]",
        "[data-queue-state=\"review\"]",
        "[data-queue-mr]",
        "[data-queue-waiting-review]",
        "[data-card-queue]",
        "[data-queue-wait-for-review]",
        "[data-project-queue-wait]",
    ];

    fn run(url: &str, responses: Vec<Value>, steps: Vec<Value>) -> Option<Value> {
        if !web_deps::ready() {
            return None;
        }
        let mut all = vec![json!({"submit": TOKEN})];
        all.extend(steps);
        let dir = env!("CARGO_MANIFEST_DIR");
        let scenario = json!({"url": url, "responses": responses, "steps": all, "selectors": SELECTORS});
        let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        Some(serde_json::from_slice(&output.stdout).unwrap())
    }

    fn entry(feature: &str, state: &str, run: Value, mr_url: Value) -> Value {
        json!({"feature": feature, "state": state, "run": run, "reason": null, "added_at": 1, "added_by": "web", "implementer": null, "reviewer": null, "mr_url": mr_url})
    }

    /// COM-01's run waits for review (its MR is `mr_url`, if any) and COM-02 is next.
    fn waiting(mr_url: Value) -> Value {
        let entries = vec![entry("COM-01", "review", json!("run-1-COM-01"), mr_url.clone()), entry("COM-02", "next", Value::Null, Value::Null)];
        json!({"entries": entries, "count": 2, "paused": null, "waiting_for_review": {"feature": "COM-01", "mr_url": mr_url}})
    }

    fn reply(queue: Value) -> Value {
        json!({"status": 200, "json": {"queue": queue, "remote_actions": true}})
    }

    fn project(id: &str, queue: Value) -> Value {
        json!({"id": id, "name": id, "available": true, "features": 4, "active_runs": 0, "max_active_runs": null, "start_blocked": null, "reviews_waiting": 1, "failed_runs": 0, "failed_window_hours": 24, "queue": queue})
    }

    fn projects(list: Vec<Value>) -> Value {
        json!({"status": 200, "json": {"projects": list}})
    }

    fn last(out: &Value) -> &Value {
        out["frames"].as_array().unwrap().last().unwrap()
    }

    fn count(frame: &Value, selector: &str) -> u64 {
        frame["settled"]["matches"][selector].as_u64().unwrap()
    }

    fn texts(frame: &Value) -> Vec<String> {
        frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
    }

    fn shows(frame: &Value, wanted: &str) -> bool {
        texts(frame).iter().any(|t| t.contains(wanted))
    }

    fn links(frame: &Value) -> Vec<(String, String)> {
        frame["settled"]["links"].as_array().unwrap().iter().map(|l| (l["href"].as_str().unwrap_or("").to_string(), l["text"].as_str().unwrap_or("").to_string())).collect()
    }

    #[test]
    fn the_queue_page_shows_the_entry_waiting_for_review_with_its_mr_link() {
        let Some(out) = run("/projects/alpha/queue", vec![reply(waiting(json!(MR))), projects(vec![project("alpha", Value::Null)])], vec![]) else { return };
        let frame = last(&out);
        assert_eq!(count(frame, "[data-queue-state=\"review\"]"), 1, "{out}");
        assert!(shows(frame, "Waiting for review"), "{:?}", texts(frame));
        assert_eq!(count(frame, "[data-queue-mr]"), 1, "{out}");
        assert!(links(frame).iter().any(|(href, _)| href == MR), "the MR is linked: {:?}", links(frame));
        assert_eq!(count(frame, "[data-queue-waiting-review]"), 1, "{out}");
        assert!(shows(frame, "Waiting for COM-01's MR to merge"), "{:?}", texts(frame));

        let Some(out) = run("/projects/alpha/queue", vec![reply(waiting(Value::Null)), projects(vec![project("alpha", Value::Null)])], vec![]) else { return };
        let frame = last(&out);
        assert_eq!((count(frame, "[data-queue-state=\"review\"]"), count(frame, "[data-queue-mr]")), (1, 0), "no MR, nothing to link: {out}");
        assert!(shows(frame, "Waiting for COM-01's review"), "{:?}", texts(frame));
    }

    #[test]
    fn the_all_projects_queue_page_says_a_project_s_queue_waits_for_review() {
        let empty = json!({"status": 200, "json": {"queue": {"entries": [], "count": 0, "paused": null, "waiting_for_review": null}, "remote_actions": true}});
        let listing = projects(vec![project("alpha", Value::Null), project("beta", Value::Null)]);
        let Some(out) = run("/queue", vec![listing, reply(waiting(json!(MR))), empty], vec![]) else { return };
        let frame = last(&out);
        assert_eq!(count(frame, "[data-queue-waiting-review]"), 1, "{out}");
        assert!(shows(frame, "Waiting for COM-01's MR to merge"), "{:?}", texts(frame));
        assert_eq!(count(frame, "[data-queue-state=\"review\"]"), 1, "{out}");
        assert!(links(frame).iter().any(|(href, _)| href == MR), "{:?}", links(frame));
    }

    #[test]
    fn the_dashboard_card_s_queue_line_says_it_waits_for_review() {
        let listing = projects(vec![project("alpha", json!({"count": 2, "paused": false, "waiting_for_review": "COM-01"})), project("beta", json!({"count": 1, "paused": false, "waiting_for_review": null}))]);
        let Some(out) = run("/dashboard", vec![listing], vec![]) else { return };
        let frame = last(&out);
        assert_eq!(count(frame, "[data-card-queue]"), 2, "{out}");
        assert!(texts(frame).iter().any(|t| t == "Queue: 2 (waiting for COM-01's review)"), "{:?}", texts(frame));
        assert!(texts(frame).iter().any(|t| t == "Queue: 1"), "{:?}", texts(frame));
    }

    fn project_settings(wait: Value) -> Value {
        let mut settings = json!({
            "implementer": {"project": null, "fallback": null},
            "reviewer": {"project": null, "fallback": null},
            "delivery": "review",
            "max_active_runs": 1,
            "min_free_disk_gb": null,
            "queue_hours": null,
            "queue_stop_on_failure": false,
            "revision": 3
        });
        if !wait.is_null() {
            settings["queue_wait_for_review"] = wait;
        }
        json!({"status": 200, "json": {"settings": settings}})
    }

    #[test]
    fn the_settings_queue_card_shows_and_saves_whether_the_queue_waits_for_review() {
        let Some(out) = run("/projects/alpha/settings", vec![project_settings(json!(true)), projects(vec![project("alpha", Value::Null)])], vec![]) else { return };
        let frame = last(&out);
        assert_eq!(count(frame, "[data-project-queue-wait]"), 1, "{out}");
        assert!(shows(frame, "Wait for each run's review (and MR) before the next"), "{:?}", texts(frame));
        assert!(shows(frame, "Waits"), "{:?}", texts(frame));

        let machine = json!({"status": 200, "json": {"settings": {"implementer": null, "reviewer": null, "models": [], "remote_actions": true, "version": "v-1"}}});
        let options = json!({"status": 200, "json": {"options": [{"harness": "claude", "models": ["opus"]}]}});
        let ok = json!({"status": 200, "json": {"settings": {}}});
        let steps = vec![
            json!({"click": "[data-project-settings-edit]"}),
            json!({"click": "[data-queue-wait-for-review]"}),
            json!({"click": "[data-project-settings-save]"}),
            json!({"click": "[data-confirm]"}),
        ];
        // An older server sends no such setting: the page reads it as on, as the server's default is.
        let responses = vec![project_settings(Value::Null), projects(vec![project("alpha", Value::Null)]), machine, options, ok, project_settings(json!(false))];
        let Some(out) = run("/projects/alpha/settings", responses, steps) else { return };
        let sent: Vec<Value> = out["requests"].as_array().unwrap().iter().filter(|r| r["method"] == "POST").map(|r| serde_json::from_str(r["body"].as_str().unwrap()).unwrap()).collect();
        assert_eq!(sent.len(), 1, "{out}");
        assert_eq!(sent[0]["queue_wait_for_review"], false, "unticking it saves it off: {}", sent[0]);
        assert!(shows(last(&out), "Starts the next as the run limit allows"), "{:?}", texts(last(&out)));
    }
}
