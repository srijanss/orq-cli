//! A design group run on the run page: the group's members in order with their state and the components each
//! covers, each member's design rounds grouped under it (the newest member open), Add design issues naming the
//! member that stopped, and the runs list saying which member of how many the run is at.

mod web_deps;

use std::path::{Path, PathBuf};
use std::process::Command;

use orq_core::remote_inbox::{RemoteConfig, Request};
use orq_core::storage::StorageRoots;
use orq_orchestrator::{Event, RunRecord, RunStore};
use orq_remote::projects::StoreProjects;
use serde_json::{json, Value};

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";

// The page in jsdom, fed what a fake server answers.

fn harness(scenario: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let output = Command::new("node")
        .arg(format!("{dir}/tests/web_react_harness.mjs"))
        .arg(format!("{dir}/web/app.react.js"))
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

fn projects() -> Value {
    json!({"status": 200, "json": {"projects": [{"id": "alpha", "name": "alpha", "available": true, "features": 3, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24}]}})
}

/// One design round of a member, with one issue in its Hero section.
fn round(n: u64, verdict: &str, issue: &str) -> Value {
    json!({"round": n, "verdict": verdict, "decided_by": "design reviewer", "reason": null, "viewports": [], "issues": [{"id": issue, "section": "Hero", "viewport": "all", "severity": "major", "status": "open", "expected": "a", "actual": "b", "fix_direction": "c", "from_person": false}], "suggestions": [], "not_assessed": [], "blockers": []})
}

/// A design group run of three members that stopped needing review in its second member's first design round:
/// the first passed after two rounds, the third waits.
fn group_run() -> Value {
    json!({
        "run_id": "r-1", "feature": "shop-header", "target_branch": "main", "state": "needs_review", "stage": "design_review",
        "started_at": "2026-11-01T10:00:00Z", "updated_at": "2026-11-01T11:00:00Z", "delivery": "review",
        "design_group": {
            "at": 2, "of": 3, "stopped": "shop-cards",
            "members": [
                {"feature": "shop-header", "state": "passed", "round": 2, "components": ["header", "nav"], "design": {"of": 3, "rounds": [round(1, "changes_required", "header-1"), round(2, "pass", "header-2")]}},
                {"feature": "shop-cards", "state": "stopped", "round": 1, "components": ["cards"], "design": {"of": 3, "rounds": [round(1, "changes_required", "cards-1")]}},
                {"feature": "shop-footer", "state": "waiting", "round": null, "components": ["footer"], "design": null}
            ]
        }
    })
}

const PAGE_SELECTORS: [&str; 14] = [
    "[data-design-group]",
    "[data-group-member]",
    "[data-group-member='shop-header'][data-member-state='passed']",
    "[data-group-member='shop-cards'][data-member-state='stopped']",
    "[data-group-member='shop-footer'][data-member-state='waiting']",
    "[data-member-design]",
    "[data-member-design][open]",
    "[data-member-design='shop-cards'][open]",
    "[data-member-design='shop-header'] [data-design-pick]",
    "[data-member-design='shop-cards'] [data-design-issue='cards-1']",
    "[data-member-design='shop-header'] [data-design-issue='cards-1']",
    "[data-design-issues]",
    "[data-design-issues-member]",
    "[data-design-review]",
];

/// Opens the run page of `run` with actions on; the live follow gets a held stream, then `extra` answer what
/// `steps` ask.
fn open_run(run: Value, extra: Vec<Value>, steps: Vec<Value>) -> Option<Value> {
    let id = run["run_id"].as_str().unwrap().to_string();
    let body = json!({"run": run, "events": [], "next_cursor": null, "review_rounds": 0, "remote_actions": true});
    let mut responses = vec![
        json!({"status": 200, "json": body}),
        projects(),
        json!({"status": 200, "stream": [], "end": "hold"}),
    ];
    responses.extend(extra);
    let mut all = vec![json!({"submit": TOKEN})];
    all.extend(steps);
    harness(
        json!({"url": format!("/projects/alpha/orchestrations/{id}"), "responses": responses, "steps": all, "selectors": PAGE_SELECTORS}),
    )
}

fn last(out: &Value) -> &Value {
    out["frames"].as_array().unwrap().last().unwrap()
}

fn count(frame: &Value, selector: &str) -> u64 {
    frame["settled"]["matches"][selector]
        .as_u64()
        .unwrap_or_else(|| panic!("{selector} was not measured"))
}

fn texts(frame: &Value) -> Vec<String> {
    frame["settled"]["texts"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t.as_str())
        .map(String::from)
        .collect()
}

#[test]
fn the_run_page_lists_the_groups_members_in_order_with_their_state_and_components() {
    let Some(out) = open_run(group_run(), vec![], vec![]) else {
        return;
    };

    let end = last(&out);
    assert_eq!(count(end, "[data-design-group]"), 1, "{out}");
    assert_eq!(count(end, "[data-group-member]"), 3, "{out}");
    for selector in [
        "[data-group-member='shop-header'][data-member-state='passed']",
        "[data-group-member='shop-cards'][data-member-state='stopped']",
        "[data-group-member='shop-footer'][data-member-state='waiting']",
    ] {
        assert_eq!(count(end, selector), 1, "{selector}: {out}");
    }
    let texts = texts(end);
    let at = |needle: &str| {
        texts
            .iter()
            .position(|t| t == needle)
            .unwrap_or_else(|| panic!("{needle} missing: {texts:?}"))
    };
    assert!(
        at("shop-header") < at("shop-cards") && at("shop-cards") < at("shop-footer"),
        "in the group's order: {texts:?}"
    );
    for shown in [
        "Passed",
        "Stopped in design round 1",
        "Waiting",
        "header, nav",
        "cards",
        "footer",
        "Member 2 of 3",
    ] {
        at(shown);
    }
}

#[test]
fn each_members_design_rounds_are_grouped_under_it_with_the_newest_member_open() {
    let Some(out) = open_run(group_run(), vec![], vec![]) else {
        return;
    };

    let end = last(&out);
    assert_eq!(
        count(end, "[data-member-design]"),
        2,
        "the waiting member has no rounds: {out}"
    );
    assert_eq!(count(end, "[data-member-design][open]"), 1, "{out}");
    assert_eq!(
        count(end, "[data-member-design='shop-cards'][open]"),
        1,
        "the newest member is open: {out}"
    );
    assert_eq!(
        count(end, "[data-member-design='shop-header'] [data-design-pick]"),
        0,
        "a folded member's rounds wait until it is opened: {out}"
    );
    let opened = open_run(
        group_run(),
        vec![],
        vec![json!({"click": "[data-member-design='shop-header'] summary"})],
    )
    .unwrap();
    assert_eq!(
        count(
            last(&opened),
            "[data-member-design='shop-header'] [data-design-pick]"
        ),
        2,
        "its own two rounds: {opened}"
    );
    assert_eq!(
        count(
            end,
            "[data-member-design='shop-cards'] [data-design-issue='cards-1']"
        ),
        1,
        "{out}"
    );
    assert_eq!(
        count(
            end,
            "[data-member-design='shop-header'] [data-design-issue='cards-1']"
        ),
        0,
        "another member's issue: {out}"
    );
}

#[test]
fn a_member_that_stopped_before_its_design_loop_says_where_it_stopped_not_its_last_design_round() {
    for (step, said) in [
        ("gate", "Stopped at the gate"),
        ("implement", "Stopped while implementing"),
        ("design", "Stopped in design round 1"),
    ] {
        let mut run = group_run();
        run["design_group"]["members"][1]["step"] = json!(step);
        let Some(out) = open_run(run, vec![], vec![]) else {
            return;
        };
        let texts = texts(last(&out));
        assert!(texts.contains(&said.to_string()), "{step}: {texts:?}");
    }
}

/// The design image paths the page asked for.
fn images_asked(out: &Value) -> Vec<String> {
    out["requests"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["url"].as_str())
        .filter_map(|url| url.split_once("/design/image?path="))
        .map(|(_, path)| path.replace("%2F", "/"))
        .collect()
}

#[test]
fn a_folded_members_images_are_asked_for_only_once_it_is_opened() {
    let mut run = group_run();
    for member in run["design_group"]["members"].as_array_mut().unwrap() {
        let feature = member["feature"].as_str().unwrap().to_string();
        for round in member["design"]["rounds"].as_array_mut().into_iter().flatten() {
            let image = format!("members/{feature}/round-{}/desktop/index.png", round["round"]);
            round["viewports"] = json!([{"name": "desktop", "width": 1440, "height": 900, "reference": null, "captures": [{"route": "/", "image": image, "failure": null}]}]);
        }
    }
    let png = json!({"status": 200, "json": {"content_type": "image/png", "data": "iVBORw0KGgo="}});
    let pngs = vec![png.clone(), png.clone(), png.clone(), png];

    let Some(folded) = open_run(run.clone(), pngs.clone(), vec![]) else {
        return;
    };
    let asked = images_asked(&folded);
    assert!(asked.iter().any(|path| path.starts_with("members/shop-cards/")), "the open member's: {asked:?}");
    assert!(!asked.iter().any(|path| path.starts_with("members/shop-header/")), "nothing of the folded member: {asked:?}");

    let opened = open_run(run, pngs, vec![json!({"click": "[data-member-design='shop-header'] summary"})]).unwrap();
    let asked = images_asked(&opened);
    assert!(asked.iter().any(|path| path.starts_with("members/shop-header/")), "once opened: {asked:?}");
}

#[test]
fn the_runs_list_shows_which_member_of_how_many_a_design_group_run_is_at() {
    let row = |id: &str, group: Value| {
        let mut row = json!({"run_id": id, "feature": id, "target_branch": "main", "state": "running", "stage": "design_review", "started_at": "2026-11-02T10:00:00Z", "updated_at": "2026-11-02T10:30:00Z"});
        if !group.is_null() {
            row["design_group"] = group;
        }
        row
    };
    let runs = json!({"status": 200, "json": {"runs": [row("group-run", json!({"at": 3, "of": 7})), row("single-run", Value::Null)]}});

    let Some(out) = harness(
        json!({"url": "/projects/alpha/orchestrations", "responses": [runs, projects()], "steps": [{"submit": TOKEN}], "selectors": ["[data-run-group]"]}),
    ) else {
        return;
    };

    let end = last(&out);
    assert_eq!(
        count(end, "[data-run-group]"),
        1,
        "only the group run: {out}"
    );
    assert!(
        texts(end).contains(&"3 of 7".to_string()),
        "{:?}",
        texts(end)
    );
}

// The server's side: the run page and the runs list as StoreProjects reads them from a design group run's events
// and its members' own design folders.

struct Setup {
    roots: StorageRoots,
    scan: PathBuf,
    project: String,
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
    let scan = base.join("code");
    let path = scan.join("alpha");
    std::fs::create_dir_all(&path).unwrap();
    assert!(Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(&path)
        .status()
        .unwrap()
        .success());
    let described = |name: &str, components: &str| json!({"featureName": name, "dependsOn": [], "status": "in_progress", "kind": "frontend", "description": format!("Design package: shop\nDesign components: {components}")});
    let ledger = json!([
        described("shop-header", "header, nav"),
        described("shop-cards", "cards"),
        described("shop-footer", "footer")
    ]);
    std::fs::write(path.join(".tdd-features.json"), ledger.to_string()).unwrap();
    let project = orq_git::project_id(&orq_git::resolve_project(&path).unwrap());
    Setup {
        roots,
        scan,
        project,
    }
}

const PNG: &[u8] = b"\x89PNG\r\n\x1a\nmember capture";

fn write(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// Design round `round` of `member` in its own design folder: a desktop capture and the review's answer.
fn member_round(s: &Setup, run: &str, member: &str, round: u32, verdict: &str, issue: &str) {
    let dir = s
        .roots
        .state
        .join("logs")
        .join(run)
        .join("members")
        .join(member)
        .join("design");
    let scope = json!({"feature": member, "package": "shop", "components": [member], "done": []});
    write(&dir.join("scope.json"), scope.to_string().as_bytes());
    let round_dir = dir.join(format!("round-{round}"));
    write(&round_dir.join("desktop/index.png"), PNG);
    let capture = json!({"round": round, "commit": "abc", "pages": [{"route": "/", "viewport": "desktop", "viewport_width": 1440, "viewport_height": 900, "image": "desktop/index.png", "failure": null}], "failures": []});
    write(
        &round_dir.join("capture.json"),
        capture.to_string().as_bytes(),
    );
    let issues = json!([{"id": issue, "section": "Hero", "viewport": "all", "expected": "a", "actual": "b", "evidence": [], "severity": "major", "status": "open", "fix_direction": "c", "basis": "visual"}]);
    let review = json!({"run_id": run, "reference_hash": "1".repeat(64), "capture_revision": "abc", "evaluation": "compared", "verdict": verdict, "checks": [], "issues": issues, "suggestions": [], "blockers": []});
    let record = json!({"run_id": run, "round": round, "verdict": verdict, "reason": null, "decided_by": "design reviewer", "escalation": null, "review": review, "reviewer_review": null});
    write(
        &round_dir.join("review.json"),
        record.to_string().as_bytes(),
    );
}

/// A design group run of shop-header, shop-cards and shop-footer: shop-header passed after two design rounds,
/// shop-cards stopped needing review in its first, shop-footer waits. Events as (kind, stage, payload).
fn stored_group_run(s: &Setup, run: &str) {
    let store = RunStore::new(&s.roots.state);
    store
        .create(&RunRecord {
            run_id: run.into(),
            project_id: s.project.clone(),
            feature_id: "shop-header".into(),
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
    let step = |member: &str, index: u32, step: &str| {
        (
            "design_group.member_step",
            None,
            json!({"member": member, "index": index, "of": 3, "step": step}),
        )
    };
    let member_event = |kind: &str, member: &str, index: u32| {
        (
            kind.to_string(),
            None,
            json!({"member": member, "index": index, "of": 3, "step": kind}),
        )
    };
    let review = |member: &str, round: u32, verdict: &str| {
        (
            "design_review",
            Some("design_review"),
            json!({"member": member, "round": round, "of": 3, "verdict": verdict, "head": "abc", "batches": 0, "issues": []}),
        )
    };
    let mut events: Vec<(String, Option<&str>, Value)> = vec![(
        "run.grouped".into(),
        None,
        json!({"features": ["shop-header", "shop-cards", "shop-footer"]}),
    )];
    let mut push = |(kind, stage, payload): (&str, Option<&'static str>, Value)| {
        events.push((kind.to_string(), stage, payload))
    };
    push(step("shop-header", 1, "implement"));
    push(step("shop-header", 1, "gate"));
    push(step("shop-header", 1, "design"));
    push(review("shop-header", 1, "changes_required"));
    push(review("shop-header", 2, "pass"));
    push(step("shop-cards", 2, "implement"));
    push(step("shop-cards", 2, "gate"));
    push(step("shop-cards", 2, "design"));
    push(review("shop-cards", 1, "changes_required"));
    push((
        "run.completed",
        Some("design_review"),
        json!({"ending": "needs_review", "reason": "shop-cards: the design still requires changes"}),
    ));
    events.insert(
        6,
        member_event("design_group.member_passed", "shop-header", 1),
    );
    for (n, (kind, stage, payload)) in events.into_iter().enumerate() {
        store
            .append_event(&Event {
                schema_version: 1,
                event_id: format!("{run}-{n}"),
                run_id: run.into(),
                project_id: s.project.clone(),
                feature_id: "shop-header".into(),
                kind,
                stage: stage.map(String::from),
                attempt: 1,
                occurred_at: "2026-11-01T10:00:00Z".into(),
                payload,
            })
            .unwrap();
    }
    member_round(s, run, "shop-header", 1, "changes_required", "header-1");
    member_round(s, run, "shop-header", 2, "pass", "header-2");
    member_round(s, run, "shop-cards", 1, "changes_required", "cards-1");
}

fn get(s: &Setup, path: &str) -> (u16, Value) {
    let projects = StoreProjects::new(s.roots.clone(), s.scan.clone());
    let config = RemoteConfig {
        token: "tok".into(),
    };
    let request = Request {
        method: "GET",
        path,
        peer: "127.0.0.1".parse().unwrap(),
        authorization: Some("Bearer tok"),
    };
    let r = orq_core::remote_inbox::handle_with(
        &s.roots,
        &config,
        "server-project",
        "server",
        &request,
        &projects,
    );
    (r.status, r.body)
}

#[test]
fn the_run_page_reads_each_members_state_components_and_own_design_rounds() {
    let s = setup("design-group-run-page");
    stored_group_run(&s, "r-1");

    let (status, body) = get(&s, "/api/projects/alpha/runs/r-1");

    assert_eq!(status, 200, "{body}");
    let group = &body["run"]["design_group"];
    assert_eq!(
        (&group["at"], &group["of"], &group["stopped"]),
        (&json!(2), &json!(3), &json!("shop-cards")),
        "{group}"
    );
    let members = group["members"].as_array().unwrap();
    let shown: Vec<(&str, &str, Value)> = members
        .iter()
        .map(|m| {
            (
                m["feature"].as_str().unwrap(),
                m["state"].as_str().unwrap(),
                m["round"].clone(),
            )
        })
        .collect();
    assert_eq!(
        shown,
        [
            ("shop-header", "passed", json!(2)),
            ("shop-cards", "stopped", json!(1)),
            ("shop-footer", "waiting", Value::Null)
        ],
        "{group}"
    );
    // A member that reached its design loop covers what its frozen scope names; one still waiting, what the ledger says.
    assert_eq!(members[0]["components"], json!(["shop-header"]), "{group}");
    assert_eq!(members[2]["components"], json!(["footer"]), "{group}");
    // The step the member it is at was in when it stopped; the others are at none.
    let steps: Vec<&Value> = members.iter().map(|m| &m["step"]).collect();
    assert_eq!(steps, [&Value::Null, &json!("design"), &Value::Null], "{group}");
    let rounds = |m: &Value| {
        m["design"]["rounds"].as_array().map(|rounds| {
            rounds
                .iter()
                .map(|r| {
                    (
                        r["round"].as_u64().unwrap(),
                        r["issues"][0]["id"].as_str().unwrap().to_string(),
                    )
                })
                .collect::<Vec<_>>()
        })
    };
    assert_eq!(
        rounds(&members[0]),
        Some(vec![
            (1, "header-1".to_string()),
            (2, "header-2".to_string())
        ]),
        "{group}"
    );
    assert_eq!(
        rounds(&members[1]),
        Some(vec![(1, "cards-1".to_string())]),
        "{group}"
    );
    assert_eq!(members[2]["design"], Value::Null, "{group}");
    assert_eq!(
        members[1]["design"]["rounds"][0]["viewports"][0]["captures"][0]["image"],
        json!("members/shop-cards/round-1/desktop/index.png"),
        "{group}"
    );
    assert!(
        body["run"].get("design").is_none(),
        "no rounds of the run's own: {body}"
    );

    let (status, image) = get(&s, "/api/projects/alpha/runs/r-1/design/image?path=members/shop-cards/round-1/desktop/index.png");
    assert_eq!(status, 200, "a member's capture is served: {image}");
    assert_eq!(image["content_type"], "image/png");
    for refused in [
        "members/shop-footer/round-1/desktop/index.png",
        "members/../design/round-1/desktop/index.png",
        "members/shop-cards/round-2/desktop/index.png",
    ] {
        assert_eq!(
            get(
                &s,
                &format!("/api/projects/alpha/runs/r-1/design/image?path={refused}")
            )
            .0,
            404,
            "{refused}"
        );
    }
}

#[test]
fn a_members_image_behind_a_link_anywhere_on_its_way_is_refused() {
    const IMAGE: &str = "members/shop-cards/round-1/desktop/index.png";
    // Each folder on the way to a member's capture, and the capture itself, from the run's logs.
    for linked in [
        "members",
        "members/shop-cards",
        "members/shop-cards/design",
        "members/shop-cards/design/round-1/desktop/index.png",
    ] {
        let s = setup("design-group-member-link");
        stored_group_run(&s, "r-1");
        let logs = s.roots.state.join("logs").join("r-1");
        let route = format!("/api/projects/alpha/runs/r-1/design/image?path={IMAGE}");
        assert_eq!(get(&s, &route).0, 200, "served before {linked} is a link");

        // The same content, moved aside and linked back where it was.
        let (at, aside) = (logs.join(linked), logs.join("aside"));
        std::fs::rename(&at, &aside).unwrap();
        std::os::unix::fs::symlink(&aside, &at).unwrap();

        assert_eq!(get(&s, &route).0, 404, "{linked} is a link");
    }
}

#[test]
fn the_runs_list_row_says_which_member_of_how_many_a_design_group_run_is_at() {
    let s = setup("design-group-runs-row");
    stored_group_run(&s, "r-1");

    let (status, body) = get(&s, "/api/projects/alpha/runs");

    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body["runs"][0]["design_group"],
        json!({"at": 2, "of": 3}),
        "{body}"
    );
}

fn posts(out: &Value) -> Vec<(String, Value)> {
    out["requests"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["method"] == "POST")
        .map(|r| {
            (
                r["url"].as_str().unwrap().to_string(),
                r["body"]
                    .as_str()
                    .and_then(|body| serde_json::from_str(body).ok())
                    .unwrap_or(Value::Null),
            )
        })
        .collect()
}

#[test]
fn add_design_issues_names_the_member_that_stopped_and_sends_the_issues_for_it() {
    let field = |name: &str| format!("[data-person-issue='0'] [data-issue-{name}]");
    let steps = vec![
        json!({"click": "[data-design-issues]"}),
        json!({"set": field("section"), "value": "Hero"}),
        json!({"set": field("expected"), "value": "the cards line up"}),
        json!({"set": field("actual"), "value": "the cards overlap"}),
        json!({"click": "[data-design-issues-send]"}),
    ];

    let Some(out) = open_run(
        group_run(),
        vec![json!({"status": 202, "json": {"run": "r-1-r1"}})],
        steps,
    ) else {
        return;
    };

    let first = &out["frames"][1];
    assert_eq!(
        count(first, "[data-design-issues]"),
        1,
        "a group stopped in a member's design review takes issues: {out}"
    );
    let form = &out["frames"][2];
    assert_eq!(count(form, "[data-design-issues-member]"), 1, "{out}");
    assert!(
        texts(form).contains(&"For shop-cards".to_string()),
        "{:?}",
        texts(form)
    );
    let issue = json!({"section": "Hero", "viewport": "all", "expected": "the cards line up", "actual": "the cards overlap", "severity": "major"});
    assert_eq!(
        posts(&out),
        [(
            "/api/projects/alpha/runs/r-1/design-issues".to_string(),
            json!({"issues": [issue]})
        )],
        "the run resumes with the member it stopped at: {out}"
    );
    assert!(
        texts(last(&out))
            .contains(&"Correcting your issues for shop-cards as run r-1-r1.".to_string()),
        "{out}"
    );
}
