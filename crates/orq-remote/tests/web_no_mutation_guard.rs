//! The page offers exactly sixteen run and feature controls — start, start with chosen profiles, start marked, promote, promote marked, build a missing review report, pause, resume, review again, stop, discard, accept, merge an accepted merge-local run (through the accept route), queue a run's resume, review again or accept, set aside my changes and merge, clean up a merged run, and switch model — plus the feature editor, the storage cleanup, the kept worktrees' delete and the run queue's add, pause, resume, remove and reorder, each behind a confirmation,
//! and a GitHub project's Push <target> of the sha it showed, never forced,
//! and the Settings server card's restart when idle and its cancel, the Todo list's add, edit, delete and move,
//! and a draft's queue wish, and no other merge, nor any approve,
//! restart or edit control or request.

fn code() -> String {
    std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/web-src/src/main.jsx")).unwrap()
}

#[test]
fn the_page_has_only_the_start_start_with_promote_pause_resume_stop_discard_accept_set_aside_merge_and_model_actions() {
    let code = code();
    let calls: Vec<&str> = code.split("<RunAction ").skip(1).map(|call| call.split(" />").next().unwrap()).collect();
    // A literal kind, or {kind} for the one queueing helper that names its kind per use.
    let kinds: Vec<&str> = calls.iter().map(|call| match call.split("kind=\"").nth(1) {
        Some(kind) => kind.split('"').next().unwrap(),
        None => call.split("kind={").nth(1).unwrap().split('}').next().unwrap(),
    }).collect();
    assert_eq!(kinds, ["start", "promote", "build-report", "pause", "resume", "stop", "discard", "accept", "merge-accepted", "review-again", "kind", "merge-set-aside", "run-cleanup"]);
    let tail = |call: &str| call.split("controlPath(control, ").nth(1).unwrap().to_string();
    assert!(tail(calls[0]).starts_with("\"features/\"") && tail(calls[0]).contains("+ \"/runs\")"), "{}", tail(calls[0]));
    assert!(tail(calls[1]).starts_with("\"features/\"") && tail(calls[1]).contains("+ \"/promote\")"), "{}", tail(calls[1]));
    assert!(!calls[1].contains("check="), "promote asks nothing of the server first");
    assert!(tail(calls[2]).starts_with("\"runs/\"") && tail(calls[2]).contains("+ \"/build-report\")"), "{}", tail(calls[2]));
    assert!(!calls[2].contains("check="), "building the report asks nothing of the server first");
    let calls = [&calls[..2], &calls[3..]].concat();
    assert!(tail(calls[2]).starts_with("\"runs/\"") && tail(calls[2]).contains("+ \"/pause\")"), "{}", tail(calls[2]));
    assert!(tail(calls[3]).starts_with("\"runs/\"") && tail(calls[3]).contains("+ \"/resume\")"), "{}", tail(calls[3]));
    let check = calls[3].split("check={controlPath(control, ").nth(1).expect("resume asks the resume check first");
    assert!(check.starts_with("\"runs/\"") && check.contains("+ \"/resume-check\")"), "{check}");
    assert!(tail(calls[4]).starts_with("\"runs/\"") && tail(calls[4]).contains("+ \"/stop\")"), "{}", tail(calls[4]));
    assert!(tail(calls[5]).starts_with("\"runs/\"") && tail(calls[5]).contains("+ \"/discard\")"), "{}", tail(calls[5]));
    let warning = calls[5].split("check={controlPath(control, ").nth(1).expect("discard asks the discard warning first");
    assert!(warning.starts_with("\"runs/\"") && warning.contains("+ \"/discard-warning\")"), "{warning}");
    // Accept, merging an accepted merge-local run, and review again post through the row's run path:
    // the first two to the run's accept route, the last to its review-again route; nothing else.
    const RUN_PATH: &str = "const runPath = (tail) => controlPath(control, \"runs/\" + encodeURIComponent(run.run_id) + \"/\" + tail);";
    assert_eq!(code.matches(RUN_PATH).count(), 1, "the row's run path is defined once");
    assert_eq!(code.matches("runPath(").count(), 3, "only Accept, Merge into and Review again use the row's run path");
    assert!(calls[6].contains("path={runPath(\"accept\")}"), "{}", calls[6]);
    assert!(!calls[6].contains("check="), "accept asks nothing of the server first");
    assert!(calls[7].contains("path={runPath(\"accept\")}"), "{}", calls[7]);
    assert!(!calls[7].contains("check="), "merging an accepted run asks nothing of the server first");
    assert!(calls[8].contains("path={runPath(\"review-again\")}"), "{}", calls[8]);
    assert!(!calls[8].contains("check="), "review again asks nothing of the server first");
    let calls = [&calls[..7], &calls[8..]].concat();
    // Queueing a run's Resume, Review again or Accept posts to the project's queue add route; nothing else.
    assert!(tail(calls[8]).starts_with("\"queue/add\")"), "{}", tail(calls[8]));
    assert!(!calls[8].contains("check="), "queueing asks nothing of the server first");
    let queued_kinds: Vec<&str> = code.split("queued(\"").skip(1).map(|tail| tail.split('"').next().unwrap()).collect();
    assert_eq!(queued_kinds, ["queue-run-review-again", "queue-run-resume", "queue-run-accept"]);
    // The one merge the page offers: a blocked run's set-aside merge, at exactly this route.
    const SET_ASIDE: &str = "\"runs/\" + encodeURIComponent(run.run_id) + \"/merge-set-aside\")";
    assert!(tail(calls[9]).starts_with(SET_ASIDE), "{}", tail(calls[9]));
    assert!(!calls[9].contains("check="), "the set-aside merge asks nothing of the server first");
    // A merged run's Clean up posts to the run's cleanup route, built once in its card; nothing else.
    assert!(calls[10].contains("path={path}") && !calls[10].contains("check="), "{}", calls[10]);
    let cleanup_body = code.split("function CleanupCard(").nth(1).expect("CleanupCard").split("\nfunction ").next().unwrap();
    let cleanup: Vec<&str> = cleanup_body.split("controlPath(control, ").skip(1).map(|tail| tail.split(';').next().unwrap()).collect();
    assert_eq!(cleanup, ["\"runs/\" + encodeURIComponent(run.run_id) + \"/cleanup\")"]);
    // The model switch and the launch picker read the launch profiles through one reader; nothing else.
    let paths = |name: &str| -> Vec<String> {
        let body = code.split(&format!("function {name}(")).nth(1).unwrap_or_else(|| panic!("{name}")).split("\nfunction ").next().unwrap();
        // A function ends at the next top-level one, async or not.
        let body = body.split("\nasync function ").next().unwrap();
        body.split("controlPath(control, ").skip(1).map(|tail| tail.split(';').next().unwrap().to_string()).collect()
    };
    let read = paths("readLaunchProfiles");
    assert_eq!(read.len(), 1, "{read:?}");
    assert!(read[0].starts_with("\"launch-profiles\")"), "{}", read[0]);
    // Switch model posts to the run's model route; nothing else.
    let model = paths("ModelAction");
    assert_eq!(model.len(), 1, "{model:?}");
    assert!(model[0].starts_with("\"runs/\"") && model[0].ends_with("+ \"/model\")"), "{}", model[0]);
    // Start with… posts the chosen profiles to the feature's start route; nothing else.
    let launch = paths("LaunchAction");
    assert_eq!(launch.len(), 1, "{launch:?}");
    assert!(launch[0].starts_with("\"features/\"") && launch[0].ends_with("+ \"/runs\")"), "{}", launch[0]);
    // Promote marked posts to each marked feature's promote route; nothing else.
    let promoted = paths("PromoteMarked");
    assert_eq!(promoted.len(), 1, "{promoted:?}");
    assert!(promoted[0].starts_with("\"features/\"") && promoted[0].contains("+ \"/promote\")"), "{}", promoted[0]);
    // Start marked posts the chosen profiles to each marked feature's start route; nothing else.
    let started = paths("StartMarked");
    assert_eq!(started.len(), 1, "{started:?}");
    assert!(started[0].starts_with("\"features/\"") && started[0].contains("+ \"/runs\")"), "{}", started[0]);
    // The feature editor reads the feature's fields and posts to the ledger (add) or the feature (edit); nothing else.
    let editor = paths("FeatureEditor");
    assert_eq!(editor.len(), 2, "{editor:?}");
    assert!(editor[0].starts_with("\"features/\"") && editor[0].ends_with("+ \"/fields\"), control.token)"), "{}", editor[0]);
    assert!(editor[1].starts_with("adding ? \"features\" : \"features/\" + encodeURIComponent(name))"), "{}", editor[1]);
    // The Storage card reads the cleanup preview and posts the prune with its hash; nothing else.
    let storage: Vec<&str> = code.split("function StorageCleanup(").nth(1).expect("StorageCleanup").split("\nfunction ").next().unwrap().split("API_SETTINGS + ").skip(1).map(|tail| tail.split(',').next().unwrap()).collect();
    assert_eq!(storage, ["\"/storage/preview\"", "\"/storage/prune\""]);
    // The Kept worktrees card reads the kept worktrees and posts a delete of one, or of the merged ones; nothing else.
    let kept_body = code.split("function KeptWorktrees(").nth(1).expect("KeptWorktrees").split("\nfunction ").next().unwrap();
    let kept: Vec<&str> = kept_body.split("API_WORKTREES").skip(1).map(|tail| tail.split(',').next().unwrap()).collect();
    assert_eq!(kept, [""," + \"/\" + encodeURIComponent(project) + \"/\" + branchPath(worktree.branch) + \"/delete\"", " + \"/delete-merged\""]);
    // The Server card posts the restart when idle and its cancel, to the one restart route; nothing else.
    const RESTART_ROUTE: &str = "const API_SERVER_RESTART = \"/api/server/restart\";";
    assert_eq!(code.matches(RESTART_ROUTE).count(), 1, "the restart route is defined once");
    let server_body = code.split("function ServerCard(").nth(1).expect("ServerCard").split("\nfunction ").next().unwrap();
    let sent: Vec<&str> = server_body.split("send(").skip(1).filter(|tail| !tail.starts_with("path)")).map(|tail| tail.split(')').next().unwrap()).collect();
    assert_eq!(sent, ["API_SERVER_RESTART + \"/cancel\"", "API_SERVER_RESTART"]);
    assert_eq!(code.matches("API_SERVER_RESTART").count(), 3, "only the Server card uses the restart route");
    // A Todo item posts its delete, and its Edit popup its edit, to the item's own path under the Todo list; nothing else.
    let body_of = |name: &str| code.split(&format!("function {name}(")).nth(1).unwrap_or_else(|| panic!("{name}")).split("\nfunction ").next().unwrap();
    let todo_body = body_of("TodoItem");
    let edit_body = body_of("TodoEditDialog");
    let todo_paths = |body: &str| -> Vec<String> { body.split("todoSend(").skip(1).map(|tail| tail.split(',').next().unwrap().to_string()).collect() };
    assert_eq!(todo_paths(todo_body), ["API_TODO + \"/\" + item.id + \"/delete\""]);
    assert_eq!(todo_paths(edit_body), ["API_TODO + \"/\" + item.id"]);
    // Adding to the Todo list, and a move out of it, post under the Todo list; a draft's queue wish posts to its own route.
    assert_eq!(code.matches("todoSend(").count(), 5, "the helper, a Todo item's delete, its Edit popup's save, Todo's add and the wish's cancel");
    assert!(code.contains("todoSend(API_TODO, token, onSignedOut, { name, description }"), "Todo's add");
    assert!(code.contains("path: API_TODO + \"/\" + item.id + \"/move\""), "a Todo item's move");
    assert!(code.contains("encodeURIComponent(feature.name) + \"/queue-wish\";"), "the queue wish");
    // A GitHub project's Push <target> reads the push's checks and posts the confirmed sha to the project's
    // push-target route, from its one component; no other push, and never a force.
    let push_body = body_of("PushTarget");
    const PUSH_ROUTE: &str = "const path = \"/api/projects/\" + encodeURIComponent(push.projectId) + \"/push-target\";";
    assert_eq!(code.matches("/push-target").count(), 1, "only Push <target> builds the push-target route");
    assert!(push_body.contains(PUSH_ROUTE), "{push_body}");
    let pushed: Vec<&str> = push_body.split("actionRequest(").skip(1).map(|tail| tail.split(';').next().unwrap()).collect();
    assert_eq!(pushed, ["path, push.token, { sha: state.sha })"], "Push N commits posts the sha it showed, nothing else");
    assert!(!push_body.to_lowercase().contains("force"), "Push <target> asks for no forced push");
    let rest = code.replacen(kept_body, "", 1).replacen(SET_ASIDE, "", 1).replacen(server_body, "", 1).replacen(RESTART_ROUTE, "", 1).replacen(todo_body, "", 1).replacen(edit_body, "", 1).replacen(push_body, "", 1);
    // Queueing posts the chosen features to the project's queue; the queue view posts to its pause,
    // resume, remove and reorder routes; nothing else.
    let queued = paths("queueFeatures");
    assert_eq!(queued.len(), 1, "{queued:?}");
    assert!(queued[0].starts_with("\"queue\")"), "{}", queued[0]);
    let queue_view = paths("QueueView");
    // The queue view also retries the pull of a target a merge left behind, at the project's own pull route.
    assert_eq!(queue_view.len(), 2, "{queue_view:?}");
    assert!(queue_view[0].starts_with("\"queue/\" + tail)"), "{}", queue_view[0]);
    assert_eq!(queue_view[1], "\"pull\"), control.token, {})", "{queue_view:?}");
    let queue_body = code.split("function QueueView(").nth(1).unwrap().split("\nfunction ").next().unwrap();
    let tails: Vec<&str> = queue_body.split("change(\"").skip(1).map(|tail| tail.split('"').next().unwrap()).collect();
    assert_eq!(tails, ["reorder", "resume", "pause", "remove"]);
    assert_eq!(code.matches("controlPath(control, ").count(), 24, "the definition, the seven run actions with their own path, a merged run's clean up, the row's run path, queueing a run's action, the two checks, the profiles read, the model switch, the launch picker, start marked, promote marked, the feature editor's read and save, queueing, the queue view's changes and its target pull retry are the only users");
    for banned in ["merge", "approve", "restart", "edit", "delete", "cancel"] {
        assert!(!rest.contains(&format!("\"/{banned}")) && !rest.contains(&format!("/{banned}\"")) && !rest.contains(&format!("/{banned}/")), "the page builds a {banned} request");
    }
}
