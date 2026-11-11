//! The page offers exactly ten controls — start, start with chosen profiles, promote, promote marked, pause, resume, stop, discard, accept and switch model — each behind a confirmation,
//! and no merge, approve, restart or edit control or request.

fn code() -> String {
    std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/web-src/src/main.jsx")).unwrap()
}

#[test]
fn the_page_has_only_the_start_start_with_promote_pause_resume_stop_discard_accept_and_model_actions() {
    let code = code();
    let calls: Vec<&str> = code.split("<RunAction ").skip(1).map(|call| call.split(" />").next().unwrap()).collect();
    let kinds: Vec<&str> = calls.iter().map(|call| call.split("kind=\"").nth(1).unwrap().split('"').next().unwrap()).collect();
    assert_eq!(kinds, ["start", "promote", "pause", "resume", "stop", "discard", "accept"]);
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
    let marked = code.split("function PromoteMarked(").nth(1).expect("the promote marked control").split("\nfunction ").next().unwrap();
    let paths: Vec<&str> = marked.split("controlPath(control, ").skip(1).map(|tail| tail.split(';').next().unwrap()).collect();
    assert_eq!(paths.len(), 1, "{paths:?}");
    assert!(paths[0].starts_with("\"features/\"") && paths[0].contains("+ \"/promote\")"), "{}", paths[0]);
    assert_eq!(code.matches("controlPath(control, ").count(), 14, "the definition, the seven actions, the two checks, the profiles read, the model switch, the launch picker and promote marked are the only users");
    for banned in ["merge", "approve", "restart", "edit", "delete", "cancel"] {
        assert!(!code.contains(&format!("\"/{banned}")) && !code.contains(&format!("/{banned}\"")) && !code.contains(&format!("/{banned}/")), "the page builds a {banned} request");
    }
}
