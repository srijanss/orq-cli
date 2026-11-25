use orq_core::FeatureStatus;

/// Why a status could not be edited into a ledger text.
#[derive(Debug, PartialEq, Eq)]
pub enum EditError {
    /// The text is not a JSON array of entries, or the entry has no string `status`.
    Malformed,
    /// No entry carries the requested `featureName`.
    UnknownFeature,
}

/// Returns `text` with the `status` of the entry named `feature_name` replaced
/// by `status`. Only that one string literal changes: every other byte of the
/// ledger (unknown fields, key and entry order, indentation, trailing newline)
/// is carried over verbatim.
pub fn set_entry_status(
    text: &str,
    feature_name: &str,
    status: FeatureStatus,
) -> Result<String, EditError> {
    // The scanner below assumes well-formed JSON.
    serde_json::from_str::<serde::de::IgnoredAny>(text).map_err(|_| EditError::Malformed)?;
    let span = Scanner { bytes: text.as_bytes(), pos: 0, names: Default::default(), captured: Vec::new() }
        .status_span_of(feature_name)?
        .ok_or(EditError::UnknownFeature)?;
    let mut edited = String::with_capacity(text.len() + 16);
    edited.push_str(&text[..span.0]);
    edited.push('"');
    edited.push_str(status_text(status));
    edited.push('"');
    edited.push_str(&text[span.1..]);
    Ok(edited)
}

/// [`set_entry_status`] for every entry named in `names`, found in a single pass over `text`.
/// Every name must be in the ledger; nothing else changes.
pub fn set_entry_statuses(text: &str, names: &[String], status: FeatureStatus) -> Result<String, EditError> {
    serde_json::from_str::<serde::de::IgnoredAny>(text).map_err(|_| EditError::Malformed)?;
    let wanted: std::collections::HashSet<&str> = names.iter().map(String::as_str).collect();
    let spans = Scanner { bytes: text.as_bytes(), pos: 0, names: Default::default(), captured: Vec::new() }
        .scan_all(&|name| wanted.contains(name))?;
    // Entry names are unique, so each wanted name matches at most one entry.
    if spans.len() != wanted.len() {
        return Err(EditError::UnknownFeature);
    }
    let mut edited = String::with_capacity(text.len() + 16 * spans.len());
    let mut copied = 0;
    for (start, end) in spans {
        edited.push_str(&text[copied..start]);
        edited.push('"');
        edited.push_str(status_text(status));
        edited.push('"');
        copied = end;
    }
    edited.push_str(&text[copied..]);
    Ok(edited)
}

/// Returns `text` with the given top-level members of the entry named
/// `feature_name` set to the given JSON values: an existing member's value is
/// replaced in place, a missing one is added after the last member. Every
/// other byte is carried over verbatim.
pub(crate) fn set_entry_fields(
    text: &str,
    feature_name: &str,
    members: &[(&str, String)],
) -> Result<String, EditError> {
    serde_json::from_str::<serde::de::IgnoredAny>(text).map_err(|_| EditError::Malformed)?;
    let mut scanner = Scanner { bytes: text.as_bytes(), pos: 0, names: Default::default(), captured: Vec::new() };
    scanner.scan(feature_name)?.ok_or(EditError::UnknownFeature)?;
    let captured = scanner.captured.pop().ok_or(EditError::UnknownFeature)?;
    let mut ops = field_ops(text, &captured, members, false);
    ops.sort_by_key(|(start, _, _)| std::cmp::Reverse(*start));
    let mut edited = text.to_string();
    for (start, end, replacement) in ops {
        edited.replace_range(start..end, &replacement);
    }
    Ok(edited)
}

