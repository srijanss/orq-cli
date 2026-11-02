//! The desktop actions behind the Runs view: opening a link and copying text. The program
//! lookup is a parameter so the success path, a missing tool and a failing tool are all
//! testable without touching the machine's real tools.

use std::path::PathBuf;

/// Finds a program by name, `None` when it is not installed.
pub(crate) type Lookup<'a> = &'a dyn Fn(&str) -> Option<PathBuf>;

/// Opens `url` with the first desktop opener that works.
pub(crate) fn open_url_with(url: &str, lookup: Lookup<'_>) -> Result<String, String> {
    for name in ["xdg-open", "open"] {
        let Some(program) = lookup(name) else { continue };
        let status = std::process::Command::new(program)
            .arg(url)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        if matches!(status, Ok(status) if status.success()) {
            return Ok(format!("Opened {}", crate::printable(url)));
        }
    }
    Err(format!("cannot open the link, no opener worked: {}", crate::printable(url)))
}

/// Puts `text` on the clipboard with the first clipboard tool that works.
pub(crate) fn copy_text_with(text: &str, lookup: Lookup<'_>) -> Result<String, String> {
    use std::io::Write;
    let tools: [(&str, &[&str]); 3] = [("wl-copy", &[]), ("xclip", &["-selection", "clipboard"]), ("pbcopy", &[])];
    for (name, args) in tools {
        let Some(program) = lookup(name) else { continue };
        let Ok(mut child) = std::process::Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        else {
            continue;
        };
        let wrote = child.stdin.take().is_some_and(|mut stdin| stdin.write_all(text.as_bytes()).is_ok());
        if matches!(child.wait(), Ok(status) if status.success()) && wrote {
            return Ok(format!("Copied {}", crate::printable(text)));
        }
    }
    Err(format!(
        "cannot copy: no clipboard tool worked (wl-copy, xclip, pbcopy); the link is {}",
        crate::printable(text)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    fn scratch(tag: &str) -> PathBuf {
        let dir = orq_test_support::process_temp_dir().join(format!("orq-desktop-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn lookup(tools: HashMap<&'static str, PathBuf>) -> impl Fn(&str) -> Option<PathBuf> {
        move |name| tools.get(name).cloned()
    }

    #[test]
    fn an_opener_that_works_is_given_the_url_and_the_result_says_it_opened() {
        let dir = scratch("open-ok");
        let out = dir.join("arg");
        let opener = script(&dir, "xdg-open", &format!("printf '%s' \"$1\" > '{}'", out.display()));
        let find = lookup(HashMap::from([("xdg-open", opener)]));

        let result = open_url_with("https://example.test/pull/7", &find);

        assert_eq!(result, Ok("Opened https://example.test/pull/7".to_string()));
        assert_eq!(std::fs::read_to_string(out).unwrap(), "https://example.test/pull/7");
    }

    #[test]
    fn a_failing_first_opener_falls_back_to_the_next_and_no_opener_names_the_link() {
        let dir = scratch("open-fallback");
        let broken = script(&dir, "xdg-open", "exit 3");
        let marker = dir.join("second");
        let second = script(&dir, "open", &format!("touch '{}'", marker.display()));

        let fell_back = open_url_with("https://x.test/1", &lookup(HashMap::from([("xdg-open", broken.clone()), ("open", second)])));
        assert!(fell_back.is_ok(), "{fell_back:?}");
        assert!(marker.exists());

        let all_fail = open_url_with("https://x.test/1", &lookup(HashMap::from([("xdg-open", broken)])));
        let message = all_fail.unwrap_err();
        assert!(message.contains("no opener worked") && message.contains("https://x.test/1"), "{message}");

        let none = open_url_with("https://x.test/1", &lookup(HashMap::new()));
        assert!(none.unwrap_err().contains("no opener worked"));
    }

    #[test]
    fn a_clipboard_tool_that_works_receives_the_text_on_stdin() {
        let dir = scratch("copy-ok");
        let out = dir.join("clip");
        let tool = script(&dir, "xclip", &format!("cat > '{}'", out.display()));
        let find = lookup(HashMap::from([("xclip", tool)]));

        let result = copy_text_with("https://example.test/pull/7", &find);

        assert_eq!(result, Ok("Copied https://example.test/pull/7".to_string()));
        assert_eq!(std::fs::read_to_string(out).unwrap(), "https://example.test/pull/7");
    }

    #[test]
    fn a_failing_or_missing_clipboard_tool_is_skipped_and_when_none_works_the_link_is_in_the_message() {
        let dir = scratch("copy-fail");
        let broken = script(&dir, "wl-copy", "cat > /dev/null; exit 1");
        let out = dir.join("clip");
        let good = script(&dir, "pbcopy", &format!("cat > '{}'", out.display()));

        let fell_back = copy_text_with("link-1", &lookup(HashMap::from([("wl-copy", broken.clone()), ("pbcopy", good)])));
        assert_eq!(fell_back, Ok("Copied link-1".to_string()));
        assert_eq!(std::fs::read_to_string(out).unwrap(), "link-1");

        let failed = copy_text_with("link-2", &lookup(HashMap::from([("wl-copy", broken)]))).unwrap_err();
        assert!(failed.contains("no clipboard tool worked") && failed.contains("link-2"), "{failed}");
        let missing = copy_text_with("link-3", &lookup(HashMap::new())).unwrap_err();
        assert!(missing.contains("no clipboard tool worked") && missing.contains("link-3"), "{missing}");
    }
}
