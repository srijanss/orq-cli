//! The design skills orq ships: installed where the agent tooling installs the implement-and-review
//! skill, and found by a Claude and a pi session started with the command each harness names it by.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use orq_agents::{check_against_schema, install_design_skills, skill_command, DESIGN_SKILLS};
use orq_test_support::TempDir;

/// A fake Claude: resolves the leading slash command of its last argument the way Claude does, from
/// the worktree's `.claude/commands/<name>.md`, follows the skill file that command names and prints it.
const FAKE_CLAUDE: &str = r#"#!/bin/sh
for last; do :; done
name=$(printf '%s' "$last" | sed -n '1s|^/\([A-Za-z0-9_-]*\).*|\1|p')
command=".claude/commands/$name.md"
[ -f "$command" ] || { echo "no command $name" >&2; exit 3; }
skill=$(sed -n 's|.*`\(\.agents/skills/[^`]*/SKILL\.md\)`.*|\1|p' "$command" | head -n 1)
[ -n "$skill" ] && [ -f "$skill" ] || { echo "no skill for $name" >&2; exit 4; }
cat "$skill"
"#;

/// A fake pi: resolves `/skill:<name>` from the worktree's `.agents/skills/<name>/SKILL.md` and prints it.
const FAKE_PI: &str = r#"#!/bin/sh
for last; do :; done
name=$(printf '%s' "$last" | sed -n '1s|^/skill:\([A-Za-z0-9_-]*\).*|\1|p')
skill=".agents/skills/$name/SKILL.md"
[ -n "$name" ] && [ -f "$skill" ] || { echo "no skill $name" >&2; exit 3; }
cat "$skill"
"#;

fn fake(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, body).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[test]
fn each_design_skill_is_installed_and_found_by_a_fake_claude_and_a_fake_pi_session() {
    let bin = TempDir::new("design-skills-bin");
    let worktree = TempDir::new("design-skills-worktree");
    install_design_skills(worktree.path()).unwrap();
    let harnesses = [
        ("claude", fake(bin.path(), "claude", FAKE_CLAUDE)),
        ("pi", fake(bin.path(), "pi", FAKE_PI)),
    ];

    assert_eq!(
        DESIGN_SKILLS,
        ["design-implement", "design-review", "design-prepare"]
    );
    for skill in DESIGN_SKILLS {
        for (harness, program) in &harnesses {
            let prompt = format!("{} COM-01", skill_command(harness, skill).unwrap());
            let output = Command::new(program)
                .args(["--model", "m", &prompt])
                .current_dir(worktree.path())
                .output()
                .unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success(),
                "{harness} {skill}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(stdout.starts_with("---\n"), "{harness} {skill}: {stdout}");
            assert!(
                stdout.contains(&format!("\nname: {skill}\n")),
                "{harness} {skill}: {stdout}"
            );
        }
    }
}

/// A review stopped by a blocker can have no open issue at all: it must never read as a pass.
#[test]
fn the_review_skill_decides_blocked_before_changes_required_before_pass() {
    let worktree = TempDir::new("design-skills-verdict");
    install_design_skills(worktree.path()).unwrap();
    let skill = fs::read_to_string(worktree.path().join(".agents/skills/design-review/SKILL.md")).unwrap();
    let rule = |needle: &str| skill.find(needle).unwrap_or_else(|| panic!("{needle:?} missing from {skill}"));
    let blocked = rule("1. `blocked` whenever a blocker");
    let changes = rule("2. `changes_required` when");
    let pass = rule("3. `pass` otherwise");
    assert!(blocked < changes && changes < pass, "{skill}");
    assert!(!skill.contains("`verdict` is `pass` with no open"), "the overlapping rule is gone: {skill}");
}

/// A person using the skill directly gets the same tests rule a frontend session's prompt carries.
#[test]
fn the_implement_skill_says_its_tests_leave_layout_and_markup_structure_to_the_design_review() {
    let skill = orq_agents::design_skill("design-implement").unwrap();
    let section = skill.split("\n## Tests\n").nth(1).unwrap_or_else(|| panic!("no \"## Tests\" section: {skill}"));
    let section = section.split("\n## ").next().unwrap();
    let dont = section.lines().find(|line| line.contains("Don't test:")).unwrap_or_else(|| panic!("no \"Don't test\" rule: {section}"));
    assert!(dont.contains("layout") && dont.contains("markup structure"), "{dont}");
    for line in orq_agents::FRONTEND_TESTS.lines() {
        assert!(section.contains(line), "the prompt's rule, word for word: {line:?} missing from {section}");
    }
}

#[test]
fn a_skill_file_hard_linked_to_a_file_outside_is_replaced_and_the_outside_file_is_untouched() {
    let outside = TempDir::new("design-skills-hardlink-outside");
    let target = outside.path().join("precious.txt");
    fs::write(&target, "keep me").unwrap();
    let worktree = TempDir::new("design-skills-hardlink");
    let commands = worktree.path().join(".claude/commands");
    let skill = worktree.path().join(".agents/skills/design-review");
    fs::create_dir_all(&commands).unwrap();
    fs::create_dir_all(&skill).unwrap();
    fs::hard_link(&target, commands.join("design-prepare.md")).unwrap();
    fs::hard_link(&target, skill.join("SKILL.md")).unwrap();

    install_design_skills(worktree.path()).unwrap();

    assert_eq!(fs::read_to_string(&target).unwrap(), "keep me");
    assert!(fs::read_to_string(skill.join("SKILL.md")).unwrap().contains("\nname: design-review\n"));
    assert!(fs::read_to_string(commands.join("design-prepare.md")).unwrap().contains(".agents/skills/design-prepare/SKILL.md"));
    let leftovers: Vec<_> = fs::read_dir(&skill).unwrap().map(|entry| entry.unwrap().file_name()).collect();
    assert_eq!(leftovers.len(), 2, "only SKILL.md and the schema, no temporary file: {leftovers:?}");
}

