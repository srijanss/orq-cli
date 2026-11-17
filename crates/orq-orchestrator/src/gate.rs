//! The local gate: the checks a run's worktree must pass before anything is
//! pushed. A configured gate wins; otherwise the worktree's stacks decide,
//! mirroring the Bash orchestrator's detection (Python with uv, Node, Rust).
//!
//! Gates are lists of argv vectors, never shell strings, so nothing from a
//! feature file or an agent is ever interpreted by a shell. Lint and build
//! steps are optional and come only from [`GateOptions`].

use std::collections::HashSet;
use std::fs;
use std::path::Path;

/// What to run, or why there is nothing to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gate {
    /// Run these argv steps in order in the worktree; the gate fails if any
    /// step fails. `notes` records stacks that were seen but skipped.
    Steps {
        steps: Vec<Vec<String>>,
        notes: Vec<String>,
    },
    /// No gate: the run continues, with the reason recorded.
    Skipped { reason: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateError {
    /// A configured gate whose first word (the program) is blank.
    EmptyProgram,
}

impl std::fmt::Display for GateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GateError::EmptyProgram => {
                write!(f, "the configured gate command has no program to run")
            }
        }
    }
}

impl std::error::Error for GateError {}

const NOTHING_DETECTED: &str = "no local gate detected: no Cargo.toml, package.json test scripts or uv.lock with pytest, and no gate command configured";

/// The gate for `worktree`: `configured` (a non-empty argv) if given,
/// otherwise whatever its Python, Node and Rust files call for, in that order.
pub fn choose_gate(worktree: &Path, configured: Option<&[String]>) -> Result<Gate, GateError> {
    choose_gate_scoped(worktree, configured, None)
}

/// Which optional parts of the detected gate to include.
#[derive(Debug, Clone, Copy, Default)]
pub struct GateOptions<'a> {
    /// Paths changed by the run, relative to the worktree. A Django project then runs pytest only for the
    /// changed apps that have tests, and ruff checks only the changed Python files.
    pub changed: Option<&'a [String]>,
    /// ruff for Python, the package.json `lint` script, `cargo fmt --check` and clippy.
    pub lint: bool,
    /// The package.json `build` script.
    pub build: bool,
    /// Whether cargo-nextest runs in the worktree; `None` asks `cargo nextest --version` there.
    pub nextest: Option<bool>,
}

/// Like [`choose_gate`], with a scope for Django projects.
pub fn choose_gate_scoped(
    worktree: &Path,
    configured: Option<&[String]>,
    changed: Option<&[String]>,
) -> Result<Gate, GateError> {
    choose_gate_with(
        worktree,
        configured,
        &GateOptions {
            changed,
            ..GateOptions::default()
        },
    )
}

/// Like [`choose_gate`], with the optional steps `options` asks for. A configured gate ignores them.
pub fn choose_gate_with(
    worktree: &Path,
    configured: Option<&[String]>,
    options: &GateOptions,
) -> Result<Gate, GateError> {
    if let Some(argv) = configured.filter(|argv| !argv.is_empty()) {
        if argv[0].trim().is_empty() {
            return Err(GateError::EmptyProgram);
        }
        return Ok(Gate::Steps {
            steps: vec![argv.to_vec()],
            notes: Vec::new(),
        });
    }
    let mut steps = Vec::new();
    let mut notes = Vec::new();
    python_steps(worktree, options, &mut steps, &mut notes);
    node_steps(worktree, options, &mut steps, &mut notes);
    rust_steps(worktree, options, &mut steps, &mut notes);
    if steps.is_empty() {
        let reason = if notes.is_empty() {
            NOTHING_DETECTED.to_string()
        } else {
            notes.join("; ")
        };
        return Ok(Gate::Skipped { reason });
    }
    Ok(Gate::Steps { steps, notes })
}

fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| word.to_string()).collect()
}

