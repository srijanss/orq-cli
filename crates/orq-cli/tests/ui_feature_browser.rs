use std::path::Path;
use std::process::Command;

use orq_test_support::{write_ledger, write_ledger_bytes, GitFixture, LedgerEntry};

/// Drives `orq ui` on a pseudo-terminal through `steps`, a JSON list of
/// `{"wait": text}`, `{"send": hex}`, `{"sleep": seconds}` and
/// `{"write": [path, text]}` steps. Returns (exit code, everything drawn).
fn drive(pane_dir: &Path, steps: serde_json::Value) -> (i32, String) {
    const DRIVER: &str = r#"
import fcntl, json, os, pty, select, struct, sys, termios, time
steps = json.loads(sys.argv[2])
fd, slave = pty.openpty()
fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 100, 0, 0))
pid = os.fork()
if pid == 0:
    os.setsid()
    fcntl.ioctl(slave, termios.TIOCSCTTY, 0)
    for n in (0, 1, 2):
        os.dup2(slave, n)
    os.close(fd)
    os.close(slave)
    os.execv(sys.argv[1], [sys.argv[1], "ui"])
out = b""

def pump(timeout):
    global out
    r, _, _ = select.select([fd], [], [], timeout)
    if r:
        try:
            chunk = os.read(fd, 4096)
        except OSError:
            return False
        if not chunk:
            return False
        out += chunk
    return True

for step in steps:
    if "wait" in step:
        needle = step["wait"].encode()
        mark = len(out)
        deadline = time.time() + 10
        while needle not in out[mark:] and time.time() < deadline:
            if not pump(0.1):
                break
    elif "send" in step:
        os.write(fd, bytes.fromhex(step["send"]))
    elif "sleep" in step:
        end = time.time() + step["sleep"]
        while time.time() < end:
            pump(0.05)
    elif "write" in step:
        with open(step["write"][0], "w") as f:
            f.write(step["write"][1])

status = None
end = time.time() + 10
while status is None and time.time() < end:
    pump(0.05)
    done, st = os.waitpid(pid, os.WNOHANG)
    if done:
        status = st
if status is None:
    import signal
    os.kill(pid, signal.SIGKILL)
    os.waitpid(pid, 0)
    sys.stdout.buffer.write(out)
    sys.exit(124)
while select.select([fd], [], [], 0)[0]:
    try:
        chunk = os.read(fd, 4096)
    except OSError:
        break
    if not chunk:
        break
    out += chunk
sys.stdout.buffer.write(out)
sys.exit(os.waitstatus_to_exitcode(status))
"#;
    let output = Command::new("python3")
        .args(["-I", "-c", DRIVER, env!("CARGO_BIN_EXE_orq"), &steps.to_string()])
        .env("HERDR_ACTIVE_PANE_CWD", pane_dir)
        .env("GIT_CEILING_DIRECTORIES", std::env::temp_dir())
        .output()
        .unwrap();
    (output.status.code().unwrap(), String::from_utf8_lossy(&output.stdout).into_owned())
}

const DOWN: &str = "1b5b42";
const P: &str = "70";
const ESC: &str = "1b";

fn promote_steps() -> serde_json::Value {
    serde_json::json!([
        {"wait": "alpha"},
        {"send": DOWN},
        {"sleep": 0.3},
        {"send": P},
        {"sleep": 0.5},
        {"send": ESC},
    ])
}

fn ledger_text(dir: &Path) -> String {
    std::fs::read_to_string(dir.join(".tdd-features.json")).unwrap()
}

