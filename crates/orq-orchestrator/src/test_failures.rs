//! The tests a run of the suite failed, picked out of what it printed, each with the end of its own output, so a
//! failed Accept or verify gate can say which tests failed and why. Shared by both.

/// The most failed tests kept by name; the rest are only counted.
pub const FAILED_TESTS_KEPT: usize = 50;

/// One failed test: its name as the runner printed it, and the end of its own output, redacted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedTest {
    pub name: String,
    pub output: String,
}

/// The failed tests found, in the order the output first named them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TestFailures {
    pub tests: Vec<FailedTest>,
    /// How many more failed than are kept.
    pub more: usize,
}

impl TestFailures {
    /// "3 tests failed: a, b, …", or `None` when no test was named.
    pub fn headline(&self) -> Option<String> {
        let total = self.tests.len() + self.more;
        let first = self.tests.first()?;
        let mut line = format!("{total} {} failed: {}", if total == 1 { "test" } else { "tests" }, first.name);
        if let Some(second) = self.tests.get(1) {
            line.push_str(", ");
            line.push_str(&second.name);
        }
        if total > 2 {
            line.push_str(", …");
        }
        Some(line)
    }

    /// What an event records of them: `failed_tests` (each `{name, output}`) and `failed_tests_more`.
    pub fn add_to(&self, payload: &mut serde_json::Value) {
        let tests: Vec<_> = self.tests.iter().map(|test| serde_json::json!({ "name": test.name, "output": test.output })).collect();
        payload["failed_tests"] = serde_json::Value::Array(tests);
        payload["failed_tests_more"] = self.more.into();
    }
}

/// Where in the output a line is.
enum At {
    Elsewhere,
    /// In a nextest `STDOUT:`/`STDERR:` block of this test: libtest's own lists inside it are its output.
    NextestBlock(String),
    /// In a libtest `---- <test> stdout ----` block.
    LibtestBlock(String),
    /// In a libtest `failures:` list, whose indented lines name tests.
    FailuresList,
}

/// The failed tests named in `output`: nextest's `FAIL [ … ] (n/m) <binary> <test>` lines and `STDOUT:`/`STDERR:`
/// blocks, libtest's `failures:` list and `---- <test> stdout ----` blocks (doc tests' included).
pub fn find_test_failures(output: &str) -> TestFailures {
    let mut found = Found::default();
    let mut at = At::Elsewhere;
    for line in output.lines() {
        if let Some(name) = nextest_block_start(line) {
            found.name(&name);
            at = At::NextestBlock(name);
            continue;
        }
        if let Some(status) = nextest_status(line) {
            at = At::Elsewhere;
            match status {
                Status::Failed(name) => found.name(&name),
                Status::PassedOnRetry(name) => found.unname(&name),
                Status::Other => {}
            }
            continue;
        }
        if let At::NextestBlock(name) = &at {
            found.add_output(name, line);
            continue;
        }
        if let Some(name) = libtest_block_start(line) {
            found.name(&name);
            at = At::LibtestBlock(name);
            continue;
        }
        if line.trim_end() == "failures:" {
            at = At::FailuresList;
            continue;
        }
        if line.starts_with("test result:") {
            at = At::Elsewhere;
            continue;
        }
        match &at {
            At::LibtestBlock(name) => found.add_output(name, line),
            At::FailuresList if line.trim().is_empty() => {}
            At::FailuresList if line.starts_with(char::is_whitespace) => found.name(line.trim()),
            At::FailuresList => at = At::Elsewhere,
            At::Elsewhere | At::NextestBlock(_) => {}
        }
    }
    found.finish()
}

/// The test a libtest `---- <test> stdout ----` header opens the output of.
fn libtest_block_start(line: &str) -> Option<String> {
    let name = line.trim_end().strip_prefix("---- ")?.strip_suffix(" stdout ----")?.trim();
    (!name.is_empty()).then(|| name.to_string())
}

#[derive(Default)]
struct Found {
    names: Vec<String>,
    outputs: std::collections::HashMap<String, String>,
}

impl Found {
    fn name(&mut self, name: &str) {
        if !self.names.iter().any(|known| known == name) {
            self.names.push(name.to_string());
        }
    }

    /// A test named as failed on a try that then passed on a later one.
    fn unname(&mut self, name: &str) {
        self.names.retain(|known| known != name);
        self.outputs.remove(name);
    }

