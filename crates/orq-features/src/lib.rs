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
