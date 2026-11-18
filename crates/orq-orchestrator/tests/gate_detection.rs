use std::fs;
use std::path::{Path, PathBuf};

use orq_orchestrator::{
    choose_gate, choose_gate_scoped, choose_gate_with, Gate, GateError, GateOptions,
};

fn worktree(tag: &str) -> PathBuf {
    let dir =
        orq_test_support::process_temp_dir().join(format!("orq-gate-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn touch(dir: &Path, name: &str, text: &str) {
    let path = dir.join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn steps(list: &[&[&str]]) -> Vec<Vec<String>> {
    list.iter()
        .map(|argv| argv.iter().map(|word| word.to_string()).collect())
        .collect()
}

fn run(list: &[&[&str]]) -> Gate {
    Gate::Steps {
        steps: steps(list),
        notes: vec![],
    }
}

/// The gate detected in `dir`, with cargo-nextest there or not.
fn detected(dir: &Path, nextest: bool) -> Result<Gate, GateError> {
    choose_gate_with(
        dir,
        None,
        &GateOptions {
            nextest: Some(nextest),
            ..GateOptions::default()
        },
    )
}

const NEXTEST: &[&str] = &["cargo", "nextest", "run", "--workspace", "--no-fail-fast", "--no-tests=pass"];

const PACKAGE: &str =
    r#"{"scripts": {"typecheck": "tsc --noEmit", "test": "vitest run", "lint": "eslint ."}}"#;

#[test]
fn a_configured_gate_wins_over_every_detected_stack() {
    let dir = worktree("configured");
    touch(&dir, "Cargo.toml", "[workspace]\n");
    touch(&dir, "package.json", PACKAGE);

    let configured = [
        "make".to_string(),
        "check".to_string(),
        "--jobs=4".to_string(),
    ];
    assert_eq!(
        choose_gate(&dir, Some(&configured)),
        Ok(run(&[&["make", "check", "--jobs=4"]]))
    );
    // An empty configured list is the same as none, so detection still runs.
    assert_eq!(choose_gate(&dir, Some(&[])), choose_gate(&dir, None));
    assert!(matches!(choose_gate(&dir, None), Ok(Gate::Steps { .. })));
}

#[test]
fn a_configured_gate_needs_a_program_to_run() {
    let dir = worktree("empty-program");
    for bad in [
        vec!["".to_string()],
        vec!["  ".to_string(), "x".to_string()],
    ] {
        assert_eq!(choose_gate(&dir, Some(&bad)), Err(GateError::EmptyProgram));
    }
}

#[test]
fn without_nextest_a_rust_workspace_runs_cargo_test_and_says_why() {
    let dir = worktree("rust");
    touch(&dir, "Cargo.toml", "[workspace]\n");
    assert_eq!(
        detected(&dir, false),
        Ok(Gate::Steps {
            steps: steps(&[&["cargo", "test", "--workspace"]]),
            notes: vec!["cargo-nextest not found: running cargo test".to_string()],
        })
    );
}

#[test]
fn a_rust_workspace_runs_nextest_when_it_is_there() {
    let dir = worktree("rust-nextest");
    touch(&dir, "Cargo.toml", "[workspace]\n");
    assert_eq!(detected(&dir, true), Ok(run(&[NEXTEST])));
}

#[test]
fn a_rust_workspace_with_no_tests_passes_under_nextest_as_it_does_without_it() {
    let dir = worktree("rust-no-tests");
    touch(&dir, "Cargo.toml", "[workspace]\n");
    let Ok(Gate::Steps { steps, .. }) = detected(&dir, true) else { panic!("a Rust workspace has steps") };
    // nextest exits non-zero when it finds no test where the plain runner passes; the gate must pass too.
    assert!(steps[0].iter().any(|word| word == "--no-tests=pass"), "{steps:?}");
}

#[test]
fn left_unset_nextest_is_looked_for_by_asking_cargo_in_the_worktree() {
    let dir = worktree("rust-probe");
    touch(&dir, "Cargo.toml", "[workspace]\n");
    // Whatever this host has, the probe settles on one of the two whole gates, never a mix; its success,
    // failure and time limit are covered by the bounded runner's own tests in gate.rs.
    let probed = choose_gate(&dir, None);
    assert!(probed == detected(&dir, true) || probed == detected(&dir, false), "{probed:?}");
}

const DOC: &[&str] = &["cargo", "test", "--workspace", "--doc"];

fn package(dir: &Path, name: &str, extra: &str) {
    touch(dir, &format!("crates/{name}/Cargo.toml"), &format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n{extra}"));
}

#[test]
fn a_library_cargo_would_doc_test_adds_the_doc_test_step_since_nextest_runs_none() {
    let dir = worktree("rust-doc");
    touch(&dir, "Cargo.toml", "[workspace]\nmembers = [\"crates/*\"]\nresolver = \"2\"\n");
    // A binary is never doc-tested, nor a library that turns doc tests off; target/ is not a member.
    package(&dir, "tool", "");
    touch(&dir, "crates/tool/src/main.rs", "/// ```\n/// assert!(true);\n/// ```\nfn main() {}\n");
    package(&dir, "quiet", "[lib]\ndoctest = false\n");
    touch(&dir, "crates/quiet/src/lib.rs", "/// ```\n/// assert!(true);\n/// ```\npub fn f() {}\n");
    touch(&dir, "target/package/x/Cargo.toml", "[package]\nname = \"x\"\nversion = \"0.1.0\"\n");
    touch(&dir, "target/package/x/src/lib.rs", "pub fn f() {}\n");
    assert_eq!(detected(&dir, true), Ok(run(&[NEXTEST])), "nothing Cargo would doc-test");

    // Cargo doc-tests a library wherever its [lib] path points, whatever its doc comments look like.
    package(&dir, "api", "[lib]\npath = \"src/api.rs\"\n");
    touch(&dir, "crates/api/src/api.rs", "/** Block docs.\n\n    let indented = 1;\n*/\npub fn f() {}\n");
    assert_eq!(detected(&dir, true), Ok(run(&[NEXTEST, DOC])));
    assert_eq!(
        detected(&dir, false),
        Ok(Gate::Steps {
            steps: steps(&[&["cargo", "test", "--workspace"]]),
            notes: vec!["cargo-nextest not found: running cargo test".to_string()],
        }),
        "cargo test runs the doc tests itself"
    );
}

#[test]
fn a_workspace_cargo_cannot_describe_keeps_the_doc_test_step_rather_than_drop_its_doc_tests() {
    let dir = worktree("rust-doc-unreadable");
    touch(&dir, "Cargo.toml", "[workspace\nnot toml\n");
    assert_eq!(detected(&dir, true), Ok(run(&[NEXTEST, DOC])));
}

#[test]
fn node_runs_typecheck_then_test_with_the_package_manager_the_lockfile_names() {
    for (lockfile, pm) in [
        ("pnpm-lock.yaml", "pnpm"),
        ("yarn.lock", "yarn"),
        ("bun.lockb", "bun"),
        ("bun.lock", "bun"),
        ("", "npm"),
    ] {
        let dir = worktree(&format!("node-{pm}-{}", lockfile.replace('.', "_")));
        touch(&dir, "package.json", PACKAGE);
        fs::create_dir_all(dir.join("node_modules")).unwrap();
        if !lockfile.is_empty() {
            touch(&dir, lockfile, "");
        }
        assert_eq!(
            choose_gate(&dir, None),
            Ok(run(&[&[pm, "run", "typecheck"], &[pm, "run", "test"]])),
            "{lockfile:?}"
        );
    }
    // pnpm outranks yarn, which outranks bun, when several lockfiles exist.
    let dir = worktree("node-lockfile-priority");
    touch(&dir, "package.json", PACKAGE);
    fs::create_dir_all(dir.join("node_modules")).unwrap();
    for lockfile in ["bun.lockb", "yarn.lock", "pnpm-lock.yaml"] {
        touch(&dir, lockfile, "");
    }
    assert_eq!(
        choose_gate(&dir, None),
        Ok(run(&[
            &["pnpm", "run", "typecheck"],
            &["pnpm", "run", "test"]
        ]))
    );
}

#[test]
fn node_typecheck_falls_back_to_type_check_then_to_tsc() {
    let dir = worktree("node-type-check");
    touch(
        &dir,
        "package.json",
        r#"{"scripts": {"type-check": "tsc", "test": "jest"}}"#,
    );
    fs::create_dir_all(dir.join("node_modules")).unwrap();
    assert_eq!(
        choose_gate(&dir, None),
        Ok(run(&[
            &["npm", "run", "type-check"],
            &["npm", "run", "test"]
        ]))
    );

    let dir = worktree("node-both-typechecks");
    touch(
        &dir,
        "package.json",
        r#"{"scripts": {"type-check": "a", "typecheck": "b", "test": "jest"}}"#,
    );
    fs::create_dir_all(dir.join("node_modules")).unwrap();
    assert_eq!(
        choose_gate(&dir, None),
        Ok(run(&[
            &["npm", "run", "typecheck"],
            &["npm", "run", "test"]
        ]))
    );

    let dir = worktree("node-tsc");
    touch(&dir, "package.json", r#"{"scripts": {"test": "jest"}}"#);
    touch(&dir, "tsconfig.json", "{}");
    touch(&dir, "node_modules/.bin/tsc", "");
    assert_eq!(
        choose_gate(&dir, None),
        Ok(run(&[
            &["node_modules/.bin/tsc", "--noEmit"],
            &["npm", "run", "test"]
        ]))
    );
}

#[test]
fn npms_placeholder_test_script_is_not_a_test() {
    let dir = worktree("node-placeholder");
    touch(
        &dir,
        "package.json",
        r#"{"scripts": {"test": "echo \"Error: no test specified\" && exit 1"}}"#,
    );
    fs::create_dir_all(dir.join("node_modules")).unwrap();
    assert!(matches!(choose_gate(&dir, None), Ok(Gate::Skipped { .. })));

    // Surrounding whitespace does not make it a different script.
    let dir = worktree("node-placeholder-padded");
    touch(
        &dir,
        "package.json",
        r#"{"scripts": {"test": "  echo \"Error: no test specified\" && exit 1\n"}}"#,
    );
    fs::create_dir_all(dir.join("node_modules")).unwrap();
    assert!(matches!(choose_gate(&dir, None), Ok(Gate::Skipped { .. })));
}

#[test]
fn node_without_installed_dependencies_is_skipped_with_a_recorded_note() {
    let dir = worktree("node-no-modules");
    touch(&dir, "package.json", PACKAGE);
    touch(&dir, "Cargo.toml", "[workspace]\n");

    assert_eq!(
        detected(&dir, true),
        Ok(Gate::Steps {
            steps: steps(&[NEXTEST]),
            notes: vec![
                "package.json found but node_modules is missing; install the dependencies to run the Node checks"
                    .to_string()
            ],
        })
    );
    // Yarn's plug-and-play has no node_modules but does have .pnp.cjs.
    touch(&dir, ".pnp.cjs", "");
    touch(&dir, "yarn.lock", "");
    assert_eq!(
        detected(&dir, true),
        Ok(run(&[
            &["yarn", "run", "typecheck"],
            &["yarn", "run", "test"],
            NEXTEST
        ]))
    );
}

#[test]
fn a_malformed_package_json_is_a_note_not_a_failure() {
    let dir = worktree("node-malformed");
    touch(&dir, "package.json", "{ not json");
    touch(&dir, "Cargo.toml", "[workspace]\n");
    fs::create_dir_all(dir.join("node_modules")).unwrap();

    assert_eq!(
        detected(&dir, true),
        Ok(Gate::Steps {
            steps: steps(&[NEXTEST]),
            notes: vec!["package.json is not valid JSON; the Node checks were skipped".to_string()],
        })
    );
}

#[test]
fn python_with_uv_runs_pytest_only_when_pytest_is_configured() {
    for (tag, files, expected) in [
        (
            "uv-pytest-ini",
            vec![("uv.lock", ""), ("pytest.ini", "")],
            true,
        ),
        (
            "uv-tests-dir",
            vec![("uv.lock", ""), ("tests/test_a.py", "")],
            true,
        ),
        (
            "uv-pyproject",
            vec![
                ("uv.lock", ""),
                ("pyproject.toml", "[tool.pytest.ini_options]\n"),
            ],
            true,
        ),
        (
            "uv-bare",
            vec![
                ("uv.lock", ""),
                ("pyproject.toml", "[project]\nname = \"x\"\n"),
            ],
            false,
        ),
        (
            "pytest-without-uv",
            vec![("pytest.ini", ""), ("tests/test_a.py", "")],
            false,
        ),
    ] {
        let dir = worktree(tag);
        for (name, text) in files {
            touch(&dir, name, text);
        }
        let gate = choose_gate(&dir, None).unwrap();
        if expected {
            assert_eq!(gate, run(&[&["uv", "run", "pytest"]]), "{tag}");
        } else {
            assert!(matches!(gate, Gate::Skipped { .. }), "{tag}: {gate:?}");
        }
    }
}

const MIGRATIONS: &[&str] = &[
    "uv",
    "run",
    "python",
    "src/manage.py",
    "makemigrations",
    "--check",
    "--dry-run",
];

fn django(tag: &str) -> PathBuf {
    let dir = worktree(tag);
    touch(&dir, "uv.lock", "");
    touch(&dir, "src/manage.py", "");
    touch(&dir, "pytest.ini", "");
    touch(&dir, "src/shop/tests/test_views.py", "");
    touch(&dir, "src/blog/models.py", "");
    dir
}

#[test]
fn a_django_project_checks_migrations_and_then_runs_pytest() {
    let dir = django("django");
    touch(&dir, "Cargo.toml", "[workspace]\n");

    assert_eq!(
        detected(&dir, true),
        Ok(run(&[
            MIGRATIONS,
            &["uv", "run", "pytest"],
            NEXTEST
        ]))
    );
}

#[test]
fn a_changed_scope_runs_pytest_only_for_the_changed_tested_apps() {
    let dir = django("django-scoped");
    touch(&dir, "src/billing/tests/test_invoices.py", "");
    let changed = [
        "src/shop/views.py".to_string(),
        "src/shop/urls.py".to_string(),
        "src/billing/models.py".to_string(),
        "README.md".to_string(),
    ];

    assert_eq!(
        choose_gate_scoped(&dir, None, Some(&changed)),
        Ok(run(&[
            MIGRATIONS,
            &["uv", "run", "pytest", "src/billing", "src/shop"]
        ]))
    );
}

#[test]
fn a_changed_scope_of_only_documentation_still_checks_migrations_but_runs_no_tests() {
    let dir = django("django-scoped-docs");
    let changed = ["README.md".to_string(), "docs/setup.rst".to_string()];

    assert_eq!(
        choose_gate_scoped(&dir, None, Some(&changed)),
        Ok(run(&[MIGRATIONS]))
    );
}

#[test]
fn a_change_that_cannot_be_pinned_to_a_tested_app_runs_the_whole_suite() {
    let full = run(&[MIGRATIONS, &["uv", "run", "pytest"]]);
    for (tag, changed) in [
        ("untested-app", vec!["src/blog/models.py"]),
        ("dependencies", vec!["uv.lock"]),
        ("settings", vec!["src/config/settings.py"]),
        ("root-tests", vec!["tests/test_smoke.py"]),
        ("mixed", vec!["src/shop/views.py", "pyproject.toml"]),
        ("requirements", vec!["requirements.txt"]),
        (
            "requirements-with-app",
            vec!["src/shop/views.py", "constraints.txt"],
        ),
    ] {
        let dir = django(&format!("django-wide-{tag}"));
        let changed: Vec<String> = changed.into_iter().map(String::from).collect();
        assert_eq!(
            choose_gate_scoped(&dir, None, Some(&changed)),
            Ok(full.clone()),
            "{tag}"
        );
    }
}

#[test]
fn stacks_run_in_the_order_python_node_rust() {
    let dir = worktree("order");
    touch(&dir, "Cargo.toml", "[workspace]\n");
    touch(&dir, "package.json", r#"{"scripts": {"test": "jest"}}"#);
    fs::create_dir_all(dir.join("node_modules")).unwrap();
    touch(&dir, "uv.lock", "");
    touch(&dir, "pytest.ini", "");

    assert_eq!(
        detected(&dir, true),
        Ok(run(&[
            &["uv", "run", "pytest"],
            &["npm", "run", "test"],
            NEXTEST
        ]))
    );
}

#[test]
fn with_nothing_to_run_the_gate_is_skipped_and_says_why() {
    let dir = worktree("nothing");
    assert_eq!(
        choose_gate(&dir, None),
        Ok(Gate::Skipped {
            reason: "no local gate detected: no Cargo.toml, package.json test scripts or uv.lock with pytest, and no gate command configured"
                .to_string()
        })
    );

    // When stacks were noted but none produced a step, the notes are the reason.
    let dir = worktree("nothing-noted");
    touch(&dir, "package.json", PACKAGE);
    assert_eq!(
        choose_gate(&dir, None),
        Ok(Gate::Skipped {
            reason: "package.json found but node_modules is missing; install the dependencies to run the Node checks"
                .to_string()
        })
    );
}

#[test]
fn a_directory_that_does_not_exist_is_just_nothing_detected() {
    let missing = orq_test_support::process_temp_dir()
        .join(format!("orq-gate-{}-absent", std::process::id()));
    assert!(matches!(
        choose_gate(&missing, None),
        Ok(Gate::Skipped { .. })
    ));
}

#[test]
fn only_the_test_script_can_be_the_npm_placeholder_and_only_when_it_is_exactly_that() {
    // A typecheck script that happens to print the phrase is still a check.
    let dir = worktree("node-phrase-in-typecheck");
    touch(
        &dir,
        "package.json",
        r#"{"scripts": {"typecheck": "echo no test specified && tsc --noEmit", "test": "jest"}}"#,
    );
    fs::create_dir_all(dir.join("node_modules")).unwrap();
    assert_eq!(
        choose_gate(&dir, None),
        Ok(run(&[
            &["npm", "run", "typecheck"],
            &["npm", "run", "test"]
        ]))
    );

    // A real test command that merely mentions the phrase still runs.
    let dir = worktree("node-phrase-in-test");
    touch(
        &dir,
        "package.json",
        r#"{"scripts": {"test": "jest --passWithNoTests || echo no test specified"}}"#,
    );
    fs::create_dir_all(dir.join("node_modules")).unwrap();
    assert_eq!(choose_gate(&dir, None), Ok(run(&[&["npm", "run", "test"]])));

    // Starting with echo, mentioning the phrase and ending in exit 1 is not
    // enough: only npm's own placeholder is skipped.
    for (tag, script) in [
        (
            "fallback",
            r#"echo \"no test specified fallback\"; jest || exit 1"#,
        ),
        (
            "chained",
            r#"echo \"Error: no test specified\" && jest && exit 1"#,
        ),
    ] {
        let dir = worktree(&format!("node-lookalike-{tag}"));
        touch(
            &dir,
            "package.json",
            &format!(r#"{{"scripts": {{"test": "{script}"}}}}"#),
        );
        fs::create_dir_all(dir.join("node_modules")).unwrap();
        assert_eq!(
            choose_gate(&dir, None),
            Ok(run(&[&["npm", "run", "test"]])),
            "{tag}"
        );
    }
}

#[test]
fn pytest_is_configured_by_the_real_tool_pytest_table_not_by_a_line_prefix() {
    for (tag, pyproject, expected) in [
        (
            "indented",
            "  [tool.pytest.ini_options]\n  addopts = \"-q\"\n",
            true,
        ),
        (
            "quoted",
            "[tool.\"pytest\".ini_options]\naddopts = \"-q\"\n",
            true,
        ),
        (
            "dotted",
            "[tool]\npytest.ini_options.addopts = \"-q\"\n",
            true,
        ),
        (
            "inline",
            "[tool]\npytest = { ini_options = { addopts = \"-q\" } }\n",
            true,
        ),
        ("lookalike", "[tool.pytest_extra]\nx = 1\n", false),
        (
            "in-a-string",
            "[project]\ndescription = \"\"\"\n[tool.pytest.ini_options]\n\"\"\"\n",
            false,
        ),
    ] {
        let dir = worktree(&format!("pytest-toml-{tag}"));
        touch(&dir, "uv.lock", "");
        touch(&dir, "pyproject.toml", pyproject);
        let gate = choose_gate(&dir, None).unwrap();
        if expected {
            assert_eq!(gate, run(&[&["uv", "run", "pytest"]]), "{tag}");
        } else {
            assert!(matches!(gate, Gate::Skipped { .. }), "{tag}: {gate:?}");
        }
    }
}

#[test]
fn manifests_that_cannot_be_read_are_reported_as_such_not_as_syntax_errors_or_absence() {
    // package.json that is not UTF-8 is a read problem, not "not valid JSON".
    let dir = worktree("node-not-utf8");
    fs::write(dir.join("package.json"), [0xff, 0xfe, 0x7b]).unwrap();
    fs::create_dir_all(dir.join("node_modules")).unwrap();
    assert_eq!(
        choose_gate(&dir, None),
        Ok(Gate::Skipped {
            reason: "package.json could not be read; the Node checks were skipped".to_string()
        })
    );

    // pyproject.toml that is not UTF-8 or not TOML is reported, and the other
    // signals (here a tests directory) still count.
    for (tag, bytes) in [
        ("not-utf8", vec![0xffu8, 0xfe]),
        ("not-toml", b"[tool\nx".to_vec()),
    ] {
        let dir = worktree(&format!("pyproject-{tag}"));
        touch(&dir, "uv.lock", "");
        fs::write(dir.join("pyproject.toml"), bytes).unwrap();
        let reason = "pyproject.toml could not be read as TOML; settings in it were ignored";
        assert_eq!(
            choose_gate(&dir, None),
            Ok(Gate::Skipped {
                reason: reason.to_string()
            }),
            "{tag}"
        );
        touch(&dir, "tests/test_a.py", "");
        assert_eq!(
            choose_gate(&dir, None),
            Ok(Gate::Steps {
                steps: steps(&[&["uv", "run", "pytest"]]),
                notes: vec![reason.to_string()],
            }),
            "{tag}"
        );
    }
}

#[test]
fn a_changed_app_whose_tests_live_in_tests_py_gets_that_file_named_because_pytest_would_not_collect_it(
) {
    let dir = django("django-tests-py");
    touch(&dir, "src/billing/tests.py", "");
    let changed = [
        "src/billing/models.py".to_string(),
        "src/shop/views.py".to_string(),
    ];

    assert_eq!(
        choose_gate_scoped(&dir, None, Some(&changed)),
        Ok(run(&[
            MIGRATIONS,
            &["uv", "run", "pytest", "src/billing/tests.py", "src/shop"]
        ]))
    );
}

#[test]
fn a_scope_changes_neither_a_configured_gate_nor_a_project_that_is_not_django() {
    let dir = django("django-scope-ignored");
    let configured = ["make".to_string(), "check".to_string()];
    let changed = ["src/shop/views.py".to_string()];
    assert_eq!(
        choose_gate_scoped(&dir, Some(&configured), Some(&changed)),
        Ok(run(&[&["make", "check"]]))
    );

    let plain = worktree("plain-scope");
    touch(&plain, "uv.lock", "");
    touch(&plain, "pytest.ini", "");
    assert_eq!(
        choose_gate_scoped(&plain, None, Some(&changed)),
        Ok(run(&[&["uv", "run", "pytest"]]))
    );
}

#[test]
fn changed_files_listed_in_any_order_and_repeatedly_name_each_app_once_in_sorted_order() {
    let dir = django("django-order");
    touch(&dir, "src/billing/tests/test_a.py", "");
    let changed: Vec<String> = [
        "src/shop/b.py",
        "src/billing/a.py",
        "src/shop/a.py",
        "src/billing/b.py",
    ]
    .iter()
    .map(|p| p.to_string())
    .collect();

    assert_eq!(
        choose_gate_scoped(&dir, None, Some(&changed)),
        Ok(run(&[
            MIGRATIONS,
            &["uv", "run", "pytest", "src/billing", "src/shop"]
        ]))
    );
}

fn with(options: &GateOptions, dir: &Path) -> Gate {
    choose_gate_with(dir, None, options).unwrap()
}

#[test]
fn lint_adds_ruff_on_the_changed_python_files_before_pytest() {
    let dir = worktree("lint-python");
    touch(&dir, "uv.lock", "");
    touch(&dir, "pytest.ini", "");
    for file in ["src/a.py", "b.py", "--output-file=victim.py"] {
        touch(&dir, file, "");
    }
    // `gone.py` was deleted or renamed away by the run: ruff would fail on a path that no longer exists.
    let changed = [
        "src/a.py".to_string(),
        "README.md".to_string(),
        "gone.py".to_string(),
        "b.py".to_string(),
        "--output-file=victim.py".to_string(),
    ];

    let linted = GateOptions {
        lint: true,
        changed: Some(&changed),
        ..GateOptions::default()
    };
    assert_eq!(
        with(&linted, &dir),
        run(&[
            &[
                "uv",
                "run",
                "ruff",
                "check",
                "--",
                "--output-file=victim.py",
                "b.py",
                "src/a.py"
            ],
            &["uv", "run", "pytest"]
        ])
    );

    let docs_only = ["README.md".to_string()];
    let linted = GateOptions {
        lint: true,
        changed: Some(&docs_only),
        ..GateOptions::default()
    };
    assert_eq!(with(&linted, &dir), run(&[&["uv", "run", "pytest"]]));

    let only_deleted = ["gone.py".to_string()];
    let linted = GateOptions {
        lint: true,
        changed: Some(&only_deleted),
        ..GateOptions::default()
    };
    assert_eq!(
        with(&linted, &dir),
        run(&[&["uv", "run", "pytest"]]),
        "nothing left to lint"
    );

    let whole = GateOptions {
        lint: true,
        ..GateOptions::default()
    };
    assert_eq!(
        with(&whole, &dir),
        run(&[&["uv", "run", "ruff", "check"], &["uv", "run", "pytest"]])
    );

    // A change to ruff's own configuration can affect every file, so the whole project is checked.
    for config in ["ruff.toml", ".ruff.toml", "pyproject.toml", "sub/ruff.toml"] {
        let changed = [config.to_string(), "b.py".to_string()];
        let linted = GateOptions {
            lint: true,
            changed: Some(&changed),
            ..GateOptions::default()
        };
        assert_eq!(
            with(&linted, &dir),
            run(&[&["uv", "run", "ruff", "check"], &["uv", "run", "pytest"]]),
            "{config}"
        );
    }
}

#[test]
fn lint_and_build_add_the_package_scripts_around_the_tests() {
    let dir = worktree("lint-node");
    touch(
        &dir,
        "package.json",
        r#"{"scripts": {"typecheck": "tsc", "lint": "eslint .", "test": "vitest run", "build": "vite build"}}"#,
    );
    fs::create_dir_all(dir.join("node_modules")).unwrap();

    let both = GateOptions {
        lint: true,
        build: true,
        ..GateOptions::default()
    };
    assert_eq!(
        with(&both, &dir),
        run(&[
            &["npm", "run", "typecheck"],
            &["npm", "run", "lint"],
            &["npm", "run", "test"],
            &["npm", "run", "build"]
        ])
    );
    let off = GateOptions::default();
    assert_eq!(
        with(&off, &dir),
        run(&[&["npm", "run", "typecheck"], &["npm", "run", "test"]])
    );

    touch(
        &dir,
        "package.json",
        r#"{"scripts": {"test": "vitest run"}}"#,
    );
    assert_eq!(
        with(&both, &dir),
        run(&[&["npm", "run", "test"]]),
        "scripts that do not exist are not run"
    );
}

#[test]
fn lint_adds_cargo_fmt_check_and_clippy_before_the_tests() {
    let dir = worktree("lint-rust");
    touch(&dir, "Cargo.toml", "[workspace]\n");

    let linted = GateOptions {
        lint: true,
        build: true,
        nextest: Some(true),
        ..GateOptions::default()
    };
    assert_eq!(
        with(&linted, &dir),
        run(&[
            &["cargo", "fmt", "--check"],
            &[
                "cargo",
                "clippy",
                "--workspace",
                "--all-targets",
                "--",
                "-D",
                "warnings"
            ],
            NEXTEST
        ])
    );
}

#[test]
fn a_configured_gate_ignores_the_lint_and_build_options() {
    let dir = worktree("lint-configured");
    touch(&dir, "Cargo.toml", "[workspace]\n");
    let configured = ["make".to_string(), "check".to_string()];
    let both = GateOptions {
        lint: true,
        build: true,
        ..GateOptions::default()
    };

    assert_eq!(
        choose_gate_with(&dir, Some(&configured), &both),
        Ok(run(&[&["make", "check"]]))
    );
}

const PYTEST_IN_SRC: &[&str] = &["uv", "run", "--directory", "src", "pytest"];

/// A Django project whose pytest settings sit beside manage.py, as `src/pytest.ini` does: they make
/// the project's root, so pytest runs there.
fn django_configured_in_src(tag: &str) -> PathBuf {
    let dir = worktree(tag);
    touch(&dir, "uv.lock", "");
    touch(&dir, "src/manage.py", "");
    touch(&dir, "src/shop/tests/test_views.py", "");
    touch(&dir, "src/blog/models.py", "");
    dir
}

#[test]
fn pytest_settings_beside_manage_py_run_pytest_from_src_after_the_migrations_check() {
    let dir = django_configured_in_src("django-src-ini");
    touch(&dir, "src/pytest.ini", "[pytest]\nDJANGO_SETTINGS_MODULE = config.settings\n");

    assert_eq!(detected(&dir, true), Ok(run(&[MIGRATIONS, PYTEST_IN_SRC])));
}

#[test]
fn setup_cfg_tox_ini_or_a_conftest_beside_manage_py_are_pytest_settings_too() {
    for (tag, name, text) in [
        ("setup-cfg", "src/setup.cfg", "[metadata]\nname = shop\n\n[tool:pytest]\naddopts = --reuse-db\n"),
        ("tox-ini", "src/tox.ini", "[tox]\nenvlist = py312\n\n[pytest]\naddopts = --reuse-db\n"),
        ("conftest", "src/conftest.py", ""),
    ] {
        let dir = django_configured_in_src(&format!("django-src-{tag}"));
        touch(&dir, name, text);
        assert_eq!(detected(&dir, true), Ok(run(&[MIGRATIONS, PYTEST_IN_SRC])), "{tag}");
    }
}

/// A setup.cfg or tox.ini that only configures other tools holds no pytest settings.
#[test]
fn setup_cfg_or_tox_ini_without_a_pytest_section_are_not_pytest_settings() {
    for (tag, name, text) in [
        ("setup-cfg-other", "src/setup.cfg", "[flake8]\nmax-line-length = 100\n# [tool:pytest] comes later\n"),
        ("tox-ini-other", "src/tox.ini", "[tox]\nenvlist = py312\n[testenv:pytest]\n"),
    ] {
        let dir = django_configured_in_src(&format!("django-src-{tag}"));
        touch(&dir, name, text);
        assert_eq!(detected(&dir, true), Ok(run(&[MIGRATIONS])), "{tag}");
    }
}

/// Run from `src/`, pytest is handed the changed apps relative to it.
#[test]
fn run_from_src_pytest_is_handed_the_changed_tested_apps_relative_to_src() {
    let dir = django_configured_in_src("django-src-scoped");
    touch(&dir, "src/pytest.ini", "[pytest]\n");
    touch(&dir, "src/billing/tests.py", "");
    let changed = ["src/shop/views.py".to_string(), "src/billing/models.py".to_string()];

    assert_eq!(
        choose_gate_scoped(&dir, None, Some(&changed)),
        Ok(run(&[MIGRATIONS, &["uv", "run", "--directory", "src", "pytest", "billing/tests.py", "shop"]]))
    );
}

/// A root `pytest.ini` wins over one beside manage.py, so pytest runs from the root as before.
#[test]
fn root_pytest_settings_win_over_those_beside_manage_py() {
    let dir = django("django-root-and-src");
    touch(&dir, "src/pytest.ini", "[pytest]\n");

    assert_eq!(detected(&dir, true), Ok(run(&[MIGRATIONS, &["uv", "run", "pytest"]])));
}

const PYTEST_UNSET: &str = "pytest is a dependency but no pytest settings were found; the tests were not run";

/// pytest among the dependencies with no settings anywhere the gate looks is said, not skipped silently.
#[test]
fn pytest_as_a_dependency_without_settings_is_a_note_not_a_silent_skip() {
    for (tag, pyproject) in [
        ("dependencies", "[project]\nname = \"shop\"\ndependencies = [\"django>=5\", \"pytest>=8\"]\n"),
        ("optional", "[project]\nname = \"shop\"\n[project.optional-dependencies]\ntest = [\"pytest-django\", \"pytest==8.3\"]\n"),
        ("groups", "[project]\nname = \"shop\"\n[dependency-groups]\ndev = [\"pytest[testing]\", {include-group = \"lint\"}]\nlint = [\"ruff\"]\n"),
        ("uv-dev", "[project]\nname = \"shop\"\n[tool.uv]\ndev-dependencies = [\"Pytest ; python_version >= '3.10'\"]\n"),
    ] {
        let dir = django_configured_in_src(&format!("django-note-{tag}"));
        touch(&dir, "pyproject.toml", pyproject);
        assert_eq!(
            detected(&dir, true),
            Ok(Gate::Steps { steps: steps(&[MIGRATIONS]), notes: vec![PYTEST_UNSET.to_string()] }),
            "{tag}"
        );
    }
}

/// Without pytest among the dependencies, or with only a plugin of it, there is nothing to say.
#[test]
fn without_pytest_among_the_dependencies_there_is_no_note() {
    for (tag, pyproject) in [
        ("none", "[project]\nname = \"shop\"\ndependencies = [\"django>=5\"]\n"),
        ("plugin-only", "[project]\nname = \"shop\"\ndependencies = [\"pytest-django\"]\n"),
    ] {
        let dir = django_configured_in_src(&format!("django-no-note-{tag}"));
        touch(&dir, "pyproject.toml", pyproject);
        assert_eq!(detected(&dir, true), Ok(run(&[MIGRATIONS])), "{tag}");
    }
}

/// An app directory named like an option stays a path once `src/` is dropped, so the repository
/// cannot hand pytest an option such as `--ignore=shop`.
#[test]
fn an_app_named_like_an_option_is_still_a_path_when_pytest_runs_from_src() {
    let dir = django_configured_in_src("django-src-option-app");
    touch(&dir, "src/pytest.ini", "[pytest]\n");
    touch(&dir, "src/--ignore=shop/tests/test_x.py", "");
    let changed = ["src/--ignore=shop/views.py".to_string()];

    assert_eq!(
        choose_gate_scoped(&dir, None, Some(&changed)),
        Ok(run(&[MIGRATIONS, &["uv", "run", "--directory", "src", "pytest", "./--ignore=shop"]]))
    );
}

/// A root `tests/` directory is only where pytest looks when nothing configures it, so pytest
/// settings beside manage.py still decide where it runs.
#[test]
fn settings_beside_manage_py_win_over_a_root_tests_directory_that_configures_nothing() {
    let dir = django_configured_in_src("django-src-and-root-tests");
    touch(&dir, "src/pytest.ini", "[pytest]\n");
    touch(&dir, "tests/test_smoke.py", "");

    assert_eq!(detected(&dir, true), Ok(run(&[MIGRATIONS, PYTEST_IN_SRC])));
}

/// The root's settings are found the way those beside manage.py are, and win over them.
#[test]
fn root_setup_cfg_or_tox_ini_settings_win_over_those_beside_manage_py() {
    for (tag, name, text) in [
        ("setup-cfg", "setup.cfg", "[tool:pytest]\naddopts = -q\n"),
        ("tox-ini", "tox.ini", "[pytest]\naddopts = -q\n"),
    ] {
        let dir = django_configured_in_src(&format!("django-root-{tag}-and-src"));
        touch(&dir, "src/pytest.ini", "[pytest]\n");
        touch(&dir, name, text);
        assert_eq!(detected(&dir, true), Ok(run(&[MIGRATIONS, &["uv", "run", "pytest"]])), "{tag}");
    }
}

/// A root conftest.py shares fixtures but configures nothing, so settings beside manage.py still
/// decide where pytest runs, and a root conftest.py alone still runs pytest from the root.
#[test]
fn a_root_conftest_py_does_not_hide_settings_beside_manage_py() {
    let dir = django_configured_in_src("django-src-and-root-conftest");
    touch(&dir, "src/pytest.ini", "[pytest]\n");
    touch(&dir, "conftest.py", "");
    assert_eq!(detected(&dir, true), Ok(run(&[MIGRATIONS, PYTEST_IN_SRC])));

    let dir = django_configured_in_src("django-root-conftest-only");
    touch(&dir, "conftest.py", "");
    assert_eq!(detected(&dir, true), Ok(run(&[MIGRATIONS, &["uv", "run", "pytest"]])));
}
