//! What an agent session used, recorded as `session.usage` when it ends: read from the agent CLI's
//! JSON result for a session orq ran unattended, and summed from the transcript its hooks named for one
//! on the terminal. Usage is never guessed: when there is nothing to read, nothing is recorded.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use serde_json::Value;

/// How much of the end of a session's output is searched for its JSON result.
const MAX_RESULT_BYTES: u64 = 4 * 1024 * 1024;

/// The tokens and turns one session used. No money figure is ever kept.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub turns: u64,
    /// The model that wrote the most.
    pub model: String,
    /// What each model used, by model name.
    pub models: Vec<ModelUsage>,
}

/// The tokens one model of a session used.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelUsage {
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
}

impl SessionUsage {
    /// The `session.usage` payload, with `duration_secs` as orq measured it.
    pub fn payload(&self, duration_secs: u64) -> Value {
        let models: Vec<Value> = self
            .models
            .iter()
            .map(|used| {
                serde_json::json!({
                    "model": used.model,
                    "input_tokens": used.input_tokens,
                    "output_tokens": used.output_tokens,
                    "cache_read_tokens": used.cache_read_tokens,
                    "cache_write_tokens": used.cache_write_tokens,
                })
            })
            .collect();
        serde_json::json!({
            "input_tokens": self.input_tokens,
            "output_tokens": self.output_tokens,
            "cache_read_tokens": self.cache_read_tokens,
            "cache_write_tokens": self.cache_write_tokens,
            "turns": self.turns,
            "duration_secs": duration_secs,
            "model": self.model,
            "models": models,
        })
    }
}

fn count(usage: &Value, key: &str) -> u64 {
    usage.get(key).and_then(Value::as_u64).unwrap_or(0)
}

/// The model that wrote the most of `models`; empty when there are none.
fn dominant(models: &[ModelUsage]) -> String {
    models
        .iter()
        .max_by_key(|used| used.output_tokens)
        .map(|used| used.model.clone())
        .unwrap_or_default()
}

/// The usage in the last JSON result line of `output` (`{"type":"result", "usage": ...}`), if any.
pub fn usage_from_result(output: &str) -> Option<SessionUsage> {
    output.lines().rev().find_map(|line| {
        let result: Value = serde_json::from_str(line.trim()).ok()?;
        if result.get("type")?.as_str()? != "result" {
            return None;
        }
        let usage = result.get("usage")?;
        let mut models: Vec<ModelUsage> = result
            .get("modelUsage")
            .and_then(Value::as_object)
            .map(|models| {
                models
                    .iter()
                    .map(|(model, used)| ModelUsage {
                        model: model.clone(),
                        input_tokens: count(used, "inputTokens"),
                        output_tokens: count(used, "outputTokens"),
                        cache_read_tokens: count(used, "cacheReadInputTokens"),
                        cache_write_tokens: count(used, "cacheCreationInputTokens"),
                    })
                    .collect()
            })
            .unwrap_or_default();
        models.sort_by(|a, b| a.model.cmp(&b.model));
        Some(SessionUsage {
            input_tokens: count(usage, "input_tokens"),
            output_tokens: count(usage, "output_tokens"),
            cache_read_tokens: count(usage, "cache_read_input_tokens"),
            cache_write_tokens: count(usage, "cache_creation_input_tokens"),
            turns: result.get("num_turns").and_then(Value::as_u64).unwrap_or(0),
            model: dominant(&models),
            models,
        })
    })
}

/// The run file the session's hooks write its transcript's path to.
pub(crate) const TRANSCRIPT_FILE: &str = "session-transcript";

/// The longest transcript read; a longer one is read only this far.
const MAX_TRANSCRIPT_BYTES: u64 = 256 * 1024 * 1024;

