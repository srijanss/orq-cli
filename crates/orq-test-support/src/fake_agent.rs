use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// What a [`FakeAgent`] observed on its most recent invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub args: Vec<String>,
    pub cwd: PathBuf,
}

/// A scriptable stand-in for a harness executable (claude/pi).
pub struct FakeAgent {
    dir: PathBuf,
    script: PathBuf,
}

pub struct FakeAgentBuilder {
    stdout: String,
    stderr: String,
    exit_code: i32,
    hang: bool,
    session_id: Option<String>,
    steps: Vec<Step>,
}

/// Something the agent does to its working directory before it prints.
enum Step {
    Write { path: String, body: String },
    Commit { message: String },
}

impl FakeAgent {
    pub fn builder() -> FakeAgentBuilder {
        FakeAgentBuilder {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: 0,
            hang: false,
            session_id: None,
            steps: Vec::new(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.script
    }

    /// The cwd and argv of the most recent invocation, if it was ever run.
    pub fn last_run(&self) -> Option<Run> {
        self.read_record("record")
    }

    /// The cwd and argv of the first invocation, if it was ever run.
    pub fn first_run(&self) -> Option<Run> {
        self.read_record("first")
    }

    fn read_record(&self, name: &str) -> Option<Run> {
        let raw = fs::read(self.dir.join(name)).ok()?;
        let mut fields = raw
            .split(|byte| *byte == 0)
            .map(|field| String::from_utf8_lossy(field).into_owned())
            .collect::<Vec<_>>();
        fields.pop(); // trailing NUL leaves one empty field
        let mut fields = fields.into_iter();
        let cwd = PathBuf::from(fields.next()?);
        Some(Run {
            args: fields.collect(),
            cwd,
        })
    }
}

impl FakeAgentBuilder {
    pub fn stdout(mut self, text: &str) -> Self {
        self.stdout = text.to_string();
        self
    }

    /// Text written to standard error before the agent finishes.
    pub fn stderr(mut self, text: &str) -> Self {
        self.stderr = text.to_string();
        self
    }

    pub fn exit_code(mut self, code: i32) -> Self {
        self.exit_code = code;
        self
    }

    /// Keep running (after recording argv/cwd) until the process is killed.
    pub fn hang(mut self) -> Self {
        self.hang = true;
        self
    }

    /// Create (or overwrite) `path`, relative to the agent's cwd, with
    /// `contents` before the agent prints anything. Steps run in call order.
    pub fn write_file(mut self, path: &str, contents: &str) -> Self {
        assert!(
            !path.is_empty()
                && !path.starts_with('/')
                && !path.contains('\n')
                && path.split('/').all(|part| !part.is_empty() && part != "." && part != ".."),
            "write_file needs a relative path inside the cwd, got {path:?}"
        );
        self.steps.push(Step::Write {
            path: path.to_string(),
            body: contents.to_string(),
        });
        self
    }

    /// Stage everything in the agent's cwd and commit it with `message`,
    /// using a fixed identity and no hooks, signing or user git config.
    pub fn commit(mut self, message: &str) -> Self {
        assert!(!message.trim().is_empty(), "commit needs a message");
        self.steps.push(Step::Commit {
            message: message.to_string(),
        });
        self
    }

    /// Print `{"session_id":"<id>"}` on the first line of standard output.
    pub fn session_id(mut self, id: &str) -> Self {
        assert!(
            !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "session id must be letters, digits, - or _, got {id:?}"
        );
        self.session_id = Some(id.to_string());
        self
    }

    pub fn build(self) -> FakeAgent {
        let dir = crate::private_dir("orq-fake-agent");
        fs::write(dir.join("stdout"), &self.stdout).expect("write stdout file");
        fs::write(dir.join("stderr"), &self.stderr).expect("write stderr file");

        let script = dir.join("agent");
        let finish = if self.hang {
            "exec sleep 86400".to_string()
        } else {
            format!("exit {}", self.exit_code)
        };
        // Step payloads live in files next to the script, so no user text is
        // ever embedded in shell source and nothing needs quoting.
        let mut steps = String::new();
        for (index, step) in self.steps.iter().enumerate() {
            match step {
                Step::Write { path, body } => {
                    fs::write(dir.join(format!("step{index}.path")), format!("{path}\n")).expect("write step path");
                    fs::write(dir.join(format!("step{index}.body")), body).expect("write step body");
                    steps.push_str(&format!("write_step {index}\n"));
                }
                Step::Commit { message } => {
                    fs::write(dir.join(format!("step{index}.msg")), message).expect("write step message");
                    steps.push_str(&format!("commit_step {index}\n"));
                }
            }
        }
        let session = match &self.session_id {
            Some(id) => format!("printf '%s\\n' '{{\"session_id\":\"{id}\"}}'\n"),
            None => String::new(),
        };
        // The script locates its own directory via $0, so no path is embedded
        // in shell source and nothing needs quoting.
        let body = format!(
            "#!/bin/sh\n[ -n \"$FAKE_AGENT_PROBE\" ] && exit 0\ndir=${{0%/*}}\nprintf '%s\\0' \"$PWD\" \"$@\" > \"$dir/record\"\n[ -e \"$dir/first\" ] || cp \"$dir/record\" \"$dir/first\"\n{STEP_FUNCTIONS}{steps}{session}cat \"$dir/stdout\"\ncat \"$dir/stderr\" >&2\n{finish}\n",
        );
        fs::write(&script, body).expect("write fake agent script");
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod fake agent");
        wait_until_executable(&script);

        FakeAgent { dir, script }
    }
}

/// Shell functions for [`Step`]s; they read their payload from `$dir/step<N>.*`.
const STEP_FUNCTIONS: &str = concat!(
    "write_step() { IFS= read -r p < \"$dir/step$1.path\" && { case $p in */*) mkdir -p -- \"${p%/*}\";; esac; } && cat \"$dir/step$1.body\" > \"$p\" ",
    "|| { echo \"fake agent: write failed: $p\" >&2; exit 70; }; }\n",
    "commit_step() { export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1; ",
    "git add -A >/dev/null 2>&1 && git -c user.name='Orq Fake Agent' -c user.email=fake-agent@example.invalid ",
    "-c core.hooksPath=/dev/null -c commit.gpgsign=false commit -q --cleanup=verbatim -F \"$dir/step$1.msg\" >/dev/null 2>&1 ",
    "|| { echo 'fake agent: commit failed' >&2; exit 70; }; }\n",
);

/// Another test thread may fork while the script is still open for writing,
/// leaving the file briefly "busy" (ETXTBSY) to exec. Probe until it is free.
pub(crate) fn wait_until_executable(script: &Path) {
    for _ in 0..200 {
        let probe = std::process::Command::new(script)
            .env("FAKE_AGENT_PROBE", "1")
            .output();
        match probe {
            Err(err) if err.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            _ => return,
        }
    }
}

impl Drop for FakeAgent {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}
