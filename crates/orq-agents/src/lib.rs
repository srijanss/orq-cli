//! Harness adapters: what each agent harness can do and how to start it.
//! Orchestration asks an adapter for a capability and never branches on the
//! harness name; only `adapter_for` maps a name to an adapter.

/// Something a harness may or may not be able to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Capability {
    ResumeSession,
    RemoteControl,
    PushNotification,
    AutoCompact,
    McpPreapproval,
    PrintMode,
}

/// A resolved harness, model and thinking level.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelSpec {
    pub harness: String,
    pub model: String,
    pub thinking: Option<String>,
}

/// What the implement stage asks an adapter to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImplementRequest<'a> {
    pub feature: &'a str,
    pub implementer: ModelSpec,
    pub reviewer: ModelSpec,
    /// Free text appended to the request for this run.
    pub context: Option<&'a str>,
    /// A file the session creates as its very last step, so orq can close it; `None` asks for nothing.
    pub exit_marker: Option<&'a str>,
}

/// The longest extra context a run accepts, in characters.
pub const MAX_CONTEXT_CHARS: usize = 4000;

/// The context text as it goes into the prompt: `None` when blank, an error when it is too
/// long or holds control characters other than newline and tab.
pub fn checked_context(text: &str) -> Result<Option<&str>, AdapterError> {
    let bad = text.chars().count() > MAX_CONTEXT_CHARS || text.chars().any(|c| c.is_control() && !matches!(c, '\n' | '\t'));
    if bad {
        let shown: String = text.chars().take(40).collect();
        return Err(AdapterError::InvalidValue { field: "context", value: shown });
    }
    let trimmed = text.trim();
    Ok((!trimmed.is_empty()).then_some(trimmed))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdapterError {
    UnknownHarness(String),
    /// The request names a model of another harness than the adapter's.
    WrongHarness { adapter: &'static str, requested: String },
    /// The adapter cannot do this yet.
    Unsupported { adapter: &'static str, what: &'static str },
    /// A value that would not stay a single plain argument.
    InvalidValue { field: &'static str, value: String },
}

impl std::fmt::Display for AdapterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AdapterError::UnknownHarness(name) => write!(f, "unknown agent harness {name:?}; use claude or pi"),
            AdapterError::WrongHarness { adapter, requested } => {
                write!(f, "the {adapter} adapter cannot run a {requested} model")
            }
            AdapterError::Unsupported { adapter, what } => write!(f, "the {adapter} adapter does not support {what}"),
            AdapterError::InvalidValue { field, value } => write!(f, "invalid {field}: {value:?}"),
        }
    }
}

impl std::error::Error for AdapterError {}

pub trait HarnessAdapter {
    fn name(&self) -> &'static str;
    fn capabilities(&self) -> &'static [Capability];

    fn supports(&self, capability: Capability) -> bool {
        self.capabilities().contains(&capability)
    }

    /// Arguments (after the program) that start the implement session.
    fn implement_argv(&self, request: &ImplementRequest<'_>) -> Result<Vec<String>, AdapterError>;
}

pub fn adapter_for(harness: &str) -> Result<Box<dyn HarnessAdapter>, AdapterError> {
    match harness {
        "claude" => Ok(Box::new(Claude)),
        "pi" => Ok(Box::new(Pi)),
        other => Err(AdapterError::UnknownHarness(other.to_string())),
    }
}

/// A plain word: no whitespace, no leading dash, nothing a shell or the
/// slash-command parser could split or reinterpret.
fn plain(field: &'static str, value: &str) -> Result<(), AdapterError> {
    let ok = !value.is_empty()
        && !value.starts_with('-')
        && value.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | ':' | '@' | '+'));
    if ok {
        Ok(())
    } else {
        Err(AdapterError::InvalidValue { field, value: value.to_string() })
    }
}

struct Claude;

impl HarnessAdapter for Claude {
    fn name(&self) -> &'static str {
        "claude"
    }

    fn capabilities(&self) -> &'static [Capability] {
        &[
            Capability::ResumeSession,
            Capability::RemoteControl,
            Capability::PushNotification,
            Capability::AutoCompact,
            Capability::McpPreapproval,
            Capability::PrintMode,
        ]
    }

    fn implement_argv(&self, request: &ImplementRequest<'_>) -> Result<Vec<String>, AdapterError> {
        if request.implementer.harness != "claude" {
            return Err(AdapterError::WrongHarness {
                adapter: "claude",
                requested: request.implementer.harness.clone(),
            });
        }
        plain("feature", request.feature)?;
        plain("implementer model", &request.implementer.model)?;
        adapter_for(&request.reviewer.harness)?;
        plain("reviewer model", &request.reviewer.model)?;
        let mut prompt = format!(
            "/implement-and-review {} --reviewer {} --model {}",
            request.feature, request.reviewer.harness, request.reviewer.model
        );
        if let Some(thinking) = &request.reviewer.thinking {
            plain("reviewer thinking", thinking)?;
            prompt.push_str(" --thinking ");
            prompt.push_str(thinking);
        }
        if let Some(context) = request.context.map(checked_context).transpose()?.flatten() {
            prompt.push_str("\n\nExtra context:\n");
            prompt.push_str(context);
        }
        if let Some(marker) = request.exit_marker {
            plain("exit marker", marker)?;
            prompt.push_str("\n\nAs your very last step, once everything is committed, create the empty file ");
            prompt.push_str(marker);
            prompt.push_str(" and then stop.");
        }
        let mut argv = vec!["--model".to_string(), request.implementer.model.clone()];
        if let Some(thinking) = &request.implementer.thinking {
            plain("implementer thinking", thinking)?;
            argv.push("--effort".to_string());
            argv.push(thinking.clone());
        }
        argv.push(prompt);
        Ok(argv)
    }
}

struct Pi;

impl HarnessAdapter for Pi {
    fn name(&self) -> &'static str {
        "pi"
    }

    fn capabilities(&self) -> &'static [Capability] {
        &[Capability::PrintMode]
    }

    fn implement_argv(&self, _request: &ImplementRequest<'_>) -> Result<Vec<String>, AdapterError> {
        Err(AdapterError::Unsupported { adapter: "pi", what: "implementing features" })
    }
}
