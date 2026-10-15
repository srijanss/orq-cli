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
}

impl FakeAgent {
    pub fn builder() -> FakeAgentBuilder {
        FakeAgentBuilder {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: 0,
            hang: false,
        }
    }

    pub fn path(&self) -> &Path {
        &self.script
    }

    /// The cwd and argv of the most recent invocation, if it was ever run.
    pub fn last_run(&self) -> Option<Run> {
        let raw = fs::read(self.dir.join("record")).ok()?;
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
        // The script locates its own directory via $0, so no path is embedded
        // in shell source and nothing needs quoting.
        let body = format!(
            "#!/bin/sh\n[ -n \"$FAKE_AGENT_PROBE\" ] && exit 0\ndir=${{0%/*}}\nprintf '%s\\0' \"$PWD\" \"$@\" > \"$dir/record\"\ncat \"$dir/stdout\"\ncat \"$dir/stderr\" >&2\n{finish}\n",
        );
        fs::write(&script, body).expect("write fake agent script");
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod fake agent");
        wait_until_executable(&script);

        FakeAgent { dir, script }
    }
}

/// Another test thread may fork while the script is still open for writing,
/// leaving the file briefly "busy" (ETXTBSY) to exec. Probe until it is free.
fn wait_until_executable(script: &Path) {
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
