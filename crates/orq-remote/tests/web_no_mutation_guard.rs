//! The page offers exactly twelve run and feature controls — start, start with chosen profiles, start marked, promote, promote marked, pause, resume, stop, discard, accept, set aside my changes and merge, and switch model — plus the feature editor, the storage cleanup, the kept worktrees' delete and the run queue's add, pause, resume, remove and reorder, each behind a confirmation,
//! and no other merge, nor any approve, restart or edit control or request.

fn code() -> String {
    std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/web-src/src/main.jsx")).unwrap()
}

#[test]
fn the_page_has_only_the_start_start_with_promote_pause_resume_stop_discard_accept_set_aside_merge_and_model_actions() {
    let code = code();
    let calls: Vec<&str> = code.split("<RunAction ").skip(1).map(|call| call.split(" />").next().unwrap()).collect();
    let kinds: Vec<&str> = calls.iter().map(|call| call.split("kind=\"").nth(1).unwrap().split('"').next().unwrap()).collect();
    assert_eq!(kinds, ["start", "promote", "pause", "resume", "stop", "discard", "accept", "merge-set-aside"]);
    let tail = |call: &str| call.split("controlPath(control, ").nth(1).unwrap().to_string();
    assert!(tail(calls[0]).starts_with("\"features/\"") && tail(calls[0]).contains("+ \"/runs\")"), "{}", tail(calls[0]));
    assert!(tail(calls[1]).starts_with("\"features/\"") && tail(calls[1]).contains("+ \"/promote\")"), "{}", tail(calls[1]));
    assert!(!calls[1].contains("check="), "promote asks nothing of the server first");
    assert!(tail(calls[2]).starts_with("\"runs/\"") && tail(calls[2]).contains("+ \"/pause\")"), "{}", tail(calls[2]));
    assert!(tail(calls[3]).starts_with("\"runs/\"") && tail(calls[3]).contains("+ \"/resume\")"), "{}", tail(calls[3]));
    let check = calls[3].split("check={controlPath(control, ").nth(1).expect("resume asks the resume check first");
    assert!(check.starts_with("\"runs/\"") && check.contains("+ \"/resume-check\")"), "{check}");
    assert!(tail(calls[4]).starts_with("\"runs/\"") && tail(calls[4]).contains("+ \"/stop\")"), "{}", tail(calls[4]));
    assert!(tail(calls[5]).starts_with("\"runs/\"") && tail(calls[5]).contains("+ \"/discard\")"), "{}", tail(calls[5]));
    let warning = calls[5].split("check={controlPath(control, ").nth(1).expect("discard asks the discard warning first");
    assert!(warning.starts_with("\"runs/\"") && warning.contains("+ \"/discard-warning\")"), "{warning}");
    assert!(tail(calls[6]).starts_with("\"runs/\"") && tail(calls[6]).contains("+ \"/accept\")"), "{}", tail(calls[6]));
    assert!(!calls[6].contains("check="), "accept asks nothing of the server first");
    // The one merge the page offers: a blocked run's set-aside merge, at exactly this route.
    const SET_ASIDE: &str = "\"runs/\" + encodeURIComponent(run.run_id) + \"/merge-set-aside\")";
    assert!(tail(calls[7]).starts_with(SET_ASIDE), "{}", tail(calls[7]));
    assert!(!calls[7].contains("check="), "the set-aside merge asks nothing of the server first");
    // The model switch and the launch picker read the launch profiles through one reader; nothing else.
    let paths = |name: &str| -> Vec<String> {
        let body = code.split(&format!("function {name}(")).nth(1).unwrap_or_else(|| panic!("{name}")).split("\nfunction ").next().unwrap();
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
    let rest = code.replacen(kept_body, "", 1).replacen(SET_ASIDE, "", 1);
    // Queueing posts the chosen features to the project's queue; the queue view posts to its pause,
    // resume, remove and reorder routes; nothing else.
    let queued = paths("queueFeatures");
    assert_eq!(queued.len(), 1, "{queued:?}");
    assert!(queued[0].starts_with("\"queue\")"), "{}", queued[0]);
    let queue_view = paths("QueueView");
    assert_eq!(queue_view.len(), 1, "{queue_view:?}");
    assert!(queue_view[0].starts_with("\"queue/\" + tail)"), "{}", queue_view[0]);
    let queue_body = code.split("function QueueView(").nth(1).unwrap().split("\nfunction ").next().unwrap();
    let tails: Vec<&str> = queue_body.split("change(\"").skip(1).map(|tail| tail.split('"').next().unwrap()).collect();
    assert_eq!(tails, ["reorder", "resume", "pause", "remove"]);
    assert_eq!(code.matches("controlPath(control, ").count(), 20, "the definition, the eight actions, the two checks, the profiles read, the model switch, the launch picker, start marked, promote marked, the feature editor's read and save, queueing and the queue view's changes are the only users");
    for banned in ["merge", "approve", "restart", "edit", "delete", "cancel"] {
        assert!(!rest.contains(&format!("\"/{banned}")) && !rest.contains(&format!("/{banned}\"")) && !rest.contains(&format!("/{banned}/")), "the page builds a {banned} request");
    }
}
