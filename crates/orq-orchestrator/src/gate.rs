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
    rust_steps(worktree, options, &mut steps);
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

fn rust_steps(dir: &Path, options: &GateOptions, steps: &mut Vec<Vec<String>>) {
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
        steps.push(argv(&["cargo", "test", "--workspace"]));
    }
}
