//! The page offers exactly eight controls — start, promote, pause, resume, stop, discard, accept and switch model — each behind a confirmation,
//! and no merge, approve, restart or edit control or request.

fn code() -> String {
    std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/web-src/src/main.jsx")).unwrap()
}

#[test]
fn the_page_has_only_the_start_promote_pause_resume_stop_discard_accept_and_model_actions() {
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
    // Switch model reads the launch profiles, then posts to the run's model route; nothing else.
    let model = code.split("function ModelAction(").nth(1).expect("the switch model control").split("\nfunction ").next().unwrap();
    let paths: Vec<&str> = model.split("controlPath(control, ").skip(1).map(|tail| tail.split(';').next().unwrap()).collect();
    assert_eq!(paths.len(), 2, "{paths:?}");
    assert!(paths[0].starts_with("\"runs/\"") && paths[0].ends_with("+ \"/model\")"), "{}", paths[0]);
    assert!(paths[1].starts_with("\"launch-profiles\")"), "{}", paths[1]);
    assert_eq!(code.matches("controlPath(control, ").count(), 12, "the definition, the seven actions, the two checks and the model switch's two paths are the only users");
    for banned in ["merge", "approve", "restart", "edit", "delete", "cancel"] {
        assert!(!code.contains(&format!("\"/{banned}")) && !code.contains(&format!("/{banned}\"")) && !code.contains(&format!("/{banned}/")), "the page builds a {banned} request");
    }
}
