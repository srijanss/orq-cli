use std::cell::RefCell;
use std::path::Path;

use orq_herdr::{open_command_pane, open_grid_command_pane, GridTab, HerdrCli, PaneLocation};

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

const CREATED: &str = r#"{"result":{"root_pane":{"pane_id":"w:p1","tab_id":"w:t2","workspace_id":"w"},"type":"tab_created"}}"#;
const OK: &str = r#"{"result":{"type":"ok"}}"#;

#[test]
fn a_command_pane_runs_the_given_command_verbatim_in_a_new_tab() {
    let herdr = Fake::new(vec![Ok(CREATED.into()), Ok(OK.into())]);

    let location = open_command_pane(&herdr, Some("w"), Path::new("/repo"), "orq run-9", "'/bin/orq' run --run-id run-9").unwrap();

    assert_eq!(location, PaneLocation { workspace_id: "w".into(), tab_id: "w:t2".into(), pane_id: "w:p1".into() });
    let calls = herdr.calls.borrow();
    assert_eq!(calls[0], ["tab", "create", "--workspace", "w", "--cwd", "/repo", "--label", "orq run-9", "--no-focus"]);
    assert_eq!(calls[1], ["pane", "run", "w:p1", "'/bin/orq' run --run-id run-9"]);
}

#[test]
fn a_grid_command_pane_runs_the_given_command_in_the_root_pane_a_split_and_a_fallback_tab() {
    let split = r#"{"result":{"pane":{"pane_id":"w:p2","tab_id":"w:t2","workspace_id":"w"},"type":"pane_created"}}"#;
    let herdr = Fake::new(vec![
        Ok(CREATED.into()),
        Ok(OK.into()),
        Ok(OK.into()),
        Ok(split.into()),
        Ok(OK.into()),
        Ok(OK.into()),
        Err("no room".into()),
        Ok(CREATED.into()),
        Ok(OK.into()),
    ]);
    let mut grid = GridTab::new("orq batch");

    open_grid_command_pane(&herdr, &mut grid, None, Path::new("/repo"), "orq run-1", "run one").unwrap();
    open_grid_command_pane(&herdr, &mut grid, None, Path::new("/repo"), "orq run-2", "run two").unwrap();
    open_grid_command_pane(&herdr, &mut grid, None, Path::new("/repo"), "orq run-3", "run three").unwrap();

    let calls = herdr.calls.borrow();
    let runs: Vec<&Vec<String>> = calls.iter().filter(|call| call[..2] == ["pane", "run"]).collect();
    assert_eq!(runs.len(), 3, "{calls:?}");
    assert_eq!(*runs[0], ["pane", "run", "w:p1", "run one"]);
    assert_eq!(*runs[1], ["pane", "run", "w:p2", "run two"]);
    assert_eq!(*runs[2], ["pane", "run", "w:p1", "run three"]);
    assert_eq!(calls[7][..2], ["tab", "create"], "the refused split falls back to a tab: {calls:?}");
}