/// [`set_entry_fields`] for every entry named in `names`, found in a single pass over `text`. A member
/// added to an entry laid out one member per line goes on a line of its own, indented as the member
/// before it. Every name must be in the ledger; nothing else changes.
pub(crate) fn set_entries_fields(text: &str, names: &[String], members: &[(&str, String)]) -> Result<String, EditError> {
    serde_json::from_str::<serde::de::IgnoredAny>(text).map_err(|_| EditError::Malformed)?;
    let wanted: std::collections::HashSet<&str> = names.iter().map(String::as_str).collect();
    let mut scanner = Scanner { bytes: text.as_bytes(), pos: 0, names: Default::default(), captured: Vec::new() };
    scanner.scan_all(&|name| wanted.contains(name))?;
    // Entry names are unique, so each wanted name matches at most one entry.
    if scanner.captured.len() != wanted.len() {
        return Err(EditError::UnknownFeature);
    }
    // Entries come in text order and each entry's ops lie inside it, so sorting the ops orders them all.
    let mut ops: Vec<(usize, usize, String)> =
        scanner.captured.iter().flat_map(|captured| field_ops(text, captured, members, true)).collect();
    ops.sort_by_key(|(start, _, _)| *start);
    let mut edited = String::with_capacity(text.len() + ops.iter().map(|(_, _, r)| r.len()).sum::<usize>());
    let mut copied = 0;
    for (start, end, replacement) in ops {
        edited.push_str(&text[copied..start]);
        edited.push_str(&replacement);
        copied = end;
    }
    edited.push_str(&text[copied..]);
    Ok(edited)
}

/// The edits, as (start, end, replacement), that set `members` in the entry `captured`: an existing
/// member's value is replaced in place, missing ones are added after the last member. With `keep_layout`
/// an addition to an entry whose last member ends its line goes on a line of its own.
fn field_ops(text: &str, captured: &Captured, members: &[(&str, String)], keep_layout: bool) -> Vec<(usize, usize, String)> {
    let mut ops: Vec<(usize, usize, String)> = Vec::new();
    let last_end = captured.fields.last().map(|(_, (_, end))| *end);
    let separator = match last_end {
        Some(end) if keep_layout && text[end..].starts_with('\n') => {
            let line = &text[text[..end].rfind('\n').map_or(0, |i| i + 1)..end];
            Some(format!("\n{}", &line[..line.len() - line.trim_start().len()]))
        }
        _ => None,
    };
    let mut additions = String::new();
    for (key, value) in members {
        match captured.fields.iter().find(|(k, _)| k == key) {
            Some((_, (start, end))) => ops.push((*start, *end, value.clone())),
            None => match &separator {
                Some(separator) => additions.push_str(&format!(",{separator}{}: {value}", serde_json::Value::from(*key))),
                None => additions.push_str(&format!(",{}:{value}", serde_json::Value::from(*key))),
            },
        }
    }
    if !additions.is_empty() {
        let insert_at = match last_end {
            Some(end) => end,
            None => {
                // `{}`: no leading comma
                additions.remove(0);
                text[..captured.close].trim_end().len()
            }
        };
        ops.push((insert_at, insert_at, additions));
    }
    ops
}

fn status_text(status: FeatureStatus) -> &'static str {
    match status {
        FeatureStatus::Draft => "draft",
        FeatureStatus::Pending => "pending",
        FeatureStatus::InProgress => "in_progress",
        FeatureStatus::AwaitingDelivery => "awaiting_delivery",
        FeatureStatus::Completed => "completed",
        FeatureStatus::Abandoned => "abandoned",
    }
}

struct Scanner<'a> {
    bytes: &'a [u8],
    pos: usize,
    /// Every entry name met so far, so a repeat is caught whichever entry is edited.
    names: std::collections::HashSet<String>,
    /// Member value spans and closing brace of each entry that matched, in text order.
    captured: Vec<Captured>,
}

/// Where the members of one entry object sit in the text.
struct Captured {
    fields: Vec<(String, (usize, usize))>,
    /// Offset of the closing `}`.
    close: usize,
}

