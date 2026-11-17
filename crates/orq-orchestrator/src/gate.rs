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
            // nextest fails a run that finds no test; the plain runner passes it, and so does the gate.
            steps.push(argv(&["cargo", "nextest", "run", "--workspace", "--no-fail-fast", "--no-tests=pass"]));
            // nextest runs no doc tests: cargo test runs them, when Cargo would doc-test a target.
            if may_have_doc_tests(dir) {
                steps.push(argv(&["cargo", "test", "--workspace", "--doc"]));
            }
        } else {
            notes.push("cargo-nextest not found: running cargo test".to_string());
            steps.push(argv(&["cargo", "test", "--workspace"]));
        }
    }
}

/// Whether Cargo would doc-test any target of the workspace in `dir`, as `cargo metadata` describes
/// it: a library, wherever its `[lib] path` points, unless it sets `doctest = false`. When the
/// metadata cannot be read, this says yes, so doc tests are never dropped for want of an answer.
fn may_have_doc_tests(dir: &Path) -> bool {
    let args = ["metadata", "--no-deps", "--format-version", "1"];
    let Some(out) = run_bounded("cargo".as_ref(), &args, dir, NEXTEST_PROBE_LIMIT) else {
        return true;
    };
    let Ok(metadata) = serde_json::from_slice::<serde_json::Value>(&out) else {
        return true;
    };
    let Some(packages) = metadata["packages"].as_array() else {
        return true;
    };
    packages
        .iter()
        .filter_map(|package| package["targets"].as_array())
        .flatten()
        .any(|target| target["doctest"].as_bool() == Some(true))
}

/// How long `cargo nextest --version` may take before nextest counts as missing.
const NEXTEST_PROBE_LIMIT: std::time::Duration = std::time::Duration::from_secs(30);

/// Whether `cargo nextest --version` succeeds in `dir`, with the environment a gate step gets.
fn nextest_available(dir: &Path) -> bool {
    run_bounded("cargo".as_ref(), &["nextest", "--version"], dir, NEXTEST_PROBE_LIMIT).is_some()
}

