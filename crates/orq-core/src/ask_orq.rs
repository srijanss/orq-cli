//! Ask orq: a chat with Claude Code about one project, run on this machine in the project's main checkout.
//! Each message is one detached `claude -p` turn whose raw stream-json is kept under `<state>/chats/<chat>/`;
//! the chats and their turns are rows in orq.db. The web reads a chat back as items, each text redacted.

use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

use crate::remote_inbox::{ProjectsProvider, Response};
use crate::storage::StorageRoots;

/// How long one turn may run before it is stopped.
pub const TIME_LIMIT: Duration = Duration::from_secs(30 * 60);
/// The most characters one message may hold.
pub const MAX_TEXT: usize = 20_000;
/// The most bytes a chat action's body may hold: a message of [`MAX_TEXT`] characters, each up to four bytes,
/// escaped in JSON.
pub const MAX_BODY: usize = 128 * 1024;
/// The most bytes of a tool's result an item keeps.
const MAX_RESULT: usize = 4096;
/// The most characters a title keeps of the first message.
const TITLE_CHARS: usize = 80;
/// Most replies written at once on the machine, across every project's chats.
const MAX_RUNNING: i64 = 2;

/// Where a chat of a project runs and how its turns start.
#[derive(Clone, Debug)]
pub struct ChatPlace {
    /// The project's main checkout: each turn's working directory.
    pub root: PathBuf,
    /// The git project id the project's runs are recorded under.
    pub project: String,
    /// The `claude` program a turn starts.
    pub program: PathBuf,
    /// How long a turn may run.
    pub time_limit: Duration,
}

fn reply(status: u16, body: Value) -> Response {
    Response { status, body }
}

fn error(status: u16, message: &str) -> Response {
    reply(status, json!({ "error": message }))
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64)
}

/// The project id and what follows `/chats` in `/api/projects/{id}/chats[/...]`, when the path has that shape.
pub fn target(path: &str) -> Option<(&str, Vec<&str>)> {
    let rest = path.strip_prefix("/api/projects/")?;
    let mut parts = rest.split('/');
    let id = parts.next().filter(|id| !id.is_empty())?;
    if parts.next()? != "chats" {
        return None;
    }
    let tail: Vec<&str> = parts.collect();
    if tail.iter().any(|part| part.is_empty()) {
        return None;
    }
    Some((id, tail))
}

/// Opens orq.db with the chat tables in it.
fn open(roots: &StorageRoots) -> Result<Connection, String> {
    // The store's own migrations run first, so the chat tables never land in a database it would refuse.
    crate::store::Store::open(&roots.database())
        .map_err(|_| "the chats could not be read".to_string())?;
    let conn = Connection::open(roots.database())
        .map_err(|_| "the chats could not be read".to_string())?;
    conn.busy_timeout(Duration::from_secs(10))
        .map_err(|_| "the chats could not be read".to_string())?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS ask_orq_chats (
            id TEXT PRIMARY KEY,
            project TEXT NOT NULL,
            title TEXT NOT NULL,
            session_id TEXT NOT NULL,
            model TEXT NOT NULL,
            effort TEXT,
            context_kind TEXT,
            context_target TEXT,
            created_at INTEGER NOT NULL,
            last_message_at INTEGER
        );
        CREATE TABLE IF NOT EXISTS ask_orq_turns (
            chat_id TEXT NOT NULL,
            n INTEGER NOT NULL,
            text TEXT NOT NULL,
            state TEXT NOT NULL,
            reason TEXT,
            pid INTEGER,
            started_at INTEGER NOT NULL,
            ended_at INTEGER,
            PRIMARY KEY (chat_id, n)
        );",
    )
    .map_err(|_| "the chats could not be read".to_string())?;
    Ok(conn)
}