impl Scanner<'_> {
    /// The byte range of the `status` string of the entry named `name`.
    fn status_span_of(mut self, name: &str) -> Result<Option<(usize, usize)>, EditError> {
        self.scan(name)
    }

    fn scan(&mut self, name: &str) -> Result<Option<(usize, usize)>, EditError> {
        Ok(self.scan_all(&|entry_name| entry_name == name)?.pop())
    }

    /// The status spans, in text order, of every entry whose `featureName` is `wanted`.
    fn scan_all(&mut self, wanted: &dyn Fn(&str) -> bool) -> Result<Vec<(usize, usize)>, EditError> {
        self.skip_ws();
        self.expect(b'[')?;
        // Every entry is scanned, so an ill-shaped one is reported wherever it sits; a repeated
        // featureName, which the loader rejects, is caught through `names`.
        let mut found = Vec::new();
        loop {
            self.skip_ws();
            match self.peek() {
                Some(b']') => return Ok(found),
                Some(b',') => self.pos += 1,
                Some(b'{') => found.extend(self.entry(wanted)?),
                _ => return Err(EditError::Malformed),
            }
        }
    }

    /// Scans one entry object; its status span if its `featureName` is `wanted`.
    fn entry(&mut self, wanted: &dyn Fn(&str) -> bool) -> Result<Option<(usize, usize)>, EditError> {
        self.expect(b'{')?;
        let mut matches = false;
        let mut status = None;
        let mut fields: Vec<(String, (usize, usize))> = Vec::new();
        let close;
        let (mut seen_name, mut seen_status) = (false, false);
        loop {
            self.skip_ws();
            match self.peek() {
                Some(b'}') => {
                    close = self.pos;
                    self.pos += 1;
                    break;
                }
                Some(b',') => {
                    self.pos += 1;
                    continue;
                }
                _ => {}
            }
            let key = self.string()?;
            self.skip_ws();
            self.expect(b':')?;
            self.skip_ws();
            let start = self.pos;
            self.skip_value()?;
            let value = (start, self.pos);
            fields.push((key.clone(), value));
            match key.as_str() {
                "featureName" => {
                    if seen_name {
                        return Err(EditError::Malformed);
                    }
                    seen_name = true;
                    if self.bytes[start] == b'"' {
                        let entry_name = self.decode(value)?;
                        matches = wanted(&entry_name);
                        if !self.names.insert(entry_name) {
                            return Err(EditError::Malformed);
                        }
                    }
                }
                "status" => {
                    if seen_status {
                        return Err(EditError::Malformed);
                    }
                    seen_status = true;
                    if self.bytes[start] == b'"' {
                        status = Some(value);
                    }
                }
                _ => {}
            }
        }
        match (matches, status) {
            (true, None) => Err(EditError::Malformed),
            (true, found) => {
                self.captured.push(Captured { fields, close });
                Ok(found)
            }
            (false, _) => Ok(None),
        }
    }

    fn decode(&self, (start, end): (usize, usize)) -> Result<String, EditError> {
        let literal = std::str::from_utf8(&self.bytes[start..end]).map_err(|_| EditError::Malformed)?;
        serde_json::from_str(literal).map_err(|_| EditError::Malformed)
    }

    fn string(&mut self) -> Result<String, EditError> {
        let start = self.pos;
        self.skip_string()?;
        self.decode((start, self.pos))
    }

    fn skip_value(&mut self) -> Result<(), EditError> {
        match self.peek() {
            Some(b'"') => self.skip_string(),
            Some(b'{') | Some(b'[') => {
                let mut depth = 0usize;
                while let Some(byte) = self.peek() {
                    match byte {
                        b'"' => {
                            self.skip_string()?;
                            continue;
                        }
                        b'{' | b'[' => depth += 1,
                        b'}' | b']' => depth -= 1,
                        _ => {}
                    }
                    self.pos += 1;
                    if depth == 0 {
                        return Ok(());
                    }
                }
                Err(EditError::Malformed)
            }
            Some(_) => {
                while let Some(byte) = self.peek() {
                    if matches!(byte, b',' | b'}' | b']') || byte.is_ascii_whitespace() {
                        break;
                    }
                    self.pos += 1;
                }
                Ok(())
            }
            None => Err(EditError::Malformed),
        }
    }

    fn skip_string(&mut self) -> Result<(), EditError> {
        self.expect(b'"')?;
        while let Some(byte) = self.peek() {
            self.pos += 1;
            match byte {
                b'\\' => self.pos += 1,
                b'"' => return Ok(()),
                _ => {}
            }
        }
        Err(EditError::Malformed)
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while self.peek().is_some_and(|byte| byte.is_ascii_whitespace()) {
            self.pos += 1;
        }
    }

    fn expect(&mut self, byte: u8) -> Result<(), EditError> {
        if self.peek() == Some(byte) {
            self.pos += 1;
            Ok(())
        } else {
            Err(EditError::Malformed)
        }
    }
}
