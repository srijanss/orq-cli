use orq_core::FeatureStatus;
use serde::Deserialize;

/// One entry of a project's `.tdd-features.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeatureEntry {
    pub feature_name: String,
    pub description: Option<String>,
    pub depends_on: Vec<String>,
    pub status: LedgerStatus,
    pub test_file: Option<String>,
    pub target_files: Vec<String>,
    pub recorded_at: Option<String>,
    pub cycles_completed: Option<u64>,
}

/// A ledger status: one Orq knows, or the verbatim text of one it does not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LedgerStatus {
    Known(FeatureStatus),
    Unrecognised(String),
}

impl LedgerStatus {
    /// The known status, or `None` for an unrecognised one (never transitionable).
    pub fn known(&self) -> Option<FeatureStatus> {
        match self {
            LedgerStatus::Known(status) => Some(*status),
            LedgerStatus::Unrecognised(_) => None,
        }
    }

    fn from_text(text: String) -> Self {
        match text.as_str() {
            "draft" => LedgerStatus::Known(FeatureStatus::Draft),
            "pending" => LedgerStatus::Known(FeatureStatus::Pending),
            "in_progress" => LedgerStatus::Known(FeatureStatus::InProgress),
            "completed" => LedgerStatus::Known(FeatureStatus::Completed),
            "abandoned" => LedgerStatus::Known(FeatureStatus::Abandoned),
            _ => LedgerStatus::Unrecognised(text),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawEntry {
    feature_name: String,
    description: Option<String>,
    #[serde(default)]
    depends_on: Vec<String>,
    status: String,
    test_file: Option<String>,
    #[serde(default)]
    target_files: Vec<String>,
    recorded_at: Option<String>,
    cycles_completed: Option<u64>,
}

impl From<RawEntry> for FeatureEntry {
    fn from(raw: RawEntry) -> Self {
        FeatureEntry {
            feature_name: raw.feature_name,
            description: raw.description,
            depends_on: raw.depends_on,
            status: LedgerStatus::from_text(raw.status),
            test_file: raw.test_file,
            target_files: raw.target_files,
            recorded_at: raw.recorded_at,
            cycles_completed: raw.cycles_completed,
        }
    }
}

/// Parses the text of a `.tdd-features.json`, keeping entry order.
pub fn parse_ledger(text: &str) -> Result<Vec<FeatureEntry>, serde_json::Error> {
    let raw: Vec<RawEntry> = serde_json::from_str(text)?;
    Ok(raw.into_iter().map(FeatureEntry::from).collect())
}

/// Why a ledger could not be loaded.
#[derive(Debug)]
pub enum LedgerError {
    /// The project has no ledger at the path that was looked for.
    Missing { path: std::path::PathBuf },
    /// The ledger exists but could not be read.
    Unreadable { path: std::path::PathBuf, source: std::io::Error },
    /// The ledger was read but is not a valid feature list.
    Malformed {
        path: std::path::PathBuf,
        reason: String,
        /// 1-based (line, column), for syntax errors only.
        position: Option<(usize, usize)>,
    },
}

/// Loads and validates the ledger at `path`; never writes it.
pub fn load_ledger(path: &std::path::Path) -> Result<Vec<FeatureEntry>, LedgerError> {
    let bytes = std::fs::read(path).map_err(|source| match source.kind() {
        std::io::ErrorKind::NotFound => LedgerError::Missing { path: path.to_path_buf() },
        _ => LedgerError::Unreadable { path: path.to_path_buf(), source },
    })?;
    let malformed = |reason: String, position| LedgerError::Malformed {
        path: path.to_path_buf(),
        reason,
        position,
    };
    // Deserialise straight into `RawEntry`: going through `Value` first would
    // silently keep the last of any repeated key.
    let raw: Vec<RawEntry> = match serde_json::from_slice(&bytes) {
        Ok(raw) => raw,
        Err(e) if e.classify() != serde_json::error::Category::Data => {
            return Err(malformed(e.to_string(), Some((e.line(), e.column()))));
        }
        Err(e) => return Err(malformed(describe_shape_error(&bytes).unwrap_or_else(|| e.to_string()), None)),
    };
    let mut seen = std::collections::HashSet::new();
    for entry in &raw {
        if !seen.insert(entry.feature_name.as_str()) {
            return Err(malformed(
                format!("featureName \"{}\" is repeated", entry.feature_name),
                None,
            ));
        }
    }
    Ok(raw.into_iter().map(FeatureEntry::from).collect())
}

/// Names what is wrong when the text is valid JSON but not a feature list:
/// not an array, or an entry without a string `featureName`.
fn describe_shape_error(bytes: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let Some(items) = value.as_array() else {
        return Some("expected a JSON array of features".to_string());
    };
    let index = items.iter().position(|item| {
        item.get("featureName").and_then(serde_json::Value::as_str).is_none()
    })?;
    Some(format!("entry {index} has no string featureName"))
}
