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
    serde_json::from_str::<serde_json::Value>(text).map_err(|_| EditError::Malformed)?;
    let span = Scanner { bytes: text.as_bytes(), pos: 0 }
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

fn status_text(status: FeatureStatus) -> &'static str {
    match status {
        FeatureStatus::Draft => "draft",
        FeatureStatus::Pending => "pending",
        FeatureStatus::InProgress => "in_progress",
        FeatureStatus::Completed => "completed",
        FeatureStatus::Abandoned => "abandoned",
    }
}

struct Scanner<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Scanner<'_> {
    /// The byte range of the `status` string of the entry named `name`.
    fn status_span_of(mut self, name: &str) -> Result<Option<(usize, usize)>, EditError> {
        self.skip_ws();
        self.expect(b'[')?;
        // Every entry is scanned, so an ill-shaped one is reported wherever it sits.
        let mut found = None;
        loop {
            self.skip_ws();
            match self.peek() {
                Some(b']') => return Ok(found),
                Some(b',') => self.pos += 1,
                Some(b'{') => {
                    let span = self.entry(name)?;
                    if found.is_some() && span.is_some() {
                        // The loader rejects a repeated featureName, so must the editor.
                        return Err(EditError::Malformed);
                    }
                    found = found.or(span);
                }
                _ => return Err(EditError::Malformed),
            }
        }
    }

    /// Scans one entry object; its status span if its `featureName` is `name`.
    fn entry(&mut self, name: &str) -> Result<Option<(usize, usize)>, EditError> {
        self.expect(b'{')?;
        let mut matches = false;
        let mut status = None;
        let (mut seen_name, mut seen_status) = (false, false);
        loop {
            self.skip_ws();
            match self.peek() {
                Some(b'}') => {
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
            match key.as_str() {
                "featureName" => {
                    if seen_name {
                        return Err(EditError::Malformed);
                    }
                    seen_name = true;
                    matches = self.bytes[start] == b'"' && self.decode(value)? == name;
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
            (true, found) => Ok(found),
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