/// A fresh random UUID (version 4).
fn uuid() -> String {
    use std::io::Read;
    let mut bytes = [0u8; 16];
    if std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .is_err()
    {
        use std::hash::{BuildHasher, Hasher};
        for (i, chunk) in bytes.chunks_mut(8).enumerate() {
            let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
            hasher.write_usize(i);
            hasher.write_u128(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |elapsed| elapsed.as_nanos()),
            );
            chunk.copy_from_slice(&hasher.finish().to_le_bytes());
        }
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// The folder a chat's turn files are kept in.
fn chat_dir(roots: &StorageRoots, chat: &str) -> PathBuf {
    roots.state.join("chats").join(chat)
}

fn turn_file(roots: &StorageRoots, chat: &str, n: i64) -> PathBuf {
    chat_dir(roots, chat).join(format!("turn-{n}.jsonl"))
}

/// One chat's row.
struct Chat {
    id: String,
    title: String,
    session_id: String,
    model: String,
    effort: Option<String>,
    context_kind: Option<String>,
    context_target: Option<String>,
    created_at: i64,
    last_message_at: Option<i64>,
}

const CHAT_COLUMNS: &str = "id, title, session_id, model, effort, context_kind, context_target, created_at, last_message_at";

fn chat_row(row: &rusqlite::Row) -> rusqlite::Result<Chat> {
    Ok(Chat {
        id: row.get(0)?,
        title: row.get(1)?,
        session_id: row.get(2)?,
        model: row.get(3)?,
        effort: row.get(4)?,
        context_kind: row.get(5)?,
        context_target: row.get(6)?,
        created_at: row.get(7)?,
        last_message_at: row.get(8)?,
    })
}

impl Chat {
    fn json(&self, running: bool) -> Value {
        let context = self
            .context_kind
            .as_ref()
            .map(|kind| json!({ "kind": kind, "target": self.context_target }));
        json!({
            "id": self.id,
            "title": crate::redact::redact(&self.title),
            "session_id": self.session_id,
            "model": self.model,
            "effort": self.effort,
            "context": context,
            "created_at": self.created_at,
            "last_message_at": self.last_message_at,
            "running": running,
        })
    }
}

fn load_chat(conn: &Connection, project: &str, chat: &str) -> Result<Option<Chat>, String> {
    conn.query_row(
        &format!("SELECT {CHAT_COLUMNS} FROM ask_orq_chats WHERE project = ?1 AND id = ?2"),
        params![project, chat],
        chat_row,
    )
    .optional()
    .map_err(|_| "the chat could not be read".to_string())
}

/// Chat `chat` of project `project`, or the reply saying it is not there or could not be read.
fn found(conn: &Connection, project: &str, chat: &str) -> Result<Chat, Response> {
    match load_chat(conn, project, chat) {
        Ok(Some(chat)) => Ok(chat),
        Ok(None) => Err(error(404, "not found")),
        Err(why) => Err(error(500, &why)),
    }
}

fn running(conn: &Connection, chat: &str) -> bool {
    conn.query_row(
        "SELECT COUNT(*) FROM ask_orq_turns WHERE chat_id = ?1 AND state = 'running'",
        params![chat],
        |row| row.get::<_, i64>(0),
    )
    .unwrap_or(0)
        > 0
}

/// The global Ask orq agent: its model and effort, `None` when none is set.
fn agent(roots: &StorageRoots) -> Option<(String, Option<String>)> {
    let settings = crate::remote_inbox::public_settings(&roots.config_file()).ok()?;
    let agent = &settings["ask_orq_agent"];
    let model = agent["model"].as_str()?.to_string();
    Some((model, agent["effort"].as_str().map(str::to_string)))
}

/// Answers a request on `/api/projects/{id}/chats` and below (`tail` is what follows `/chats`). Every route needs
/// a passkey session (`passkey`), since replies can quote files; a change also needs remote actions on. Each
/// message, stop and delete leaves an audit line, allowed or refused.
#[allow(clippy::too_many_arguments)]
pub fn route(
    roots: &StorageRoots,
    provider: Option<&dyn ProjectsProvider>,
    id: &str,
    tail: &[&str],
    method: &str,
    body: &[u8],
    passkey: bool,
) -> Response {
    let audited = match (tail, method) {
        ([chat, "messages"], "POST") => Some(("chat.message", *chat)),
        ([chat, "stop"], "POST") => Some(("chat.stop", *chat)),
        ([chat], "DELETE") => Some(("chat.delete", *chat)),
        _ => None,
    };
    let Some((action, chat)) = audited else {
        return answer(roots, provider, id, tail, method, body, passkey);
    };
    // The message's first characters, or for a stop or delete the chat's title (its first message's).
    let note = match action {
        "chat.message" => serde_json::from_slice::<Value>(body)
            .ok()
            .and_then(|fields| {
                fields["text"]
                    .as_str()
                    .map(preview)
            }),
        _ => open(roots)
            .ok()
            .and_then(|conn| load_chat(&conn, id, chat).ok().flatten())
            .map(|chat| chat.title),
    };
    let response = answer(roots, provider, id, tail, method, body, passkey);
    let refusal = (response.status >= 400).then(|| {
        response.body["error"]
            .as_str()
            .unwrap_or("refused")
            .to_string()
    });
    audit(
        roots,
        action,
        id,
        chat,
        refusal.as_deref().or(note.as_deref()),
        refusal.is_none(),
    );
    response
}

fn audit(
    roots: &StorageRoots,
    action: &str,
    project: &str,
    chat: &str,
    reason: Option<&str>,
    allowed: bool,
) {
    use crate::remote_audit::{record, Attempt, Outcome};
    let outcome = if allowed {
        Outcome::Allowed
    } else {
        Outcome::Refused
    };
    let _ = record(
        roots,
        &Attempt {
            identity: "web",
            action,
            project,
            run: Some(chat),
            outcome,
            reason,
        },
        now() as u64,
    );
}

#[allow(clippy::too_many_arguments)]
fn answer(
    roots: &StorageRoots,
    provider: Option<&dyn ProjectsProvider>,
    id: &str,
    tail: &[&str],
    method: &str,
    body: &[u8],
    passkey: bool,
) -> Response {
    if !passkey {
        return error(403, "Chats need a passkey sign-in");
    }
    let read = matches!(method, "GET" | "HEAD");
    if !read && !crate::remote_inbox::remote_actions_on(roots) {
        return error(403, "Remote actions are off");
    }
    let Some(provider) = provider else {
        return error(404, "not found");
    };
    let place = match provider.chat_place(id) {
        Ok(Some(place)) => place,
        Ok(None) => return error(404, "not found"),
        Err(_) => return error(500, "the project could not be read"),
    };
    let conn = match open(roots) {
        Ok(conn) => conn,
        Err(why) => return error(500, &why),
    };
    match (tail, read, method) {
        ([], true, _) => list(&conn, id),
        ([], false, "POST") => create(roots, provider, &conn, id, body),
        ([chat], true, _) => read_chat(roots, &conn, id, chat),
        ([chat, "messages"], false, "POST") => send(roots, provider, &conn, &place, id, chat, body),
        ([chat, "stop"], false, "POST") => stop(roots, &conn, id, chat, body),
        ([chat], false, "DELETE") => delete(roots, &conn, id, chat, body),
        _ => error(405, "this action is not one a chat takes"),
    }
}

fn list(conn: &Connection, project: &str) -> Response {
    let chats = conn
        .prepare(&format!("SELECT {CHAT_COLUMNS} FROM ask_orq_chats WHERE project = ?1 ORDER BY created_at DESC, rowid DESC"))
        .and_then(|mut statement| statement.query_map(params![project], chat_row)?.collect::<rusqlite::Result<Vec<Chat>>>());
    match chats {
        Ok(chats) => reply(
            200,
            json!({ "chats": chats.iter().map(|chat| chat.json(running(conn, &chat.id))).collect::<Vec<_>>() }),
        ),
        Err(_) => error(500, "the chats could not be read"),
    }
}

fn body_fields(body: &[u8], allowed: &[&str]) -> Result<Value, Response> {
    if body.len() > MAX_BODY {
        return Err(error(413, "the request body is too large"));
    }
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(json!({}));
    }
    match serde_json::from_slice::<Value>(body) {
        Ok(Value::Object(fields)) if fields.keys().all(|key| allowed.contains(&key.as_str())) => {
            Ok(Value::Object(fields))
        }
        _ => Err(error(400, "the request body is not one this action takes")),
    }
}

