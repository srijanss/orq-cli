use orq_core::review_intelligence::{classify_files, Category, RiskLevel, MAX_CLASSIFIED_FILES};
use orq_core::review_report::ReportFile;

fn file(path: &str) -> ReportFile {
    ReportFile { path: path.to_string(), old_path: None, added: Some(1), deleted: Some(0) }
}

fn one(path: &str) -> (Category, RiskLevel, String) {
    let result = classify_files(&[file(path)]);
    let r = &result.files[0];
    (r.category, r.level, r.evidence.clone())
}

#[test]
fn sensitive_paths_are_high_risk_and_say_which_part_of_the_path_decided() {
    for path in ["src/auth/session.rs", "crates/x/src/security.rs", "lib/Login.py", "config/credentials.yml", "app/permissions.ts"] {
        let (category, level, evidence) = one(path);
        assert_eq!((category, level), (Category::Security, RiskLevel::High), "{path}");
        assert!(evidence.contains("path"), "{evidence}");
    }
    for path in ["db/migrations/001_init.sql", "prisma/schema.prisma", "schema.sql"] {
        assert_eq!(one(path).0, Category::Schema, "{path}");
        assert_eq!(one(path).1, RiskLevel::High, "{path}");
    }
}

#[test]
fn manifests_and_pipelines_are_medium_risk() {
    for path in ["Cargo.toml", "Cargo.lock", "package.json", "web/package-lock.json", "requirements.txt", "go.mod"] {
        assert_eq!((one(path).0, one(path).1), (Category::Dependencies, RiskLevel::Medium), "{path}");
    }
    for path in [".github/workflows/ci.yml", "Dockerfile", ".gitlab-ci.yml", "deploy/Makefile"] {
        assert_eq!((one(path).0, one(path).1), (Category::Build, RiskLevel::Medium), "{path}");
    }
}

#[test]
fn tests_and_docs_are_low_risk_and_code_is_medium() {
    for path in ["tests/it.rs", "src/foo_test.go", "web/app.spec.ts", "test/helper.py"] {
        assert_eq!((one(path).0, one(path).1), (Category::Tests, RiskLevel::Low), "{path}");
    }
    for path in ["README.md", "docs/guide.rst", "CHANGELOG"] {
        assert_eq!((one(path).0, one(path).1), (Category::Docs, RiskLevel::Low), "{path}");
    }
    for path in ["src/lib.rs", "app/main.py", "web/index.tsx", "cmd/run.go"] {
        assert_eq!((one(path).0, one(path).1), (Category::Code, RiskLevel::Medium), "{path}");
    }
}

#[test]
fn a_language_that_is_not_known_is_unclassified_rather_than_guessed() {
    for path in ["src/main.zig", "data.bin", "weird", "image.png"] {
        let (category, level, _) = one(path);
        assert_eq!((category, level), (Category::Unclassified, RiskLevel::Unknown), "{path}");
    }
}

#[test]
fn a_word_inside_another_word_is_not_a_match() {
    // "author" contains "auth" but is not an auth path; "latest" contains "test".
    assert_eq!(one("src/author.rs").0, Category::Code);
    assert_eq!(one("src/latest.rs").0, Category::Code);
    assert_eq!(one("src/contest/run.rs").0, Category::Code);
}

#[test]
fn a_renamed_file_is_judged_by_its_new_and_old_path() {
    let moved = ReportFile { path: "src/util.rs".into(), old_path: Some("src/auth/token.rs".into()), added: None, deleted: None };
    let result = classify_files(&[moved]);
    assert_eq!(result.files[0].category, Category::Security);
}

#[test]
fn a_very_large_diff_is_bounded_and_says_how_much_was_left_out() {
    let files: Vec<ReportFile> = (0..MAX_CLASSIFIED_FILES + 40).map(|i| file(&format!("src/f{i}.rs"))).collect();
    let result = classify_files(&files);
    assert_eq!(result.files.len(), MAX_CLASSIFIED_FILES);
    assert_eq!(result.omitted, 40);
}

#[test]
fn the_riskiest_files_survive_the_bound() {
    let mut files: Vec<ReportFile> = (0..MAX_CLASSIFIED_FILES + 5).map(|i| file(&format!("docs/d{i}.md"))).collect();
    files.push(file("src/auth/login.rs"));
    let result = classify_files(&files);
    assert_eq!(result.files[0].path, "src/auth/login.rs");
    assert_eq!(result.files.len(), MAX_CLASSIFIED_FILES);
}

#[test]
fn paths_are_carried_as_single_lines() {
    let result = classify_files(&[file("src/a\u{1b}[2Jb.rs")]);
    assert!(!result.files[0].path.chars().any(char::is_control));
}
