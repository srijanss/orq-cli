use std::cell::RefCell;

use orq_herdr::{failure_reason, focus_pane, FocusOutcome, HerdrCli, PaneLocation};

struct Fake {
    replies: RefCell<Vec<Result<String, String>>>,
    calls: RefCell<Vec<Vec<String>>>,
}

impl Fake {
    fn new(replies: Vec<Result<String, String>>) -> Self {
        Fake { replies: RefCell::new(replies), calls: RefCell::new(vec![]) }
    }
}

impl HerdrCli for Fake {
    fn run(&self, args: &[String]) -> Result<String, String> {
        self.calls.borrow_mut().push(args.to_vec());
        self.replies.borrow_mut().remove(0)
    }
}

fn pane() -> PaneLocation {
    PaneLocation { workspace_id: "w1".into(), tab_id: "w1:t2".into(), pane_id: "w1:p5".into() }
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| a.to_string()).collect()
}

#[test]
fn focusing_shows_the_tab_then_the_pane() {
    let herdr = Fake::new(vec![Ok("{}".into()), Ok("{}".into())]);

    assert_eq!(focus_pane(&herdr, &pane()), FocusOutcome::Focused);

    assert_eq!(*herdr.calls.borrow(), vec![strings(&["tab", "focus", "w1:t2"]), strings(&["pane", "focus", "w1:p5"])]);
}

#[test]
fn a_tab_or_pane_that_is_gone_is_told_apart_from_a_herdr_that_cannot_be_reached() {
    let gone = Fake::new(vec![Err("tab_not_found: tab w1:t2 not found".into())]);
    assert!(matches!(focus_pane(&gone, &pane()), FocusOutcome::Gone(reason) if reason.contains("w1:t2")));

    let pane_gone = Fake::new(vec![Ok("{}".into()), Err("pane_not_found: pane w1:p5 not found".into())]);
    assert!(matches!(focus_pane(&pane_gone, &pane()), FocusOutcome::Gone(_)));

    let down = Fake::new(vec![Err("server_not_running: no herdr server is running".into())]);
    assert!(matches!(focus_pane(&down, &pane()), FocusOutcome::Failed(reason) if reason.contains("no herdr server")));
}

#[test]
fn a_failure_reason_comes_from_herdrs_json_error_or_else_its_first_stderr_line() {
    let json = r#"{"error":{"code":"pane_not_found","message":"pane x not found"},"id":"cli:pane:get"}"#;
    assert_eq!(failure_reason(json, ""), "pane_not_found: pane x not found");
    assert_eq!(failure_reason("", "\n  boom happened\nmore"), "boom happened");
    assert_eq!(failure_reason("not json", ""), "herdr failed");
}

#[test]
fn only_a_missing_tab_or_pane_counts_as_gone_and_a_failed_first_step_stops_there() {
    let unrelated = Fake::new(vec![Err("command_not_found: no such herdr command".into())]);
    assert!(matches!(focus_pane(&unrelated, &pane()), FocusOutcome::Failed(_)));
    assert_eq!(*unrelated.calls.borrow(), vec![strings(&["tab", "focus", "w1:t2"])]);

    let second = Fake::new(vec![Ok("{}".into()), Err("permission_denied: no".into())]);
    assert!(matches!(focus_pane(&second, &pane()), FocusOutcome::Failed(reason) if reason.contains("permission_denied")));
}