/// Whether `target` names a run (`kind` "run") or a feature of project `project`.
fn belongs(provider: &dyn ProjectsProvider, project: &str, kind: &str, target: &str) -> bool {
    match kind {
        "run" => {
            crate::storage::check_run_id(target).is_ok()
                && matches!(provider.run_timeline(project, target, None), Ok(Some(_)))
        }
        _ => features_of(provider, project)
            .iter()
            .any(|entry| entry["name"] == target),
    }
}

/// The project's ledger entries, none when they cannot be read.
fn features_of(provider: &dyn ProjectsProvider, project: &str) -> Vec<Value> {
    provider
        .features(project)
        .ok()
        .flatten()
        .and_then(|features| features.as_array().cloned())
        .unwrap_or_default()
}

fn create(
    roots: &StorageRoots,
    provider: &dyn ProjectsProvider,
    conn: &Connection,
    project: &str,
    body: &[u8],
) -> Response {
    let fields = match body_fields(body, &["title", "context", "model"]) {
        Ok(fields) => fields,
        Err(refusal) => return refusal,
    };
    let context = match &fields["context"] {
        Value::Null => None,
        Value::Object(context) => match (
            context.get("kind").and_then(Value::as_str),
            context.get("target").and_then(Value::as_str),
        ) {
            (Some(kind @ ("run" | "feature")), Some(target)) if !target.is_empty() => {
                Some((kind.to_string(), Some(target.to_string())))
            }
            (Some(kind @ ("queue" | "project")), _) => Some((kind.to_string(), None)),
            _ => return error(400, "the context is not one a chat takes"),
        },
        _ => return error(400, "the context is not one a chat takes"),
    };
    if let Some((kind, Some(target))) = &context {
        if !belongs(provider, project, kind, target) {
            return error(422, &format!("that {kind} is not one of this project's"));
        }
    }
    let Some((mut model, effort)) = agent(roots) else {
        return error(409, "set the Ask orq agent in Settings");
    };
    match &fields["model"] {
        Value::Null => {}
        Value::String(picked) => {
            let options = provider
                .setting_options()
                .ok()
                .flatten()
                .unwrap_or(Value::Null);
            if !crate::remote_inbox::offered(&options, "claude", picked) {
                return error(422, "a chat takes only a Claude model from orq models");
            }
            model = picked.clone();
        }
        _ => return error(400, "the request body is not one this action takes"),
    }
    let title = preview(fields["title"].as_str().unwrap_or(""));
    let chat = format!("chat-{}", &uuid()[..18]);
    let dir = chat_dir(roots, &chat);
    use std::os::unix::fs::DirBuilderExt;
    if std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .is_err()
        || std::fs::write(dir.join("settings.json"), chat_settings()).is_err()
    {
        return error(500, "the chat could not be created");
    }
    let created = conn.execute(
        "INSERT INTO ask_orq_chats (id, project, title, session_id, model, effort, context_kind, context_target, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![chat, project, title, uuid(), model, effort, context.as_ref().map(|(kind, _)| kind), context.as_ref().and_then(|(_, target)| target.as_ref()), now()],
    );
    if created.is_err() {
        return error(500, "the chat could not be created");
    }
    match load_chat(conn, project, &chat) {
        Ok(Some(chat)) => reply(200, json!({ "chat": chat.json(false) })),
        _ => error(500, "the chat could not be created"),
    }
}

