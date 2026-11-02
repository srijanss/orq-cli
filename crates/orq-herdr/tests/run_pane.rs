use std::cell::RefCell;
use std::path::Path;

use orq_herdr::{open_run_pane, HerdrCli, PaneLocation};

struct Fake {
    calls: RefCell<Vec<Vec<String>>>,
    replies: RefCell<Vec<Result<String, String>>>,
}

impl Fake {
    fn new(replies: Vec<Result<String, String>>) -> Self {
        Fake { calls: RefCell::new(vec![]), replies: RefCell::new(replies) }
    }
}

impl HerdrCli for Fake {
    fn run(&self, args: &[String]) -> Result<String, String> {
        self.calls.borrow_mut().push(args.to_vec());
        self.replies.borrow_mut().remove(0)
    }
}

const CREATED: &str = r#"{"id":"cli:tab:create","result":{"root_pane":{"pane_id":"wR:p5","tab_id":"wR:t2","workspace_id":"wR"},"tab":{"tab_id":"wR:t2"},"type":"tab_created"}}"#;

#[test]
fn a_run_gets_a_tab_that_tails_its_log_and_the_location_is_returned() {
    let herdr = Fake::new(vec![Ok(CREATED.into()), Ok(r#"{"result":{"type":"ok"}}"#.into())]);

    let location = open_run_pane(&herdr, Some("wR"), Path::new("/repo"), "orq run-9", Path::new("/state/it's/runner.log")).unwrap();

    assert_eq!(
        location,
        PaneLocation { workspace_id: "wR".into(), tab_id: "wR:t2".into(), pane_id: "wR:p5".into() }
    );
    let calls = herdr.calls.borrow();
    assert_eq!(
        calls[0],
        ["tab", "create", "--workspace", "wR", "--cwd", "/repo", "--label", "orq run-9", "--no-focus"]
    );
    assert_eq!(calls[1][..3], ["pane", "run", "wR:p5"]);
    assert_eq!(calls[1][3], r"tail -n +1 -f '/state/it'\''s/runner.log'");
}

#[test]
fn failures_say_which_step_and_a_failed_command_closes_the_new_tab() {
    let no_herdr = Fake::new(vec![Err("herdr is not running".into())]);
    let err = open_run_pane(&no_herdr, None, Path::new("/repo"), "l", Path::new("/log")).unwrap_err();
    assert!(err.contains("herdr is not running"), "{err}");
    assert!(!no_herdr.calls.borrow()[0].contains(&"--workspace".to_string()));

    let garbled = Fake::new(vec![Ok("not json".into())]);
    assert!(open_run_pane(&garbled, None, Path::new("/repo"), "l", Path::new("/log")).is_err());

    let run_fails = Fake::new(vec![Ok(CREATED.into()), Err("pane gone".into()), Ok("{}".into())]);
    let err = open_run_pane(&run_fails, None, Path::new("/repo"), "l", Path::new("/log")).unwrap_err();
    assert!(err.contains("pane gone"), "{err}");
    assert_eq!(run_fails.calls.borrow()[2], ["tab", "close", "wR:t2"]);
}
