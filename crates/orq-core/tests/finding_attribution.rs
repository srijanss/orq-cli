use orq_core::review_input::{Finding, FindingStatus, Round, Severity, Verdict};
use orq_core::review_intelligence::{attribute, file_link, Reference, Standing};
use orq_core::review_report::ReportFile;

const SHA: &str = "1111111111111111111111111111111111111111";

fn finding(id: &str, status: FindingStatus, verdict: Option<Verdict>, file: Option<&str>) -> Finding {
    Finding { id: id.into(), severity: Severity::High, file: file.map(Into::into), line: Some(3), category: "bug".into(), claim: "c".into(), status, verdict }
}

fn changed(path: &str) -> ReportFile {
    ReportFile { path: path.into(), old_path: None, added: None, deleted: None }
}

fn round(number: usize, findings: Vec<Finding>) -> Round {
    Round { number, summary: None, findings }
}

#[test]
fn a_confirmed_verdict_or_a_fix_is_confirmed_and_everything_else_is_potential() {
    let rounds = vec![round(
        1,
        vec![
            finding("v", FindingStatus::Open, Some(Verdict::Confirmed), Some("a.rs")),
            finding("f", FindingStatus::Fixed, None, Some("a.rs")),
            finding("p", FindingStatus::Open, Some(Verdict::Plausible), Some("a.rs")),
            finding("n", FindingStatus::Open, None, Some("a.rs")),
        ],
    )];
    let out = attribute(&rounds, &[changed("a.rs")]);
    let standing = |id: &str| out.iter().find(|a| a.id == id).unwrap().standing;
    assert_eq!(standing("v"), Standing::Confirmed);
    assert_eq!(standing("f"), Standing::Confirmed);
    assert_eq!(standing("p"), Standing::Potential);
    assert_eq!(standing("n"), Standing::Potential);
}

#[test]
fn every_finding_names_the_round_and_the_evidence_it_rests_on() {
    let rounds = vec![round(1, vec![]), round(2, vec![finding("v", FindingStatus::Open, Some(Verdict::Confirmed), Some("a.rs")), finding("n", FindingStatus::Open, None, None)])];
    let out = attribute(&rounds, &[changed("a.rs")]);
    assert_eq!(out[0].round, 2);
    assert!(out[0].evidence.contains("verdict confirmed"), "{}", out[0].evidence);
    assert!(out[1].evidence.contains("no verdict"), "{}", out[1].evidence);
}

#[test]
fn rejected_findings_are_dismissed_and_open_or_deferred_ones_are_unresolved() {
    let rounds = vec![round(
        1,
        vec![
            finding("r", FindingStatus::Rejected, Some(Verdict::Confirmed), None),
            finding("o", FindingStatus::Open, None, None),
            finding("d", FindingStatus::Deferred, None, None),
            finding("u", FindingStatus::Unknown, None, None),
            finding("x", FindingStatus::Fixed, None, None),
        ],
    )];
    let out = attribute(&rounds, &[]);
    let get = |id: &str| out.iter().find(|a| a.id == id).unwrap();
    assert_eq!(get("r").standing, Standing::Dismissed);
    assert!(!get("r").unresolved && !get("x").unresolved);
    assert!(get("o").unresolved && get("d").unresolved && get("u").unresolved);
}

#[test]
fn a_reference_to_a_file_outside_the_change_is_outdated_not_trusted() {
    let rounds = vec![round(1, vec![finding("a", FindingStatus::Open, None, Some("src/gone.rs")), finding("b", FindingStatus::Open, None, Some("./src/here.rs")), finding("c", FindingStatus::Open, None, None)])];
    let out = attribute(&rounds, &[changed("src/here.rs")]);
    assert_eq!(out[0].reference, Reference::Outdated);
    assert_eq!(out[1].reference, Reference::Changed);
    assert_eq!(out[2].reference, Reference::NoFile);
}

#[test]
fn the_same_finding_in_several_rounds_is_reported_once_at_its_latest_state() {
    let rounds = vec![round(1, vec![finding("a", FindingStatus::Open, None, None)]), round(2, vec![finding("a", FindingStatus::Fixed, None, None)])];
    let out = attribute(&rounds, &[]);
    assert_eq!(out.len(), 1);
    assert_eq!((out[0].round, out[0].unresolved), (2, false));
}

#[test]
fn a_link_is_built_only_for_a_path_that_exists_and_is_percent_encoded() {
    let exists = |p: &str| p == "src/my file#1.rs" || p == "a.rs";
    let link = file_link("https://github.example/o/r", SHA, "src/my file#1.rs", Some(7), &exists).unwrap();
    assert_eq!(link, format!("https://github.example/o/r/blob/{SHA}/src/my%20file%231.rs#L7"));
    assert_eq!(file_link("https://github.example/o/r/", SHA, "a.rs", None, &exists).unwrap(), format!("https://github.example/o/r/blob/{SHA}/a.rs"));
    assert_eq!(file_link("https://github.example/o/r", SHA, "gone.rs", None, &exists), None);
}

#[test]
fn unsafe_links_are_never_built() {
    let always = |_: &str| true;
    assert_eq!(file_link("https://h/o/r", SHA, "../etc/passwd", None, &always), None);
    assert_eq!(file_link("https://h/o/r", SHA, "/abs", None, &always), None);
    assert_eq!(file_link("https://h/o/r", SHA, "a\nb", None, &always), None);
    assert_eq!(file_link("https://h/o/r", "main", "a.rs", None, &always), None);
    assert_eq!(file_link("http://h/o/r", SHA, "a.rs", None, &always), None);
    assert_eq!(file_link("javascript:alert(1)", SHA, "a.rs", None, &always), None);
    assert_eq!(file_link("https://h/o/r\u{1b}", SHA, "a.rs", None, &always), None);
}