/// The only tools a chat's turns have.
const CHAT_TOOLS: [&str; 3] = ["Read", "Grep", "Glob"];

/// The chat's Claude settings: don't-ask mode, reading only.
fn chat_settings() -> String {
    json!({ "permissions": { "defaultMode": "dontAsk", "allow": CHAT_TOOLS } }).to_string()
}

fn read_chat(roots: &StorageRoots, conn: &Connection, project: &str, chat: &str) -> Response {
    let chat = match found(conn, project, chat) {
        Ok(chat) => chat,
        Err(refusal) => return refusal,
    };
    let turns = conn
        .prepare("SELECT n, text, state, reason FROM ask_orq_turns WHERE chat_id = ?1 ORDER BY n")
        .and_then(|mut statement| {
            statement
                .query_map(params![chat.id], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()
        });
    let Ok(turns) = turns else {
        return error(500, "the chat could not be read");
    };
    let mut items = Vec::new();
    for (n, text, state, reason) in &turns {
        items.push(json!({ "type": "user", "turn": n, "text": crate::redact::redact(text), "state": state, "reason": reason }));
        let raw = std::fs::read_to_string(turn_file(roots, &chat.id, *n)).unwrap_or_default();
        items.extend(turn_items(&raw));
    }
    reply(
        200,
        json!({ "chat": chat.json(running(conn, &chat.id)), "items": items, "running": running(conn, &chat.id) }),
    )
}

/// The items one turn's stream-json holds: assistant texts, tool calls with their results, and the result.
pub fn turn_items(raw: &str) -> Vec<Value> {
    let mut items: Vec<Value> = Vec::new();
    let mut tools: Vec<(String, usize)> = Vec::new();
    for line in raw.lines() {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match event["type"].as_str() {
            Some("assistant") => {
                for part in event["message"]["content"].as_array().into_iter().flatten() {
                    match part["type"].as_str() {
                        Some("text") => items.push(json!({ "type": "assistant", "text": crate::redact::redact(part["text"].as_str().unwrap_or("")) })),
                        Some("tool_use") => {
                            tools.push((part["id"].as_str().unwrap_or("").to_string(), items.len()));
                            items.push(json!({ "type": "tool", "name": crate::redact::redact(part["name"].as_str().unwrap_or("")), "input": input_summary(&part["input"]), "status": "running", "result": null }));
                        }
                        _ => {}
                    }
                }
            }
            Some("user") => {
                for part in event["message"]["content"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|part| part["type"] == "tool_result")
                {
                    let Some(&(_, at)) = tools
                        .iter()
                        .find(|(id, _)| part["tool_use_id"] == id.as_str())
                    else {
                        continue;
                    };
                    let result = result_text(&part["content"]);
                    let status = if part["is_error"] == true {
                        if refused(&result) {
                            "refused"
                        } else {
                            "error"
                        }
                    } else {
                        "ok"
                    };
                    items[at]["status"] = json!(status);
                    items[at]["result"] = json!(cut(&crate::redact::redact(&result), MAX_RESULT));
                }
            }
            Some("result") => {
                let error = (event["is_error"] == true).then(|| {
                    crate::redact::redact(
                        event["result"]
                            .as_str()
                            .or(event["subtype"].as_str())
                            .unwrap_or("the turn failed"),
                    )
                });
                items.push(json!({ "type": "result", "cost": event["total_cost_usd"], "duration_ms": event["duration_ms"], "error": error }));
            }
            _ => {}
        }
    }
    items
}

/// Whether a tool's error says Claude Code's permissions refused the call.
fn refused(text: &str) -> bool {
    let text = text.to_lowercase();
    text.contains("permission") || text.contains("denied") || text.contains("haven't granted")
}

/// A tool result's text: a string, or the texts of its parts.
fn result_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| part["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// One line saying what a tool was given: its path, pattern, command or address when it has one.
fn input_summary(input: &Value) -> String {
    let named = [
        "file_path",
        "path",
        "pattern",
        "command",
        "url",
        "query",
        "description",
    ]
    .iter()
    .find_map(|key| input[key].as_str().map(str::to_string));
    let text = named.unwrap_or_else(|| {
        if input.is_null() {
            String::new()
        } else {
            input.to_string()
        }
    });
    let line: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    // Redacted whole first: a key the cut ends inside would no longer be recognised.
    cut(&crate::redact::redact(&line), 200)
}

/// A title or audit note from `text`: its first characters, redacted before it is cut so a key the cut ends inside
/// is not shown in part.
fn preview(text: &str) -> String {
    crate::redact::redact(text).chars().take(TITLE_CHARS).collect()
}

/// `text` cut to at most `max` bytes, at a character boundary.
fn cut(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

/// What the chat was opened on, said before the person's first message so it needs nothing else; empty for a
/// chat with no context. The run's facts come from the provider and its record, each text redacted.
fn context_prefix(
    roots: &StorageRoots,
    provider: &dyn ProjectsProvider,
    project: &str,
    chat: &Chat,
) -> String {
    let Some(kind) = chat.context_kind.as_deref() else {
        return String::new();
    };
    let target = chat.context_target.as_deref().unwrap_or_default();
    let page = if target.is_empty() {
        format!("the project's {kind} page")
    } else {
        format!("the {kind} page of {target}")
    };
    let mut lines = vec![format!(
        "[Ask orq] This chat was opened on {page}. What orq knows about it:"
    )];
    match kind {
        "run" => lines.extend(run_context(roots, provider, project, target)),
        "feature" => lines.extend(feature_context(provider, project, target)),
        "queue" => lines.extend(queue_context(provider, project)),
        _ => lines.extend(project_context(
            provider,
            project,
            &chat_place_root(provider, project),
        )),
    }
    let text = lines.join("\n");
    format!("{}\n\n", crate::redact::redact(&text))
}

/// The lines saying what run `run` of project `project` is.
fn run_context(
    roots: &StorageRoots,
    provider: &dyn ProjectsProvider,
    project: &str,
    run: &str,
) -> Vec<String> {
    if crate::storage::check_run_id(run).is_err() {
        return Vec::new();
    }
    let shown = provider
        .run_timeline(project, run, None)
        .ok()
        .flatten()
        .map(|timeline| timeline["run"].clone())
        .unwrap_or(Value::Null);
    let dir = roots.state.join("runs").join(run);
    let record: Value = std::fs::read_to_string(dir.join("run.json"))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(Value::Null);
    let text = |value: &Value| value.as_str().map(str::to_string);
    let mut lines = vec![format!("run: {run}")];
    if let Some(feature) = text(&shown["feature"]).or_else(|| text(&record["feature_id"])) {
        lines.push(format!("feature: {feature}"));
    }
    let members = group_members(&dir.join("events.jsonl"));
    if !members.is_empty() {
        lines.push(format!("group members: {}", members.join(", ")));
    }
    for (label, key) in [("state", "state"), ("stage", "stage")] {
        if let Some(value) = text(&shown[key]) {
            lines.push(format!("{label}: {value}"));
        }
    }
    lines.push(format!(
        "events file: {}",
        dir.join("events.jsonl").display()
    ));
    lines.push(format!("logs folder: {}", roots.logs().join(run).display()));
    if let Some(worktree) = text(&record["worktree"]) {
        lines.push(format!("worktree: {worktree}"));
    }
    if let Some(reason) = text(&shown["ending_reason"]) {
        lines.push(format!("why it ended: {reason}"));
    }
    if let Some(from) = text(&shown["resumed_from"]) {
        lines.push(format!("continued from run: {from}"));
    }
    if let Some(into) = text(&shown["resumed_as"]) {
        lines.push(format!("continued into run: {into}"));
    }
    lines
}

fn chat_place_root(provider: &dyn ProjectsProvider, project: &str) -> String {
    provider
        .chat_place(project)
        .ok()
        .flatten()
        .map(|place| place.root.display().to_string())
        .unwrap_or_default()
}

/// The project's runs, newest first, as the runs page lists them.
fn runs_of(provider: &dyn ProjectsProvider, project: &str) -> Vec<Value> {
    provider
        .runs(project)
        .ok()
        .flatten()
        .and_then(|runs| runs.as_array().cloned())
        .unwrap_or_default()
}

fn field<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}

/// The lines saying what feature `feature` is and how its latest runs went.
fn feature_context(provider: &dyn ProjectsProvider, project: &str, feature: &str) -> Vec<String> {
    let features = features_of(provider, project);
    let mut lines = vec![format!("feature: {feature}")];
    if let Some(entry) = features.iter().find(|entry| entry["name"] == feature) {
        lines.push(format!("status: {}", field(entry, "status")));
        if let Some(kind) = entry["kind"].as_str() {
            lines.push(format!("kind: {kind}"));
        }
    }
    let runs: Vec<String> = runs_of(provider, project)
        .iter()
        .filter(|run| run["feature"] == feature)
        .take(5)
        .map(|run| format!("{} ({})", field(run, "run_id"), field(run, "state")))
        .collect();
    lines.push(format!(
        "latest runs: {}",
        if runs.is_empty() {
            "none".to_string()
        } else {
            runs.join(", ")
        }
    ));
    lines
}

/// The lines saying what the project's queue holds and whether it is paused.
fn queue_context(provider: &dyn ProjectsProvider, project: &str) -> Vec<String> {
    let queue = provider
        .queue(project)
        .ok()
        .flatten()
        .unwrap_or(Value::Null);
    let entries: Vec<String> = queue["entries"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|entry| format!("{} ({})", field(entry, "feature"), field(entry, "state")))
        .collect();
    vec![
        format!(
            "queue: {}",
            if entries.is_empty() {
                "empty".to_string()
            } else {
                entries.join(", ")
            }
        ),
        format!(
            "paused: {}",
            if queue["paused"] == true {
                "yes, the queue is paused"
            } else {
                "no"
            }
        ),
    ]
}

/// The lines saying where the project is and which of its runs are active.
fn project_context(provider: &dyn ProjectsProvider, project: &str, root: &str) -> Vec<String> {
    let active: Vec<String> = runs_of(provider, project)
        .iter()
        .filter(|run| {
            matches!(
                field(run, "state"),
                "running" | "pause_requested" | "accepting"
            )
        })
        .map(|run| {
            format!(
                "{} ({}, {})",
                field(run, "run_id"),
                field(run, "feature"),
                field(run, "state")
            )
        })
        .collect();
    vec![
        format!("project path: {root}"),
        format!(
            "active runs: {}",
            if active.is_empty() {
                "none".to_string()
            } else {
                active.join(", ")
            }
        ),
    ]
}

/// The features a group run's last `run.grouped` event names, in order.
fn group_members(events: &std::path::Path) -> Vec<String> {
    let text = std::fs::read_to_string(events).unwrap_or_default();
    let grouped = text
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|event| event["type"] == "run.grouped")
        .next_back();
    grouped
        .and_then(|event| {
            event["payload"]["features"].as_array().map(|features| {
                features
                    .iter()
                    .filter_map(|feature| feature.as_str().map(str::to_string))
                    .collect()
            })
        })
        .unwrap_or_default()
}

#[allow(clippy::too_many_arguments)]
fn send(
    roots: &StorageRoots,
    provider: &dyn ProjectsProvider,
    conn: &Connection,
    place: &ChatPlace,
    project: &str,
    chat: &str,
    body: &[u8],
) -> Response {
    let fields = match body_fields(body, &["text"]) {
        Ok(fields) => fields,
        Err(refusal) => return refusal,
    };
    let Some(text) = fields["text"].as_str() else {
        return error(400, "the request body is not one this action takes");
    };
    let length = text.chars().count();
    if text.trim().is_empty() || length > MAX_TEXT {
        return error(422, &format!("a message is 1 to {MAX_TEXT} characters"));
    }
    let chat = match found(conn, project, chat) {
        Ok(chat) => chat,
        Err(refusal) => return refusal,
    };
    let stamp = now();
    let n = match claim_turn(conn, &chat.id, text, stamp) {
        Ok(Claim::Turn(n)) => n,
        Ok(Claim::ChatBusy) => return error(409, "a reply is still being written"),
        Ok(Claim::MachineBusy(count)) => return error(409, &format!("{count} replies are already being written on this machine; at most {MAX_RUNNING} at once")),
        Ok(Claim::Gone) => return error(404, "not found"),
        Err(_) => return error(500, "the message could not be sent"),
    };
    let title = preview(text);
    let _ = conn.execute("UPDATE ask_orq_chats SET last_message_at = ?2, title = CASE WHEN title = '' THEN ?3 ELSE title END WHERE id = ?1", params![chat.id, stamp, title]);
    let stdin = if n == 1 {
        format!("{}{text}", context_prefix(roots, provider, project, &chat))
    } else {
        text.to_string()
    };
    match start_turn(roots, place, &chat, n, stdin) {
        Ok(pid) => {
            let _ = conn.execute(
                "UPDATE ask_orq_turns SET pid = ?3 WHERE chat_id = ?1 AND n = ?2",
                params![chat.id, n, pid],
            );
            reply(202, json!({ "turn": n }))
        }
        Err(_) => {
            end_turn(
                roots,
                &chat.id,
                n,
                "failed",
                Some("the reply could not be started"),
            );
            error(500, "the reply could not be started")
        }
    }
}

/// Ends the chat's running turn: records it as stopped, then kills its process group.
fn stop(
    roots: &StorageRoots,
    conn: &Connection,
    project: &str,
    chat: &str,
    body: &[u8],
) -> Response {
    if let Err(refusal) = body_fields(body, &[]) {
        return refusal;
    }
    let chat = match found(conn, project, chat) {
        Ok(chat) => chat,
        Err(refusal) => return refusal,
    };
    let turn = conn
        .query_row(
            "SELECT n, pid FROM ask_orq_turns WHERE chat_id = ?1 AND state = 'running'",
            params![chat.id],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<u32>>(1)?)),
        )
        .optional();
    match turn {
        Ok(Some((n, pid))) => {
            end_turn(roots, &chat.id, n, "stopped", Some("stopped from the web"));
            // A turn whose pid is not stored yet is killed by its watcher, which sees it is no longer running.
            if let Some(pid) = pid {
                kill_group(pid);
            }
            reply(200, json!({ "turn": n, "state": "stopped" }))
        }
        Ok(None) => error(409, "no reply is being written"),
        Err(_) => error(500, "the chat could not be read"),
    }
}