    fn add_output(&mut self, name: &str, line: &str) {
        let output = self.outputs.entry(name.to_string()).or_default();
        output.push_str(line);
        output.push('\n');
    }

    fn finish(mut self) -> TestFailures {
        let more = self.names.len().saturating_sub(FAILED_TESTS_KEPT);
        let tests = self
            .names
            .iter()
            .take(FAILED_TESTS_KEPT)
            .map(|name| FailedTest { name: orq_core::redact::redact(name), output: kept_output(&self.outputs.remove(name).unwrap_or_default()) })
            .collect();
        TestFailures { tests, more }
    }
}

/// The most of a failed test's own output kept, in bytes.
pub const TEST_OUTPUT_KEPT: usize = 4000;
/// The most of that given to the panic and `left`/`right` lines when the output is cut.
const KEY_LINES_KEPT: usize = 1500;
const CUT: &str = "\n…\n";

/// A failed test's output, redacted whole before anything is cut. One too long keeps its panic line, the line
/// after it (the message) and its `left`/`right` lines, then as much of its end as fits.
fn kept_output(text: &str) -> String {
    let text = orq_core::redact::redact(text.trim());
    if text.len() <= TEST_OUTPUT_KEPT {
        return text;
    }
    let lines: Vec<&str> = text.lines().collect();
    let mut key = String::new();
    for (at, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        let after_panic = at > 0 && lines[at - 1].contains("panicked at");
        if line.contains("panicked at") || after_panic || trimmed.starts_with("left:") || trimmed.starts_with("right:") {
            if key.len() + line.len() + 1 > KEY_LINES_KEPT {
                break;
            }
            key.push_str(line);
            key.push('\n');
        }
    }
    let key = key.trim_end();
    let budget = TEST_OUTPUT_KEPT - key.len() - CUT.len();
    let mut from = text.len() - budget;
    while !text.is_char_boundary(from) {
        from += 1;
    }
    let mut end = &text[from..];
    // Only whole lines of the end are kept.
    if let Some(newline) = end.find('\n') {
        end = &end[newline + 1..];
    }
    if key.is_empty() {
        return end.to_string();
    }
    format!("{key}{CUT}{end}")
}

/// The test a nextest `STDOUT:`/`STDERR:` header opens the output of: `--- STDOUT: <test> ---` or `──── STDERR: <test>`.
fn nextest_block_start(line: &str) -> Option<String> {
    let rest = line.trim().trim_start_matches(['-', '─']).trim_start();
    if rest.len() == line.trim().len() {
        return None;
    }
    let name = rest.strip_prefix("STDOUT:").or_else(|| rest.strip_prefix("STDERR:"))?;
    let name = name.trim().trim_end_matches(['-', '─']).trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// What a nextest status line says of its test.
enum Status {
    /// `FAIL [ … ] (n/m) <test>`, or `TRY n FAIL` for a try that may yet pass.
    Failed(String),
    /// `TRY n PASS` or `FLAKY`: a test that failed a try passed on a later one.
    PassedOnRetry(String),
    /// Any other status, a summary or a separator.
    Other,
}

/// A nextest status line (`PASS [ … ]`, `FAIL [ … ] (n/m) <test>`, `TRY 2 PASS [ … ]`, `Summary [ … ]`, a separator).
fn nextest_status(line: &str) -> Option<Status> {
    let trimmed = line.trim();
    if trimmed.len() >= 3 && trimmed.chars().all(|c| c == '─' || c == '-') && !trimmed.starts_with("---- ") {
        return Some(Status::Other);
    }
    let (word, rest) = trimmed.split_once(" [")?;
    let status = word.split(' ').next_back()?;
    let is_status = word == "Summary" || (!word.is_empty() && word.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == ' '));
    if !is_status {
        return None;
    }
    let retried = word.starts_with("TRY ");
    let passed_on_retry = (retried && status == "PASS") || status == "FLAKY" || status == "FLKY";
    if status != "FAIL" && !passed_on_retry {
        return Some(Status::Other);
    }
    let rest = rest.split_once(']').map(|(_, rest)| rest.trim_start())?;
    let rest = match rest.strip_prefix('(') {
        Some(counted) => counted.split_once(')').map_or(counted, |(_, after)| after),
        None => rest,
    };
    let name = rest.trim();
    if name.is_empty() {
        return Some(Status::Other);
    }
    Some(if passed_on_retry { Status::PassedOnRetry(name.to_string()) } else { Status::Failed(name.to_string()) })
}
