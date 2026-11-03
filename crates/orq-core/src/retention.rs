//! How long Orq keeps what it stores, and which runs may be pruned.

/// Days to keep each class of artifact after its run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionPolicy {
    pub merged_days: u32,
    pub closed_days: u32,
    pub failed_days: u32,
    pub logs_days: u32,
    pub temporary_days: u32,
    pub checkpoint_days: u32,
    pub metadata_days: u32,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        RetentionPolicy { merged_days: 90, closed_days: 90, failed_days: 30, logs_days: 14, temporary_days: 7, checkpoint_days: 30, metadata_days: 180 }
    }
}

impl RetentionPolicy {
    /// Reads the `[retention]` table of a config file; keys left out keep their
    /// defaults. Active runs and unresolved reviews are always protected, so
    /// switching that protection off is refused rather than honoured.
    pub fn parse(text: &str) -> Result<RetentionPolicy, String> {
        let doc: toml_edit::DocumentMut = text.parse().map_err(|err| format!("the config file is not valid TOML: {err}"))?;
        let mut policy = RetentionPolicy::default();
        let Some(item) = doc.get("retention") else { return Ok(policy) };
        let table = item.as_table_like().ok_or("[retention] must be a table")?;
        for (key, value) in table.iter() {
            let days = |slot: &mut u32| -> Result<(), String> {
                let n = value.as_integer().ok_or_else(|| format!("{key} must be a whole number of days"))?;
                *slot = u32::try_from(n).map_err(|_| format!("{key} must be between 0 and {}", u32::MAX))?;
                Ok(())
            };
            match key {
                "merged_days" => days(&mut policy.merged_days)?,
                "closed_days" => days(&mut policy.closed_days)?,
                "failed_days" => days(&mut policy.failed_days)?,
                "logs_days" => days(&mut policy.logs_days)?,
                "temporary_days" => days(&mut policy.temporary_days)?,
                "checkpoint_days" => days(&mut policy.checkpoint_days)?,
                "metadata_days" => days(&mut policy.metadata_days)?,
                "protect_active" | "protect_unresolved_reviews" => {
                    if value.as_bool() != Some(true) {
                        return Err(format!("{key} cannot be turned off: such work is always protected"));
                    }
                }
                other => return Err(format!("{other} is not a retention setting")),
            }
        }
        Ok(policy)
    }
}