/// Deletes a chat with no reply being written: its row, its turns and its folder of turn files. The Claude
/// transcript under ~/.claude is left alone.
fn delete(
    roots: &StorageRoots,
    conn: &Connection,
    project: &str,
    chat: &str,
    body: &[u8],
) -> Response {
    if let Err(refusal) = body_fields(body, &[]) {
        return refusal;
    }
    let chat = match found(conn, project, chat) {
        Ok(chat) => chat,
        Err(refusal) => return refusal,
    };
    // One statement, so a message cannot start a turn between the check and the delete.
    let deleted = conn.execute(
        "DELETE FROM ask_orq_chats WHERE id = ?1 AND project = ?2 AND NOT EXISTS (SELECT 1 FROM ask_orq_turns WHERE chat_id = ?1 AND state = 'running')",
        params![chat.id, project],
    );
    match deleted {
        Ok(1) => {
            let _ = conn.execute(
                "DELETE FROM ask_orq_turns WHERE chat_id = ?1",
                params![chat.id],
            );
            match std::fs::remove_dir_all(chat_dir(roots, &chat.id)) {
                Ok(()) => reply(200, json!({ "deleted": chat.id })),
                Err(missing) if missing.kind() == std::io::ErrorKind::NotFound => {
                    reply(200, json!({ "deleted": chat.id }))
                }
                Err(_) => error(500, "the chat's files could not be deleted"),
            }
        }
        Ok(_) if running(conn, &chat.id) => error(409, "a reply is still being written"),
        Ok(_) => error(404, "not found"),
        Err(_) => error(500, "the chat could not be deleted"),
    }
}