/// Keeps where the session's transcript is, from the JSON a Claude hook is given on stdin
/// (`transcript_path`), so the usage can be read from it when the session ends.
pub fn remember_transcript(
    store: &crate::RunStore,
    run_id: &str,
    hook_input: &str,
) -> Result<(), crate::StoreError> {
    let Ok(input) = serde_json::from_str::<Value>(hook_input) else {
        return Ok(());
    };
    let Some(path) = input.get("transcript_path").and_then(Value::as_str) else {
        return Ok(());
    };
    if !Path::new(path).is_absolute() {
        return Ok(());
    }
    store.write_run_file(run_id, TRANSCRIPT_FILE, path.as_bytes())?;
    Ok(())
}

/// Seconds since the epoch of an RFC 3339 UTC time, with or without a fraction of a second.
fn transcript_time(text: &str) -> Option<u64> {
    let whole = match text.split_once('.') {
        Some((whole, fraction)) if fraction.ends_with('Z') => format!("{whole}Z"),
        _ => text.to_string(),
    };
    crate::parse_rfc3339(&whole)
}

/// The usage of the assistant messages in the transcript at `path` written at or after `since` (seconds
/// since the epoch). A message written once per content block is counted once. `None` when there are none.
pub(crate) fn usage_from_transcript(path: &Path, since: u64) -> Option<SessionUsage> {
    use std::io::BufRead;
    let file = open_without_waiting(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let reader = std::io::BufReader::new(file.take(MAX_TRANSCRIPT_BYTES));
    let mut messages: Vec<(String, String, Value)> = Vec::new();
    for line in reader.split(b'\n') {
        let Ok(line) = line else { break };
        let Ok(entry) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        if entry.get("type").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let at = entry
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(transcript_time);
        if !at.is_some_and(|at| at >= since) {
            continue;
        }
        let message = &entry["message"];
        let (Some(id), Some(usage)) = (
            message.get("id").and_then(Value::as_str),
            message.get("usage"),
        ) else {
            continue;
        };
        let model = message
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        match messages.iter_mut().find(|(seen, _, _)| seen == id) {
            Some(known) => known.2 = usage.clone(),
            None => messages.push((id.to_string(), model, usage.clone())),
        }
    }
    if messages.is_empty() {
        return None;
    }
    let mut total = SessionUsage {
        turns: messages.len() as u64,
        ..SessionUsage::default()
    };
    let mut by_model: std::collections::BTreeMap<&str, ModelUsage> = Default::default();
    for (_, model, usage) in &messages {
        let used = by_model.entry(model).or_insert_with(|| ModelUsage {
            model: model.clone(),
            ..ModelUsage::default()
        });
        used.input_tokens += count(usage, "input_tokens");
        used.output_tokens += count(usage, "output_tokens");
        used.cache_read_tokens += count(usage, "cache_read_input_tokens");
        used.cache_write_tokens += count(usage, "cache_creation_input_tokens");
    }
    total.models = by_model.into_values().collect();
    for used in &total.models {
        total.input_tokens += used.input_tokens;
        total.output_tokens += used.output_tokens;
        total.cache_read_tokens += used.cache_read_tokens;
        total.cache_write_tokens += used.cache_write_tokens;
    }
    total.model = dominant(&total.models);
    Some(total)
}

/// Opens `path` without waiting on a writer, should it be a FIFO.
#[cfg(unix)]
fn open_without_waiting(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
}

#[cfg(not(unix))]
fn open_without_waiting(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::File::open(path)
}

/// The usage in what a session wrote to `log` after `offset`, read from the end only.
pub(crate) fn usage_from_log(log: &Path, offset: u64) -> Option<SessionUsage> {
    let mut file = std::fs::File::open(log).ok()?;
    let len = file.metadata().ok()?.len();
    let from = offset.max(len.saturating_sub(MAX_RESULT_BYTES));
    file.seek(SeekFrom::Start(from)).ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_RESULT_BYTES).read_to_end(&mut bytes).ok()?;
    usage_from_result(&String::from_utf8_lossy(&bytes))
}