fn python_steps(
    dir: &Path,
    options: &GateOptions,
    steps: &mut Vec<Vec<String>>,
    notes: &mut Vec<String>,
) {
    let django = dir.join("src/manage.py").is_file();
    if !dir.join("uv.lock").is_file() {
        return;
    }
    let pyproject_path = dir.join("pyproject.toml");
    let mut pytest_in_pyproject = false;
    if pyproject_path.is_file() {
        match fs::read_to_string(&pyproject_path)
            .ok()
            .and_then(|text| text.parse::<toml_edit::DocumentMut>().ok())
        {
            Some(doc) => {
                pytest_in_pyproject = doc
                    .get("tool")
                    .and_then(|tool| tool.get("pytest"))
                    .is_some()
            }
            None => notes.push(
                "pyproject.toml could not be read as TOML; settings in it were ignored".to_string(),
            ),
        }
    }
    let pytest_configured =
        pytest_in_pyproject || dir.join("pytest.ini").is_file() || dir.join("tests").is_dir();
    let changed = options.changed;
    if options.lint {
        let mut ruff = argv(&["uv", "run", "ruff", "check"]);
        // A change to ruff's configuration can affect every file, so it checks the whole project.
        let config_changed = changed.is_some_and(|changed| {
            changed.iter().any(|path| {
                matches!(
                    path.rsplit('/').next(),
                    Some("ruff.toml" | ".ruff.toml" | "pyproject.toml")
                )
            })
        });
        match changed.filter(|_| !config_changed) {
            Some(changed) => {
                let mut files: Vec<String> = changed
                    .iter()
                    .filter(|path| path.ends_with(".py") && dir.join(path).is_file())
                    .cloned()
                    .collect();
                files.sort();
                files.dedup();
                if !files.is_empty() {
                    // After `--` a name like `--output-file=x.py` is a file, not an option.
                    ruff.push("--".to_string());
                    ruff.extend(files);
                    steps.push(ruff);
                }
            }
            None => steps.push(ruff),
        }
    }
    if django {
        steps.push(argv(&[
            "uv",
            "run",
            "python",
            "src/manage.py",
            "makemigrations",
            "--check",
            "--dry-run",
        ]));
    }
    if !pytest_configured {
        return;
    }
    let mut pytest = argv(&["uv", "run", "pytest"]);
    if let (true, Some(changed)) = (django, changed) {
        match changed_tested_apps(dir, changed) {
            Some(targets) if targets.is_empty() => return,
            Some(targets) => pytest.extend(targets),
            None => {}
        }
    }
    steps.push(pytest);
}

/// What to hand pytest for a change that touches only documentation and Django apps with tests, sorted:
/// the app directory when it has a `tests` package, and its `tests.py` by name, which default discovery
/// would not collect. `None` when anything else changed (dependencies, settings, an app without tests,
/// root tests): that can affect any app, so the whole suite runs.
fn changed_tested_apps(dir: &Path, changed: &[String]) -> Option<Vec<String>> {
    let mut targets = Vec::new();
    for path in changed {
        if [".md", ".rst"].iter().any(|ext| path.ends_with(ext)) {
            continue;
        }
        let app = path
            .strip_prefix("src/")
            .and_then(|rest| rest.split_once('/'))
            .map(|(app, _)| format!("src/{app}"))?;
        let package = dir.join(&app).join("tests").is_dir();
        let module = dir.join(&app).join("tests.py").is_file();
        if !package && !module {
            return None;
        }
        if package {
            targets.push(app.clone());
        }
        if module {
            targets.push(format!("{app}/tests.py"));
        }
    }
    targets.sort();
    targets.dedup();
    Some(targets)
}

fn node_steps(
    dir: &Path,
    options: &GateOptions,
    steps: &mut Vec<Vec<String>>,
    notes: &mut Vec<String>,
) {
    let manifest = dir.join("package.json");
    if !manifest.is_file() {
        return;
    }
    if !dir.join("node_modules").is_dir() && !dir.join(".pnp.cjs").is_file() {
        notes.push(
            "package.json found but node_modules is missing; install the dependencies to run the Node checks".to_string(),
        );
        return;
    }
    let Ok(text) = fs::read_to_string(&manifest) else {
        notes.push("package.json could not be read; the Node checks were skipped".to_string());
        return;
    };
    let parsed = serde_json::from_str::<serde_json::Value>(&text).ok();
    let Some(manifest) = parsed else {
        notes.push("package.json is not valid JSON; the Node checks were skipped".to_string());
        return;
    };
    // Real scripts only: npm's placeholder `test` is not a test. Only the
    // `test` script can be the placeholder, and only when it is that command.
    let scripts: HashSet<&str> = manifest
        .get("scripts")
        .and_then(|scripts| scripts.as_object())
        .map(|scripts| {
            scripts
                .iter()
                .filter(|(name, command)| {
                    command.as_str().is_some_and(|text| {
                        name.as_str() != "test" || !is_npm_placeholder_test(text)
                    })
                })
                .map(|(name, _)| name.as_str())
                .collect()
        })
        .unwrap_or_default();

    let manager = if dir.join("pnpm-lock.yaml").is_file() {
        "pnpm"
    } else if dir.join("yarn.lock").is_file() {
        "yarn"
    } else if dir.join("bun.lockb").is_file() || dir.join("bun.lock").is_file() {
        "bun"
    } else {
        "npm"
    };
    if scripts.contains("typecheck") {
        steps.push(argv(&[manager, "run", "typecheck"]));
    } else if scripts.contains("type-check") {
        steps.push(argv(&[manager, "run", "type-check"]));
    } else if dir.join("tsconfig.json").is_file() && dir.join("node_modules/.bin/tsc").is_file() {
        steps.push(argv(&["node_modules/.bin/tsc", "--noEmit"]));
    }
    if options.lint && scripts.contains("lint") {
        steps.push(argv(&[manager, "run", "lint"]));
    }
    if scripts.contains("test") {
        steps.push(argv(&[manager, "run", "test"]));
    }
    if options.build && scripts.contains("build") {
        steps.push(argv(&[manager, "run", "build"]));
    }
}