enum Claim {
    Turn(i64),
    ChatBusy,
    MachineBusy(i64),
    Gone,
}

/// Records the chat's next turn as running, unless the chat or the machine is already writing as many replies as
/// it may. One statement, so two requests at once cannot both pass the checks.
fn claim_turn(conn: &Connection, chat: &str, text: &str, stamp: i64) -> rusqlite::Result<Claim> {
    let inserted = conn.execute(
        "INSERT INTO ask_orq_turns (chat_id, n, text, state, started_at)
         SELECT ?1, (SELECT COALESCE(MAX(n), 0) + 1 FROM ask_orq_turns WHERE chat_id = ?1), ?2, 'running', ?3
         WHERE EXISTS (SELECT 1 FROM ask_orq_chats WHERE id = ?1)
           AND NOT EXISTS (SELECT 1 FROM ask_orq_turns WHERE chat_id = ?1 AND state = 'running')
           AND (SELECT COUNT(*) FROM ask_orq_turns WHERE state = 'running') < ?4",
        params![chat, text, stamp, MAX_RUNNING],
    )?;
    if inserted == 1 {
        return conn
            .query_row(
                "SELECT MAX(n) FROM ask_orq_turns WHERE chat_id = ?1",
                params![chat],
                |row| row.get(0),
            )
            .map(Claim::Turn);
    }
    if !conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM ask_orq_chats WHERE id = ?1)",
        params![chat],
        |row| row.get::<_, bool>(0),
    )? {
        return Ok(Claim::Gone);
    }
    if running(conn, chat) {
        return Ok(Claim::ChatBusy);
    }
    conn.query_row(
        "SELECT COUNT(*) FROM ask_orq_turns WHERE state = 'running'",
        [],
        |row| row.get(0),
    )
    .map(Claim::MachineBusy)
}

