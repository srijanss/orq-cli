use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

/// One recorded invocation of the fake `gh` or `glab`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderCall {
    /// `gh` or `glab`: which executable name was run.
    pub program: String,
    pub cwd: PathBuf,
    pub args: Vec<String>,
}

/// A scriptable stand-in for the `gh` and `glab` CLIs. Both names live in one
/// `bin` directory and share one call log, so a test sees every provider call
/// in order. A call is answered by the scripted reply whose argv prefix is the
/// longest match; a call nothing matches exits 64 and says so on stderr.
pub struct FakeProvider {
    root: PathBuf,
}

struct Scripted {
    prefix: Vec<String>,
    stdout: String,
    stderr: String,
    exit_code: i32,
}

pub struct FakeProviderBuilder {
    replies: Vec<Scripted>,
}

impl FakeProvider {
    pub fn builder() -> FakeProviderBuilder {
        FakeProviderBuilder { replies: Vec::new() }
    }

    /// Directory holding the fake `gh` and `glab`; put it on `PATH`.
    pub fn bin_dir(&self) -> PathBuf {
        self.root.join("bin")
    }

    pub fn gh(&self) -> PathBuf {
        self.bin_dir().join("gh")
    }

    pub fn glab(&self) -> PathBuf {
        self.bin_dir().join("glab")
    }

    /// Every call so far, in the order the calls claimed their slot.
    pub fn calls(&self) -> Vec<ProviderCall> {
        let Ok(entries) = fs::read_dir(self.root.join("data").join("calls")) else {
            return Vec::new();
        };
        let mut slots: Vec<(u64, PathBuf)> = entries
            .filter_map(|entry| {
                let entry = entry.ok()?;
                let slot = entry.file_name().to_str()?.parse().ok()?;
                Some((slot, entry.path()))
            })
            .collect();
        slots.sort();
        slots
            .into_iter()
            .filter_map(|(_, path)| parse_call(&fs::read(path).ok()?))
            .collect()
    }
}

/// One call's record: `argc NUL program NUL cwd NUL arg NUL ... END NUL`.
/// The trailing `END` is written last, so a record cut off anywhere (a call
/// still writing, or killed mid-write) lacks it and is skipped, never parsed
/// as a shorter or truncated call.
fn parse_call(raw: &[u8]) -> Option<ProviderCall> {
    let body = raw.strip_suffix(&[0])?;
    let mut fields = body.split(|byte| *byte == 0).map(|field| String::from_utf8_lossy(field).into_owned());
    let count: usize = fields.next()?.parse().ok()?;
    let program = fields.next()?;
    let cwd = PathBuf::from(fields.next()?);
    let mut args: Vec<String> = fields.collect();
    if args.pop().as_deref() != Some("END") || args.len() != count {
        return None;
    }
    Some(ProviderCall { program, cwd, args })
}

impl FakeProviderBuilder {
    /// Answer calls starting with `prefix` by printing `stdout` and exiting 0.
    pub fn reply(self, prefix: &[&str], stdout: &str) -> Self {
        self.script(prefix, stdout, "", 0)
    }

    /// Answer calls starting with `prefix` by printing `stderr` and exiting `exit_code`.
    pub fn fail(self, prefix: &[&str], exit_code: i32, stderr: &str) -> Self {
        self.script(prefix, "", stderr, exit_code)
    }

    fn script(mut self, prefix: &[&str], stdout: &str, stderr: &str, exit_code: i32) -> Self {
        assert!(!prefix.is_empty(), "a scripted prefix needs at least one word");
        assert!(
            prefix.iter().all(|word| !word.is_empty() && !word.contains('\n')),
            "scripted prefix words must be non-empty single lines"
        );
        self.replies.push(Scripted {
            prefix: prefix.iter().map(|word| word.to_string()).collect(),
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
            exit_code,
        });
        self
    }

    pub fn build(mut self) -> FakeProvider {
        let root = crate::private_dir("orq-fake-provider");
        let bin = root.join("bin");
        let replies = root.join("data").join("replies");
        fs::create_dir_all(&bin).expect("create fake provider bin dir");
        fs::create_dir_all(&replies).expect("create fake provider replies dir");
        fs::create_dir_all(root.join("data").join("calls")).expect("create fake provider calls dir");

        // The script tries replies in directory order, so the most specific
        // prefix must come first; equal lengths keep registration order.
        self.replies.sort_by_key(|reply| std::cmp::Reverse(reply.prefix.len()));
        for (index, reply) in self.replies.iter().enumerate() {
            let dir = replies.join(format!("{index:04}"));
            fs::create_dir(&dir).expect("create reply dir");
            fs::write(dir.join("argv"), format!("{}\n", reply.prefix.join("\n"))).expect("write reply argv");
            fs::write(dir.join("stdout"), &reply.stdout).expect("write reply stdout");
            fs::write(dir.join("stderr"), &reply.stderr).expect("write reply stderr");
            fs::write(dir.join("code"), reply.exit_code.to_string()).expect("write reply code");
        }

        // The script locates its data via $0, so no path is embedded in shell
        // source and nothing needs quoting. Each call claims its own numbered
        // file (noclobber makes the claim atomic), so concurrent calls never
        // write to the same file and their records cannot interleave. A failed
        // claim retries only when the slot exists; any other failure (such as
        // a removed calls dir) exits 70 instead of spinning.
        let body = "#!/bin/sh\n\
            [ -n \"$FAKE_AGENT_PROBE\" ] && exit 0\n\
            bin=${0%/*}\n\
            data=$bin/../data\n\
            set -C\n\
            n=0\n\
            until { true > \"$data/calls/$n\"; } 2>/dev/null; do\n\
            \x20 [ -e \"$data/calls/$n\" ] || { echo \"fake provider: cannot record call in $data/calls\" >&2; exit 70; }\n\
            \x20 n=$((n+1))\n\
            \x20 [ \"$n\" -le 100000 ] || { echo \"fake provider: cannot record call, no free slot\" >&2; exit 70; }\n\
            done\n\
            printf '%s\\0' \"$#\" \"${0##*/}\" \"$PWD\" \"$@\" END >> \"$data/calls/$n\"\n\
            for r in \"$data\"/replies/*; do\n\
            \x20 [ -d \"$r\" ] || continue\n\
            \x20 ok=1; i=0\n\
            \x20 while IFS= read -r want; do\n\
            \x20   i=$((i+1))\n\
            \x20   eval \"have=\\${$i-}\"\n\
            \x20   [ \"$have\" = \"$want\" ] || { ok=0; break; }\n\
            \x20 done < \"$r/argv\"\n\
            \x20 if [ \"$ok\" = 1 ]; then\n\
            \x20   cat \"$r/stdout\"; cat \"$r/stderr\" >&2\n\
            \x20   exit \"$(cat \"$r/code\")\"\n\
            \x20 fi\n\
            done\n\
            echo \"fake provider: no scripted reply for: $*\" >&2\n\
            exit 64\n";
        for name in ["gh", "glab"] {
            let script = bin.join(name);
            fs::write(&script, body).expect("write fake provider script");
            fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod fake provider");
            crate::fake_agent::wait_until_executable(&script);
        }

        FakeProvider { root }
    }
}

impl Drop for FakeProvider {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