/// The script `npm init` writes: `echo "Error: no test specified" && exit 1`.
fn is_npm_placeholder_test(command: &str) -> bool {
    command.trim() == r#"echo "Error: no test specified" && exit 1"#
}

fn rust_steps(
    dir: &Path,
    options: &GateOptions,
    steps: &mut Vec<Vec<String>>,
    notes: &mut Vec<String>,
) {
    if dir.join("Cargo.toml").is_file() {
        if options.lint {
            steps.push(argv(&["cargo", "fmt", "--check"]));
            steps.push(argv(&[
                "cargo",
                "clippy",
                "--workspace",
                "--all-targets",
                "--",
                "-D",
                "warnings",
            ]));
        }
        // Asked once per detection, so one gate run never mixes the two.
        if options.nextest.unwrap_or_else(|| nextest_available(dir)) {
            steps.push(argv(&["cargo", "nextest", "run", "--workspace", "--no-fail-fast"]));
            // nextest runs no doc tests: cargo test runs them, only when there are some.
            if has_doc_tests(dir) {
                steps.push(argv(&["cargo", "test", "--workspace", "--doc"]));
            }
        } else {
            notes.push("cargo-nextest not found: running cargo test".to_string());
            steps.push(argv(&["cargo", "test", "--workspace"]));
        }
    }
}

/// Whether a library under `dir` (a folder with a `Cargo.toml` and a `src/lib.rs`) has a Rust code
/// block in a doc comment of its sources. Build output, dependencies, hidden folders and links are not
/// looked in.
fn has_doc_tests(dir: &Path) -> bool {
    let skipped = |name: &str| name.starts_with('.') || name == "target" || name == "node_modules";
    let is_library = dir.join("Cargo.toml").is_file() && dir.join("src/lib.rs").is_file();
    if is_library && rust_sources_have_doc_tests(&dir.join("src")) {
        return true;
    }
    subdirectories(dir)
        .filter(|path| path.file_name().and_then(|name| name.to_str()).is_some_and(|name| !skipped(name)))
        .any(|path| has_doc_tests(&path))
}

/// The folders directly in `dir`, without following links.
fn subdirectories(dir: &Path) -> impl Iterator<Item = std::path::PathBuf> {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
}

/// Whether a `.rs` file anywhere under `src` has a doc test.
fn rust_sources_have_doc_tests(src: &Path) -> bool {
    let files = fs::read_dir(src).into_iter().flatten().flatten().filter(|entry| {
        entry.file_type().is_ok_and(|kind| kind.is_file())
            && entry.path().extension().is_some_and(|ext| ext == "rs")
    });
    for file in files {
        if fs::read_to_string(file.path()).is_ok_and(|text| has_doc_test_block(&text)) {
            return true;
        }
    }
    subdirectories(src).any(|path| rust_sources_have_doc_tests(&path))
}

/// Whether a `///` or `//!` comment in `text` opens a fenced code block rustdoc runs: one with no
/// language, or only Rust's own attributes (`rust`, `no_run`, `should_panic`, ...).
fn has_doc_test_block(text: &str) -> bool {
    let mut open = false;
    for line in text.lines().map(str::trim_start) {
        if line.starts_with("////") {
            continue;
        }
        let Some(doc) = line.strip_prefix("///").or_else(|| line.strip_prefix("//!")) else {
            continue;
        };
        let Some(info) = doc.trim().strip_prefix("```") else {
            continue;
        };
        if open {
            open = false;
            continue;
        }
        open = true;
        if is_rust_fence(info) {
            return true;
        }
    }
    false
}

/// Whether a code fence's info string leaves its block a Rust doc test.
fn is_rust_fence(info: &str) -> bool {
    info.split([',', ' ', '\t'])
        .filter(|word| !word.is_empty())
        .all(|word| {
            matches!(
                word,
                "rust" | "ignore" | "should_panic" | "no_run" | "compile_fail" | "test_harness" | "standalone_crate"
            ) || word.starts_with("edition")
        })
}

/// How long `cargo nextest --version` may take before nextest counts as missing.
const NEXTEST_PROBE_LIMIT: std::time::Duration = std::time::Duration::from_secs(30);

/// Whether `cargo nextest --version` succeeds in `dir`, with the environment a gate step gets.
fn nextest_available(dir: &Path) -> bool {
    use std::process::{Command, Stdio};
    let mut command = Command::new("cargo");
    command
        .args(["nextest", "--version"])
        .current_dir(dir)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for name in ["PATH", "HOME", "TERM"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    let Ok(mut child) = command.spawn() else {
        return false;
    };
    let deadline = std::time::Instant::now() + NEXTEST_PROBE_LIMIT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(20))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}
