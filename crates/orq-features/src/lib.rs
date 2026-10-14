use orq_core::FeatureStatus;
use serde::Deserialize;

/// One entry of a project's `.tdd-features.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeatureEntry {
    pub feature_name: String,
    pub description: Option<String>,
    pub depends_on: Vec<String>,
    pub status: FeatureStatus,
    pub test_file: Option<String>,
    pub target_files: Vec<String>,
    pub recorded_at: Option<String>,
    pub cycles_completed: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum RawStatus {
    Draft,
    Pending,
    InProgress,
    Completed,
    Abandoned,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawEntry {
    feature_name: String,
    description: Option<String>,
    #[serde(default)]
    depends_on: Vec<String>,
    status: RawStatus,
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
            status: match raw.status {
                RawStatus::Draft => FeatureStatus::Draft,
                RawStatus::Pending => FeatureStatus::Pending,
                RawStatus::InProgress => FeatureStatus::InProgress,
                RawStatus::Completed => FeatureStatus::Completed,
                RawStatus::Abandoned => FeatureStatus::Abandoned,
            },
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
