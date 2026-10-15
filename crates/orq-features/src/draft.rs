use serde_json::{json, Value};

/// A new feature as the user describes it: only a name is required, and only
/// the fields given are ever written, so a draft never gains default content.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FeatureDraft {
    pub feature_name: String,
    pub description: Option<String>,
    pub depends_on: Option<Vec<String>>,
    pub parallelizable: Option<bool>,
    pub test_file: Option<String>,
    pub target_files: Option<Vec<String>>,
}

impl FeatureDraft {
    pub fn new(feature_name: impl Into<String>) -> Self {
        FeatureDraft { feature_name: feature_name.into(), ..Default::default() }
    }

    /// The entry as compact JSON, with status `draft`, in ledger key order.
    pub fn to_json(&self) -> String {
        let mut fields: Vec<(&str, Value)> = vec![("featureName", json!(self.feature_name))];
        if let Some(description) = &self.description {
            fields.push(("description", json!(description)));
        }
        if let Some(depends_on) = &self.depends_on {
            fields.push(("dependsOn", json!(depends_on)));
        }
        fields.push(("status", json!("draft")));
        if let Some(test_file) = &self.test_file {
            fields.push(("testFile", json!(test_file)));
        }
        if let Some(target_files) = &self.target_files {
            fields.push(("targetFiles", json!(target_files)));
        }
        if let Some(parallelizable) = self.parallelizable {
            fields.push(("parallelizable", json!(parallelizable)));
        }
        let body: Vec<String> = fields.iter().map(|(key, value)| format!("\"{key}\":{value}")).collect();
        format!("{{{}}}", body.join(","))
    }
}
