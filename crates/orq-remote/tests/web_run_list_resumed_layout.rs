//! A resumed run's row in the Orchestrations list keeps the list's shape, measured on the built page in a
//! real headless Chromium: on a phone its "continued as <run>" note sits on its own line under the row's
//! first line instead of widening the state column until the feature name is one letter wide, and no
//! state note, however long and unbroken, pushes anything past the row or the page.

mod web_chromium;

use serde_json::{json, Value};
use std::path::Path;
use web_chromium::{get, measure, Reply};

const URL: &str = "/projects/demo/orchestrations";
const ROWS: &str = "tr.run-row";
const NAMES: &str = "tr.run-row td:first-child";
const NOTES: &str = "tr.run-row [data-run-ending]";
const CONTINUED: &str = "run-1794918550-3515734-1";

fn row(id: &str, feature: &str, state: &str, resumed_as: Option<&str>) -> Value {
    let mut run = json!({"run_id": id, "feature": feature, "state": state, "stage": "implement", "started_at": "2026-11-18T10:00:00Z", "updated_at": "2026-11-18T11:00:00Z", "target_branch": "main"});
    if let Some(next) = resumed_as {
        run["resumed_as"] = json!(next);
    }
    run
}

fn replies(resumed_as: &str) -> Vec<Reply> {
    let project = json!({"id": "demo", "name": "demo", "available": true, "features": 2, "active_runs": 0, "max_active_runs": null, "start_blocked": null, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24});
    let runs = [row("run-1794917000-3400000-1", "rust-gate-runs-tests-with-nextest", "completed", None), row("run-1794917237-3409773-1", "rust-gate-runs-tests-with-nextest", "resumed", Some(resumed_as))];
    vec![get("/api/projects", json!({"projects": [project]})), get("/api/projects/demo/runs*", json!({"runs": runs, "next_cursor": null, "total": 2, "running": 0, "remote_actions": false}))]
}

#[derive(Clone, Copy, Debug)]
struct Rect {
    left: i64,
    top: i64,
    right: i64,
    bottom: i64,
}

impl Rect {
    fn width(self) -> i64 {
        self.right - self.left
    }
    fn height(self) -> i64 {
        self.bottom - self.top
    }
}

fn rects(result: &Value, selector: &str) -> Vec<Rect> {
    result["boxes"][selector]
        .as_array()
        .unwrap_or_else(|| panic!("no boxes for {selector}: {result}"))
        .iter()
        .map(|b| Rect { left: b["left"].as_i64().unwrap(), top: b["top"].as_i64().unwrap(), right: b["right"].as_i64().unwrap(), bottom: b["bottom"].as_i64().unwrap() })
        .collect()
}

/// The list at `width`, with the completed row first and the resumed one second, after checking that the
/// page does not scroll sideways and no text ends past the screen.
fn load(chromium: &Path, width: u32, resumed_as: &str) -> Value {
    let steps = json!([{"wait": NOTES}, {"boxes": ROWS}, {"boxes": NAMES}, {"boxes": NOTES}, {"boxes": "tr.run-row *"}]);
    let out = measure(chromium, width, URL, &replies(resumed_as), steps);
    let result = out.result;
    assert!(result.get("error").is_none(), "{width} px: {result}\nunanswered: {:?}", out.unanswered);
    assert!(result["scrollWidth"].as_u64() <= result["clientWidth"].as_u64(), "{width} px: the page scrolls sideways: {result}");
    assert_eq!(result["offenders"], json!([]), "{width} px: text runs past the screen: {result}");
    let rows = rects(&result, ROWS);
    assert_eq!(rows.len(), 2, "{width} px: {result}");
    for each in rects(&result, "tr.run-row *") {
        assert!(each.right <= i64::from(width), "{width} px: {each:?} is wider than the page");
    }
    result
}

fn assert_fits_a_phone(chromium: &Path, resumed_as: &str) {
    for width in [360, 390] {
        let result = load(chromium, width, resumed_as);
        let (rows, names, notes) = (rects(&result, ROWS), rects(&result, NAMES), rects(&result, NOTES));
        assert!(names[1].width() >= names[0].width(), "{width} px: the resumed row's name cell {:?} is as wide as the completed row's {:?}", names[1], names[0]);
        assert!(rows[1].height() < 3 * rows[0].height(), "{width} px: the resumed row {:?} is under 3x the completed row {:?}", rows[1], rows[0]);
        assert!(notes[0].top >= names[1].bottom, "{width} px: the note {:?} is on its own line under the row's first line {:?}", notes[0], names[1]);
        assert!(notes[0].left >= rows[1].left && notes[0].right <= rows[1].right, "{width} px: the note {:?} stays inside its row {:?}", notes[0], rows[1]);
    }
}

#[test]
fn on_a_phone_a_resumed_runs_row_keeps_its_name_column_and_puts_its_note_under_it() {
    let Some(chromium) = web_chromium::binary() else { return };
    assert_fits_a_phone(&chromium, CONTINUED);
}

#[test]
fn on_a_phone_an_unbroken_200_character_note_still_fits_its_row() {
    let Some(chromium) = web_chromium::binary() else { return };
    assert_fits_a_phone(&chromium, &"r".repeat(200));
}

#[test]
fn on_a_desktop_a_resumed_runs_row_is_within_a_line_of_a_completed_one() {
    let Some(chromium) = web_chromium::binary() else { return };
    let result = load(&chromium, 1280, CONTINUED);
    let rows = rects(&result, ROWS);
    assert!(rows[1].height() <= rows[0].height() + 24, "a resumed row {:?} is within a line of a completed row {:?}", rows[1], rows[0]);
}

#[test]
fn on_a_desktop_an_unbroken_200_character_note_wraps_in_its_row() {
    let Some(chromium) = web_chromium::binary() else { return };
    let result = load(&chromium, 1280, &"r".repeat(200));
    let (rows, notes) = (rects(&result, ROWS), rects(&result, NOTES));
    assert!(notes[0].right <= rows[1].right, "the note {:?} stays inside its row {:?}", notes[0], rows[1]);
}
