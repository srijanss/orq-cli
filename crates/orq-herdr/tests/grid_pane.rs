use std::cell::RefCell;
use std::path::Path;

use orq_herdr::{open_grid_pane, GridTab, HerdrCli};

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

fn split(pane: &str) -> String {
    format!(r#"{{"result":{{"pane":{{"pane_id":"{pane}","tab_id":"w:t2","workspace_id":"w"}},"type":"pane_created"}}}}"#)
}

#[test]
fn the_first_run_opens_the_shared_tab_and_the_next_ones_split_the_newest_pane_alternating_right_and_down() {
    let herdr = Fake::new(vec![
        Ok(CREATED.into()),
        Ok(OK.into()),
        Ok(OK.into()),
        Ok(split("w:p2")),
        Ok(OK.into()),
        Ok(OK.into()),
        Ok(split("w:p3")),
        Ok(OK.into()),
        Ok(OK.into()),
    ]);
    let mut grid = GridTab::new("feats: a, b, c");

    let first = open_grid_pane(&herdr, &mut grid, Some("w"), Path::new("/repo"), "orq run-1", Path::new("/l1")).unwrap();
    let second = open_grid_pane(&herdr, &mut grid, Some("w"), Path::new("/repo"), "orq run-2", Path::new("/l2")).unwrap();
    let third = open_grid_pane(&herdr, &mut grid, Some("w"), Path::new("/repo"), "orq run-3", Path::new("/l3")).unwrap();

    assert_eq!((first.pane_id.as_str(), second.pane_id.as_str(), third.pane_id.as_str()), ("w:p1", "w:p2", "w:p3"));
    assert_eq!((first.tab_id.as_str(), second.tab_id.as_str(), third.tab_id.as_str()), ("w:t2", "w:t2", "w:t2"));
    let calls = herdr.calls.borrow();
    assert_eq!(calls[0], ["tab", "create", "--workspace", "w", "--cwd", "/repo", "--label", "feats: a, b, c", "--no-focus"]);
    assert_eq!(calls[1], ["pane", "rename", "w:p1", "orq run-1"]);
    assert_eq!(calls[2][..3], ["pane", "run", "w:p1"]);
    assert_eq!(
        calls[3],
        ["pane", "split", "--pane", "w:p1", "--direction", "right", "--ratio", "0.618", "--cwd", "/repo", "--no-focus"]
    );
    assert_eq!(calls[4], ["pane", "rename", "w:p2", "orq run-2"]);
    assert_eq!(calls[5][..3], ["pane", "run", "w:p2"]);
    assert_eq!(calls[6][..6], ["pane", "split", "--pane", "w:p2", "--direction", "down"]);
}

#[test]
fn a_split_that_fails_gives_that_run_a_tab_of_its_own_and_later_runs_keep_the_grid() {
    let herdr = Fake::new(vec![
        Ok(CREATED.into()),
        Ok(OK.into()),
        Ok(OK.into()),
        Err("pane_too_small: no room".into()),
        // The fallback tab for the second run.
        Ok(r#"{"result":{"root_pane":{"pane_id":"w:p9","tab_id":"w:t9","workspace_id":"w"}}}"#.into()),
        Ok(OK.into()),
        Ok(split("w:p3")),
        Ok(OK.into()),
        Ok(OK.into()),
    ]);
    let mut grid = GridTab::new("feats");

    open_grid_pane(&herdr, &mut grid, None, Path::new("/repo"), "orq run-1", Path::new("/l1")).unwrap();
    let second = open_grid_pane(&herdr, &mut grid, None, Path::new("/repo"), "orq run-2", Path::new("/l2")).unwrap();
    let third = open_grid_pane(&herdr, &mut grid, None, Path::new("/repo"), "orq run-3", Path::new("/l3")).unwrap();

    assert_eq!(second.tab_id, "w:t9", "the failed split fell back to a tab of its own");
    assert_eq!(third.pane_id, "w:p3", "the grid went on from the last pane that is in it");
    assert_eq!(herdr.calls.borrow()[6][3], "w:p1");
}