/// The argv of turn `n` of `chat`, after the program.
fn turn_args(roots: &StorageRoots, place: &ChatPlace, chat: &Chat, n: i64) -> Vec<String> {
    let mut args: Vec<String> = ["-p", "--output-format", "stream-json", "--verbose"]
        .iter()
        .map(|arg| arg.to_string())
        .collect();
    args.extend([
        if n == 1 { "--session-id" } else { "--resume" }.to_string(),
        chat.session_id.clone(),
    ]);
    args.extend(["--model".to_string(), chat.model.clone()]);
    if let Some(effort) = &chat.effort {
        args.extend(["--effort".to_string(), effort.clone()]);
    }
    for dir in [
        roots.state.join("runs"),
        roots.logs(),
        place.root.join(".claude").join("worktrees"),
    ] {
        args.extend(["--add-dir".to_string(), dir.display().to_string()]);
    }
    args.extend([
        "--settings".to_string(),
        chat_dir(roots, &chat.id)
            .join("settings.json")
            .display()
            .to_string(),
    ]);
    args.extend(["--permission-mode".to_string(), "dontAsk".to_string()]);
    // The tools a chat has at all, so settings it inherits from the user or the project cannot widen them, and
    // no MCP server from their configuration.
    args.extend(["--tools".to_string(), CHAT_TOOLS.join(","), "--strict-mcp-config".to_string()]);
    args
}

