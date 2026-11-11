//! The page offers exactly six controls — start, pause, resume, stop, discard and accept — each behind a confirmation,
//! and no merge, approve, restart or edit control or request.

fn code() -> String {
    std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/web-src/src/main.jsx")).unwrap()
}

#[test]
fn the_page_has_only_the_start_pause_resume_stop_discard_and_accept_actions() {
    let code = code();
    let calls: Vec<&str> = code.split("<RunAction ").skip(1).map(|call| call.split(" />").next().unwrap()).collect();
    let kinds: Vec<&str> = calls.iter().map(|call| call.split("kind=\"").nth(1).unwrap().split('"').next().unwrap()).collect();
    assert_eq!(kinds, ["start", "pause", "resume", "stop", "discard", "accept"]);
    let tail = |call: &str| call.split("controlPath(control, ").nth(1).unwrap().to_string();
    assert!(tail(calls[0]).starts_with("\"features/\"") && tail(calls[0]).contains("+ \"/runs\")"), "{}", tail(calls[0]));
    assert!(tail(calls[1]).starts_with("\"runs/\"") && tail(calls[1]).contains("+ \"/pause\")"), "{}", tail(calls[1]));
    assert!(tail(calls[2]).starts_with("\"runs/\"") && tail(calls[2]).contains("+ \"/resume\")"), "{}", tail(calls[2]));
    let check = calls[2].split("check={controlPath(control, ").nth(1).expect("resume asks the resume check first");
    assert!(check.starts_with("\"runs/\"") && check.contains("+ \"/resume-check\")"), "{check}");
    assert!(tail(calls[3]).starts_with("\"runs/\"") && tail(calls[3]).contains("+ \"/stop\")"), "{}", tail(calls[3]));
    assert!(tail(calls[4]).starts_with("\"runs/\"") && tail(calls[4]).contains("+ \"/discard\")"), "{}", tail(calls[4]));
    let warning = calls[4].split("check={controlPath(control, ").nth(1).expect("discard asks the discard warning first");
    assert!(warning.starts_with("\"runs/\"") && warning.contains("+ \"/discard-warning\")"), "{warning}");
    assert!(tail(calls[5]).starts_with("\"runs/\"") && tail(calls[5]).contains("+ \"/accept\")"), "{}", tail(calls[5]));
    assert!(!calls[5].contains("check="), "accept asks nothing of the server first");
    assert_eq!(code.matches("controlPath(control, ").count(), 9, "the definition, the six actions and the two checks are the only users");
    for banned in ["merge", "approve", "restart", "edit", "delete", "cancel"] {
        assert!(!code.contains(&format!("\"/{banned}")) && !code.contains(&format!("/{banned}\"")) && !code.contains(&format!("/{banned}/")), "the page builds a {banned} request");
    }
}