/// What `program args` prints in `dir`, when it starts and exits 0 within `limit`; `None` otherwise.
/// It gets only the environment a gate step gets, and runs in its own process group: at the limit the
/// whole group is killed, including a descendant still holding the output open after the tool exits.
fn run_bounded(program: &std::ffi::OsStr, args: &[&str], dir: &Path, limit: std::time::Duration) -> Option<Vec<u8>> {
    use std::io::Read;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(dir)
        .env_clear()
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for name in ["PATH", "HOME", "TERM"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    let mut child = command.spawn().ok()?;
    let group = child.id() as libc::pid_t;
    // SAFETY: kill(2) on the group the child leads; it takes no memory of ours. Called only while the
    // child is unreaped or a member of its group still holds the pipe, so the group id is not reused.
    let kill_group = || unsafe {
        libc::kill(-group, libc::SIGKILL);
    };
    // Read on the side, so output larger than a pipe holds cannot stall the tool. The reader reports
    // through a channel, so waiting for it keeps to the same deadline.
    let Some(mut stdout) = child.stdout.take() else {
        kill_group();
        let _ = child.wait();
        return None;
    };
    let (sender, output) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = sender.send(stdout.read_to_end(&mut out).map(|_| out));
    });
    let deadline = std::time::Instant::now() + limit;
    let exited = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Ok(None) if std::time::Instant::now() < deadline => std::thread::sleep(std::time::Duration::from_millis(20)),
            _ => {
                kill_group();
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    let left = deadline.saturating_duration_since(std::time::Instant::now());
    match output.recv_timeout(left) {
        Ok(Ok(out)) if exited => Some(out),
        Ok(_) => None,
        Err(_) => {
            // A descendant still holds the output open past the limit: stop the whole group.
            kill_group();
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::run_bounded;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    fn scratch(tag: &str) -> PathBuf {
        let dir = orq_test_support::process_temp_dir().join(format!("orq-gate-probe-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Run `body` as a shell script in `dir` through the bounded runner, with `args` as `$1`, `$2`, ...
    /// It is handed to `sh` rather than exec'd itself: a file just written can fail to exec with
    /// ETXTBSY while another test thread's fork still holds it open for writing.
    fn run_script(dir: &Path, body: &str, args: &[&str], limit: Duration) -> Option<Vec<u8>> {
        let path = dir.join("tool");
        std::fs::write(&path, format!("{body}\n")).unwrap();
        let path = path.to_str().unwrap();
        let argv: Vec<&str> = std::iter::once(path).chain(args.iter().copied()).collect();
        run_bounded("sh".as_ref(), &argv, dir, limit)
    }

    const LIMIT: Duration = Duration::from_secs(10);

    /// Whether process `pid` stops running within a few seconds: it no longer exists, or it is a
    /// zombie no one has reaped yet. SIGKILL lands a moment after it is sent, and an orphan's zombie
    /// lingers until its adopter reaps it, so neither an instant check nor `kill -0` will do. `ps` asks
    /// the same on Linux and macOS; when it cannot answer, the test fails rather than guess.
    fn gone(pid: &str) -> bool {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let ps = std::process::Command::new("ps").args(["-o", "stat=", "-p", pid]).output().expect("ps runs");
            let (state, problem) = (String::from_utf8_lossy(&ps.stdout), String::from_utf8_lossy(&ps.stderr));
            // No row and no complaint: no such process. A row's state starts with Z for a zombie.
            assert!(problem.trim().is_empty(), "could not look up process {pid}: {problem}");
            let ended = state.trim().is_empty() || state.trim_start().starts_with('Z');
            if ended || Instant::now() >= deadline {
                return ended;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn a_tool_that_succeeds_gives_its_output_from_the_folder_it_ran_in() {
        let dir = scratch("ok");
        let out = run_script(&dir, "pwd; echo \"$1 $2\"", &["nextest", "--version"], LIMIT).expect("it succeeded");
        let text = String::from_utf8(out).unwrap();
        let here = std::fs::canonicalize(&dir).unwrap();
        assert_eq!(text, format!("{}\nnextest --version\n", here.display()));
    }

    #[test]
    fn a_tool_that_exits_non_zero_or_cannot_start_gives_nothing() {
        let dir = scratch("fail");
        assert_eq!(run_script(&dir, "echo partial; exit 101", &[], LIMIT), None, "non-zero exit");
        let missing = dir.join("no-such-tool");
        assert_eq!(run_bounded(missing.as_os_str(), &[], &dir, LIMIT), None, "missing program");
    }

    #[test]
    fn a_tool_that_hangs_is_stopped_at_the_limit_and_left_dead() {
        let dir = scratch("hang");
        let pid_file = dir.join("pid");
        let started = Instant::now();
        let body = format!("echo $$ > {}; exec sleep 30", pid_file.display());
        assert_eq!(run_script(&dir, &body, &[], Duration::from_secs(2)), None);
        assert!(started.elapsed() < Duration::from_secs(10), "returned at the limit: {:?}", started.elapsed());
        let pid = std::fs::read_to_string(&pid_file).unwrap();
        assert!(gone(pid.trim()), "the hung tool {pid} was killed");
    }

    #[test]
    fn a_tool_that_exits_while_a_descendant_holds_its_output_open_is_stopped_at_the_limit() {
        let dir = scratch("stray");
        let pid_file = dir.join("pid");
        let started = Instant::now();
        let body = format!("sleep 30 & echo $! > {}; echo done; exit 0", pid_file.display());
        assert_eq!(run_script(&dir, &body, &[], Duration::from_secs(2)), None, "its output never ended");
        assert!(started.elapsed() < Duration::from_secs(10), "returned at the limit: {:?}", started.elapsed());
        let pid = std::fs::read_to_string(&pid_file).unwrap();
        assert!(gone(pid.trim()), "the descendant {pid} was killed");
    }

    #[test]
    fn a_process_that_ended_but_was_never_reaped_counts_as_gone() {
        let mut child = std::process::Command::new("true").spawn().unwrap();
        // Left unreaped, it stays a zombie that kill -0 still finds.
        assert!(gone(&child.id().to_string()), "a zombie is not running");
        let _ = child.wait();
        let mut child = std::process::Command::new("sleep").arg("30").spawn().unwrap();
        assert!(!gone(&child.id().to_string()), "a sleeping process is running");
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    #[should_panic(expected = "could not look up process")]
    fn a_process_that_cannot_be_looked_up_is_not_taken_for_gone() {
        gone("not-a-pid");
    }

    #[test]
    fn a_tool_with_more_output_than_a_pipe_holds_still_finishes() {
        let dir = scratch("big");
        let out = run_script(&dir, "head -c 300000 /dev/zero | tr '\\0' x", &[], LIMIT).expect("it finished");
        assert_eq!(out.len(), 300_000);
    }
}
