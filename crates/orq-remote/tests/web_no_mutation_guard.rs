//! The page offers exactly four controls — start, pause, resume and stop — each behind a confirmation,
//! and no merge, approve, restart or edit control or request.

fn code() -> String {
    std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/web-src/src/main.jsx")).unwrap()
}

#[test]
fn the_page_has_only_the_start_pause_resume_and_stop_actions() {
    let code = code();
    let calls: Vec<&str> = code.split("<RunAction ").skip(1).map(|call| call.split(" />").next().unwrap()).collect();
    let kinds: Vec<&str> = calls.iter().map(|call| call.split("kind=\"").nth(1).unwrap().split('"').next().unwrap()).collect();
    assert_eq!(kinds, ["start", "pause", "resume", "stop"]);
    let tail = |call: &str| call.split("controlPath(control, ").nth(1).unwrap().to_string();
    assert!(tail(calls[0]).starts_with("\"features/\"") && tail(calls[0]).contains("+ \"/runs\")"), "{}", tail(calls[0]));
    assert!(tail(calls[1]).starts_with("\"runs/\"") && tail(calls[1]).contains("+ \"/pause\")"), "{}", tail(calls[1]));
    assert!(tail(calls[2]).starts_with("\"runs/\"") && tail(calls[2]).contains("+ \"/resume\")"), "{}", tail(calls[2]));
    assert!(tail(calls[3]).starts_with("\"runs/\"") && tail(calls[3]).contains("+ \"/stop\")"), "{}", tail(calls[3]));
    assert_eq!(code.matches("controlPath(control, ").count(), 5, "the definition and the four actions are the only users");
    for banned in ["merge", "approve", "restart", "edit", "delete", "cancel"] {
        assert!(!code.contains(&format!("\"/{banned}")) && !code.contains(&format!("/{banned}\"")) && !code.contains(&format!("/{banned}/")), "the page builds a {banned} request");
    }
}