#[test]
fn the_skills_are_never_written_through_a_symlink_out_of_the_worktree() {
    let outside = TempDir::new("design-skills-outside");
    for link in [
        ".agents",
        ".agents/skills/design-review",
        ".claude/commands/design-prepare.md",
    ] {
        let worktree = TempDir::new("design-skills-symlink");
        let at = worktree.path().join(link);
        fs::create_dir_all(at.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(outside.path(), &at).unwrap();
        assert!(
            install_design_skills(worktree.path()).is_err(),
            "{link}: a symlink is refused"
        );
        assert_eq!(
            fs::read_dir(outside.path()).unwrap().count(),
            0,
            "{link}: nothing was written outside"
        );
    }
    // Installing again over an earlier install is fine.
    let worktree = TempDir::new("design-skills-twice");
    install_design_skills(worktree.path()).unwrap();
    install_design_skills(worktree.path()).unwrap();
}

fn valid_review() -> serde_json::Value {
    serde_json::json!({
        "run_id": "run-1",
        "reference_hash": "sha256:abc",
        "capture_revision": "0123abcd",
        "evaluation": "review",
        "verdict": "changes_required",
        "checks": [
            {"id": "viewport-desktop", "required": true, "status": "pass", "evidence": ["capture desktop.png is 1440x900"]},
            {"id": "hover-states", "required": false, "status": "not_assessed", "evidence": []}
        ],
        "issues": [{
            "id": "ISSUE-1",
            "section": "hero",
            "viewport": "mobile",
            "source": "RESPONSIVE.md",
            "expected": "the title wraps to two lines",
            "actual": "the title overflows",
            "evidence": ["measured width 412px in a 390px viewport"],
            "severity": "major",
            "status": "open",
            "fix_direction": "let the title wrap",
            "selector": "h1.hero-title",
            "basis": "rule",
            "copy": false,
            "quote": "The hero title wraps on mobile."
        }],
        "suggestions": ["tighten the footer spacing"],
        "blockers": []
    })
}

#[test]
fn the_review_schema_ships_next_to_the_review_skill_and_accepts_a_valid_review_only() {
    let worktree = TempDir::new("design-schema-worktree");
    install_design_skills(worktree.path()).unwrap();
    let text = fs::read_to_string(
        worktree
            .path()
            .join(".agents/skills/design-review/review.schema.json"),
    )
    .unwrap();
    let schema: serde_json::Value = serde_json::from_str(&text).expect("the schema parses");

    check_against_schema(&schema, &valid_review()).expect("a valid review is accepted");
    let mut no_selector = valid_review();
    no_selector["issues"][0]
        .as_object_mut()
        .unwrap()
        .remove("selector");
    check_against_schema(&schema, &no_selector).expect("an issue's selector is optional");

    let mut missing = valid_review();
    missing.as_object_mut().unwrap().remove("reference_hash");
    assert!(
        check_against_schema(&schema, &missing).is_err(),
        "a missing field is refused"
    );
    let mut missing_in_issue = valid_review();
    missing_in_issue["issues"][0]
        .as_object_mut()
        .unwrap()
        .remove("fix_direction");
    assert!(
        check_against_schema(&schema, &missing_in_issue).is_err(),
        "a missing issue field is refused"
    );

    let mut verdict = valid_review();
    verdict["verdict"] = "approved".into();
    assert!(
        check_against_schema(&schema, &verdict).is_err(),
        "an unknown verdict is refused"
    );
    let mut severity = valid_review();
    severity["issues"][0]["severity"] = "blocker".into();
    assert!(
        check_against_schema(&schema, &severity).is_err(),
        "an unknown severity is refused"
    );
    let mut extra = valid_review();
    extra["score"] = 9.into();
    assert!(
        check_against_schema(&schema, &extra).is_err(),
        "an unknown field is refused"
    );
    let mut structure = valid_review();
    let issue = structure["issues"][0].as_object_mut().unwrap();
    issue.remove("quote");
    issue.insert("basis".into(), "structure".into());
    issue.insert("reference".into(), "references/mobile.png".into());
    check_against_schema(&schema, &structure).expect("a structure issue names its reference");
    let mut token = valid_review();
    token["issues"][0]["token"] = "--color-primary".into();
    check_against_schema(&schema, &token).expect("a rule issue may name a token");
    for field in ["basis", "copy"] {
        let mut missing = valid_review();
        missing["issues"][0].as_object_mut().unwrap().remove(field);
        assert!(check_against_schema(&schema, &missing).is_err(), "an issue without {field} is refused");
    }
    let mut basis = valid_review();
    basis["issues"][0]["basis"] = "taste".into();
    assert!(check_against_schema(&schema, &basis).is_err(), "an unknown basis is refused");

    let mut wrong_type = valid_review();
    wrong_type["checks"][0]["required"] = "yes".into();
    assert!(
        check_against_schema(&schema, &wrong_type).is_err(),
        "a field of the wrong type is refused"
    );
}
