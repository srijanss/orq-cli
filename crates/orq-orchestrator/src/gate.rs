//! The local gate: the checks a run's worktree must pass before anything is
//! pushed. A configured gate wins; otherwise the worktree's stacks decide,
//! mirroring the Bash orchestrator's detection (Python with uv, Node, Rust).
//!
//! Gates are lists of argv vectors, never shell strings, so nothing from a
//! feature file or an agent is ever interpreted by a shell. Django projects
//! and the optional lint/build steps are not detected yet.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

/// What to run, or why there is nothing to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gate {
    /// Run these argv steps in order in the worktree; the gate fails if any
    /// step fails. `notes` records stacks that were seen but skipped.
    Steps { steps: Vec<Vec<String>>, notes: Vec<String> },
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
            GateError::EmptyProgram => write!(f, "the configured gate command has no program to run"),
        }
    }
}

impl std::error::Error for GateError {}

const NOTHING_DETECTED: &str = "no local gate detected: no Cargo.toml, package.json test scripts or uv.lock with pytest, and no gate command configured";

/// The gate for `worktree`: `configured` (a non-empty argv) if given,
/// otherwise whatever its Python, Node and Rust files call for, in that order.
pub fn choose_gate(worktree: &Path, configured: Option<&[String]>) -> Result<Gate, GateError> {
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
    python_steps(worktree, &mut steps, &mut notes);
    node_steps(worktree, &mut steps, &mut notes);
    rust_steps(worktree, &mut steps);
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

fn python_steps(dir: &Path, steps: &mut Vec<Vec<String>>, notes: &mut Vec<String>) {
    if dir.join("src/manage.py").is_file() {
        notes.push("Django project (src/manage.py) is not supported yet; set a gate command to check it".to_string());
        return;
    }
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
            Some(doc) => pytest_in_pyproject = doc.get("tool").and_then(|tool| tool.get("pytest")).is_some(),
            None => notes.push("pyproject.toml could not be read as TOML; settings in it were ignored".to_string()),
        }
    }
    let pytest_configured = pytest_in_pyproject
        || dir.join("pytest.ini").is_file()
        || dir.join("tests").is_dir();
    if pytest_configured {
        steps.push(argv(&["uv", "run", "pytest"]));
    }
}

fn node_steps(dir: &Path, steps: &mut Vec<Vec<String>>, notes: &mut Vec<String>) {
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
                    command.as_str().is_some_and(|text| name.as_str() != "test" || !is_npm_placeholder_test(text))
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
    if scripts.contains("test") {
        steps.push(argv(&[manager, "run", "test"]));
    }
}

/// The script `npm init` writes: `echo "Error: no test specified" && exit 1`.
fn is_npm_placeholder_test(command: &str) -> bool {
    let command = command.trim();
    command.starts_with("echo ") && command.contains("no test specified") && command.ends_with("exit 1")
}

fn rust_steps(dir: &Path, steps: &mut Vec<Vec<String>>) {
    if dir.join("Cargo.toml").is_file() {
        steps.push(argv(&["cargo", "test", "--workspace"]));
    }
}