/// Starts turn `n` of `chat` detached in its own process group, its stdout kept in the turn's file, and watches
/// it until it ends or runs out of time. The pid of the turn's process.
fn start_turn(
    roots: &StorageRoots,
    place: &ChatPlace,
    chat: &Chat,
    n: i64,
    text: String,
) -> Result<u32, String> {
    let out =
        std::fs::File::create(turn_file(roots, &chat.id, n)).map_err(|error| error.to_string())?;
    let mut child = Command::new(&place.program)
        .args(turn_args(roots, place, chat, n))
        .current_dir(&place.root)
        .stdin(Stdio::piped())
        .stdout(out)
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|error| error.to_string())?;
    let pid = child.id();
    let (roots, chat_id, limit) = (roots.clone(), chat.id.clone(), place.time_limit);
    // The message goes in on a thread of its own: a process that never reads it must not hold up the watcher,
    // whose time limit then kills it (which ends the write too).
    if let Some(mut stdin) = child.stdin.take() {
        std::thread::spawn(move || {
            let _ = stdin.write_all(text.as_bytes());
        });
    }
    std::thread::spawn(move || {
        let deadline = Instant::now() + limit;
        for poll in 1u64.. {
            match child.try_wait() {
                Ok(Some(status)) => {
                    match ended(status) {
                        None => end_turn(&roots, &chat_id, n, "done", None),
                        Some(reason) => end_turn(&roots, &chat_id, n, "failed", Some(&reason)),
                    }
                    return;
                }
                Err(_) => {
                    end_turn(&roots, &chat_id, n, "failed", Some("the reply could not be watched"));
                    kill_group(pid);
                    let _ = child.wait();
                    return;
                }
                Ok(None) if Instant::now() >= deadline => {
                    end_turn(
                        &roots,
                        &chat_id,
                        n,
                        "stopped",
                        Some("the reply ran out of time"),
                    );
                    kill_group(pid);
                    let _ = child.wait();
                    return;
                }
                // Stopped (or marked interrupted) by someone else: make sure the process is gone too.
                Ok(None) if poll % 10 == 0 && !turn_running(&roots, &chat_id, n) => {
                    kill_group(pid);
                    let _ = child.wait();
                    return;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            }
        }
    });
    Ok(pid)
}

/// Why a turn's process that exited did not finish its reply; `None` when it exited cleanly. Only the status is
/// said: what the agent printed on stderr can quote anything.
fn ended(status: std::process::ExitStatus) -> Option<String> {
    use std::os::unix::process::ExitStatusExt;
    match (status.code(), status.signal()) {
        (Some(0), _) => None,
        (Some(code), _) => Some(format!("the reply ended with an error (exit status {code})")),
        (None, Some(signal)) => Some(format!("the reply was ended by signal {signal}")),
        (None, None) => Some("the reply ended with an error".to_string()),
    }
}

/// Kills the process group led by `pid`.
fn kill_group(pid: u32) {
    // SAFETY: killpg only sends a signal; a group that is gone is an error it returns.
    unsafe {
        libc::killpg(pid as libc::pid_t, libc::SIGKILL);
    }
}

/// Marks every turn still recorded as running as interrupted: called when the server starts, since the server
/// that watched them is gone. Their processes are left alone, as a pid recorded then may name another process now.
pub fn interrupt_running(roots: &StorageRoots) -> Result<usize, String> {
    let conn = open(roots)?;
    conn.execute("UPDATE ask_orq_turns SET state = 'interrupted', reason = 'the server restarted while the reply was being written', ended_at = ?1 WHERE state = 'running'", params![now()])
        .map_err(|_| "the chats could not be updated".to_string())
}

/// Whether turn `n` of `chat` is still recorded as running; true when that cannot be read, so a watcher keeps going.
fn turn_running(roots: &StorageRoots, chat: &str, n: i64) -> bool {
    let Ok(conn) = open(roots) else { return true };
    conn.query_row(
        "SELECT state FROM ask_orq_turns WHERE chat_id = ?1 AND n = ?2",
        params![chat, n],
        |row| row.get::<_, String>(0),
    )
    .map_or(true, |state| state == "running")
}

/// Records how turn `n` of `chat` ended, unless something else already did.
fn end_turn(roots: &StorageRoots, chat: &str, n: i64, state: &str, reason: Option<&str>) {
    if let Ok(conn) = open(roots) {
        let _ = conn.execute("UPDATE ask_orq_turns SET state = ?3, reason = ?4, ended_at = ?5 WHERE chat_id = ?1 AND n = ?2 AND state = 'running'", params![chat, n, state, reason, now()]);
    }
}