#[test]
fn promoting_from_the_popup_changes_only_this_repos_ledger_and_survives_reopening() {
    let a = GitFixture::new();
    let b = GitFixture::new();
    write_ledger(a.root(), &[LedgerEntry::new("alpha", "draft")]);
    write_ledger(b.root(), &[LedgerEntry::new("other", "draft")]);
    let b_before = ledger_text(b.root());

    let (code, drawn) = drive(a.root(), promote_steps());

    assert_eq!(code, 0, "{drawn}");
    assert!(drawn.contains("alpha"), "{drawn}");
    assert!(!drawn.contains("other"), "{drawn}");
    let a_after = ledger_text(a.root());
    assert!(a_after.contains("\"pending\"") && !a_after.contains("\"draft\""), "{a_after}");
    assert_eq!(ledger_text(b.root()), b_before);

    // Closing and reopening shows the feature as pending, without a button.
    let (code, reopened) = drive(a.root(), serde_json::json!([{"wait": "alpha"}, {"sleep": 0.3}, {"send": ESC}]));
    assert_eq!(code, 0, "{reopened}");
    assert!(reopened.contains("pending"), "{reopened}");
    assert!(!reopened.contains("Mark pending"), "{reopened}");
}

#[test]
fn unknown_fields_survive_the_edit() {
    let repo = GitFixture::new();
    let original = "[\n  {\n    \"featureName\": \"alpha\",\n    \"status\": \"draft\",\n    \"futureField\": {\"keep\": [1, 2]}\n  }\n]\n";
    write_ledger_bytes(repo.root(), original.as_bytes());

    let (code, drawn) = drive(repo.root(), promote_steps());

    assert_eq!(code, 0, "{drawn}");
    let after = ledger_text(repo.root());
    assert_eq!(after, original.replace("\"draft\"", "\"pending\""));
}

#[test]
fn a_malformed_ledger_is_shown_read_only_with_its_error() {
    let repo = GitFixture::new();
    write_ledger_bytes(repo.root(), b"{ not json");

    let (code, drawn) = drive(
        repo.root(),
        serde_json::json!([{"wait": "malformed"}, {"sleep": 0.3}, {"send": DOWN}, {"send": P}, {"sleep": 0.3}, {"send": ESC}]),
    );

    assert_eq!(code, 0, "{drawn}");
    assert!(drawn.contains("malformed") && drawn.contains(".tdd-features.json"), "{drawn}");
    assert!(!drawn.contains("Mark pending"), "{drawn}");
    assert_eq!(ledger_text(repo.root()), "{ not json");
}

#[test]
fn a_file_changed_behind_the_popups_back_is_a_conflict_and_is_not_overwritten() {
    let repo = GitFixture::new();
    write_ledger(repo.root(), &[LedgerEntry::new("alpha", "draft")]);
    let path = repo.root().join(".tdd-features.json");
    let external = "[{\"featureName\":\"alpha\",\"status\":\"draft\"},{\"featureName\":\"zeta\",\"status\":\"draft\"}]";

    let (code, drawn) = drive(
        repo.root(),
        serde_json::json!([
            {"wait": "alpha"},
            {"send": DOWN},
            {"sleep": 0.3},
            {"write": [path.to_str().unwrap(), external]},
            {"send": P},
            {"sleep": 0.5},
            {"send": ESC},
        ]),
    );

    assert_eq!(code, 0, "{drawn}");
    assert_eq!(ledger_text(repo.root()), external);
    assert!(drawn.contains("nothing was saved"), "{drawn}");
    assert!(drawn.contains("zeta"), "{drawn}");
}

#[test]
fn enter_opens_the_detail_view_and_escape_returns_to_the_list_before_closing() {
    let repo = GitFixture::new();
    write_ledger(
        repo.root(),
        &[
            LedgerEntry::new("base", "completed"),
            LedgerEntry::new("alpha", "pending").description("zebrastripe").depends_on(&["base"]),
        ],
    );

    let (code, drawn) = drive(
        repo.root(),
        serde_json::json!([
            {"wait": "alpha"},
            {"send": DOWN},
            {"send": DOWN},
            {"sleep": 0.3},
            {"send": "0d"},
            {"wait": "zebrastripe"},
            {"sleep": 0.3},
            {"send": ESC},
            {"sleep": 0.3},
            {"send": ESC},
        ]),
    );

    assert_eq!(code, 0, "{drawn}");
    assert!(drawn.contains("zebrastripe"), "{drawn}");
    assert!(drawn.contains("depends"), "{drawn}");
}
