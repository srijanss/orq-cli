//! A project's design preparer, design reviewer and design verifier: each an optional agent kept in orq.db's
//! project settings like the implementer and reviewer, checked as a design role (pi or claude, a model that
//! accepts images, a thinking level its harness takes), and resolved for a frontend run with fallbacks.

use std::path::{Path, PathBuf};

use orq_core::project_settings::{ProjectAgent, ProjectSettings, SaveOutcome};
use orq_core::store::Store;

fn scratch(tag: &str) -> PathBuf {
    let dir = orq_test_support::process_temp_dir()
        .join(format!("orq-design-agents-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("orq.db")
}

fn agent(harness: &str, model: &str, effort: Option<&str>) -> ProjectAgent {
    ProjectAgent {
        harness: harness.to_string(),
        model: model.to_string(),
        effort: effort.map(str::to_string),
    }
}

#[test]
fn each_design_role_round_trips_through_the_store_and_is_unset_by_default() {
    let db = scratch("round-trip");
    let root = Path::new("/work/alpha");
    let mut store = Store::open(&db).unwrap();
    let fresh = store.project_settings(root).unwrap().settings;
    assert_eq!(
        (
            fresh.design_preparer,
            fresh.design_reviewer,
            fresh.design_verifier
        ),
        (None, None, None)
    );

    let chosen = ProjectSettings {
        design_preparer: Some(agent("pi", "openai/gpt-6.1-sol", Some("medium"))),
        design_reviewer: Some(agent("pi", "openai/gpt-6.1-terra", None)),
        design_verifier: Some(agent("claude", "opus", Some("high"))),
        ..ProjectSettings::default()
    };
    assert_eq!(
        store.save_project_settings(root, 0, &chosen).unwrap(),
        SaveOutcome::Saved { revision: 1 }
    );
    drop(store);
    let store = Store::open(&db).unwrap();
    assert_eq!(store.project_settings(root).unwrap().settings, chosen);
    assert_eq!(
        Store::read_project_settings(&db, root).unwrap().settings,
        chosen,
        "the read-only lookup reads them too"
    );
}

/// What `pi --list-models` prints: a header naming the `images` column, then one row per model.
const PI_MODELS: &str = "provider  model          context  max-out  thinking  images\nopenai    gpt-6.1-sol    400K     128K     yes       yes\nkimi      kimi-text      128K     32K      no        no\n";
const PI_LEVELS: &[&str] = &["off", "minimal", "low", "medium", "high", "xhigh", "max"];

#[test]
fn a_design_role_must_use_pi_or_claude_a_model_that_accepts_images_and_a_level_its_harness_takes() {
    use orq_core::project_settings::design_agent_refusal;
    let check = |agent: &ProjectAgent| {
        design_agent_refusal("Design reviewer", agent, PI_LEVELS, Some(PI_MODELS))
    };
    assert_eq!(
        check(&agent("pi", "openai/gpt-6.1-sol", Some("medium"))),
        None
    );
    assert_eq!(
        check(&agent("pi", "openai/gpt-6.1-sol", None)),
        None,
        "no level leaves the harness's own"
    );
    assert_eq!(
        design_agent_refusal(
            "Design reviewer",
            &agent("claude", "opus", Some("high")),
            &["low", "high"],
            None
        ),
        None,
        "every current claude model accepts images"
    );

    assert_eq!(
        check(&agent("pi", "kimi/kimi-text", None)),
        Some("kimi/kimi-text doesn't accept images, which the Design reviewer needs".to_string())
    );
    assert_eq!(
        check(&agent("pi", "openai/gpt-7", None)),
        Some("pi doesn't list openai/gpt-7".to_string())
    );
    assert_eq!(
        check(&agent("pi", "openai/gpt-6.1-sol", Some("turbo"))),
        Some("turbo is not a thinking level pi takes".to_string())
    );
    assert_eq!(
        check(&agent("codex", "gpt-5", None)),
        Some(
            "The Design reviewer must use pi or claude, which Orq runs read-only for review"
                .to_string()
        )
    );
    assert_eq!(
        design_agent_refusal(
            "Design reviewer",
            &agent("pi", "openai/gpt-6.1-sol", None),
            PI_LEVELS,
            None
        ),
        Some("pi's models could not be listed".to_string())
    );
}

fn pi_levels(harness: &str) -> &'static [&'static str] {
    match harness {
        "pi" => PI_LEVELS,
        "claude" => &["low", "medium", "high", "xhigh", "max"],
        _ => &[],
    }
}

#[test]
fn a_frontend_run_needs_a_design_reviewer_and_the_verifier_and_preparer_fall_back_to_it() {
    use orq_core::project_settings::{design_agents, DesignAgents, NO_DESIGN_REVIEWER};
    let reviewer = agent("pi", "openai/gpt-6.1-sol", Some("medium"));
    let none = ProjectSettings {
        implementer: Some(agent("claude", "opus", None)),
        reviewer: Some(agent("pi", "openai/gpt-6.1-sol", None)),
        ..ProjectSettings::default()
    };
    assert_eq!(
        design_agents(&none, pi_levels, Some(PI_MODELS)),
        Err(NO_DESIGN_REVIEWER.to_string()),
        "the code reviewer is never used as the design reviewer"
    );
    assert_eq!(NO_DESIGN_REVIEWER, "set a design reviewer in Settings");

    let only_reviewer = ProjectSettings {
        design_reviewer: Some(reviewer.clone()),
        ..none.clone()
    };
    assert_eq!(
        design_agents(&only_reviewer, pi_levels, Some(PI_MODELS)),
        Ok(DesignAgents {
            preparer: reviewer.clone(),
            reviewer: reviewer.clone(),
            verifier: agent("pi", "openai/gpt-6.1-sol", Some("high"))
        }),
        "no preparer is the design reviewer, no verifier is the design reviewer at thinking high"
    );

    let all = ProjectSettings {
        design_preparer: Some(agent("claude", "sonnet", Some("low"))),
        design_verifier: Some(agent("claude", "opus", Some("max"))),
        ..only_reviewer.clone()
    };
    let resolved = design_agents(&all, pi_levels, Some(PI_MODELS)).unwrap();
    assert_eq!(
        (resolved.preparer, resolved.verifier),
        (
            agent("claude", "sonnet", Some("low")),
            agent("claude", "opus", Some("max"))
        )
    );

    let refused = ProjectSettings {
        design_verifier: Some(agent("pi", "kimi/kimi-text", None)),
        ..only_reviewer
    };
    assert_eq!(
        design_agents(&refused, pi_levels, Some(PI_MODELS)),
        Err("kimi/kimi-text doesn't accept images, which the Design verifier needs".to_string()),
        "checked again when the run starts"
    );
}

/// A database at schema 16, the last before design roles, with one project's implementer saved.
fn database_before_design_roles(db: &Path, root: &Path) {
    let chosen = ProjectSettings {
        implementer: Some(agent("claude", "sonnet", Some("low"))),
        ..ProjectSettings::default()
    };
    Store::open(db)
        .unwrap()
        .save_project_settings(root, 0, &chosen)
        .unwrap();
    let conn = rusqlite::Connection::open(db).unwrap();
    for role in ["design_preparer", "design_reviewer", "design_verifier"] {
        for field in ["harness", "model", "effort"] {
            conn.execute_batch(&format!(
                "ALTER TABLE project_settings DROP COLUMN {role}_{field};"
            ))
            .unwrap();
        }
    }
    conn.execute_batch("UPDATE schema_compat SET version = 16;")
        .unwrap();
    conn.pragma_update(None, "user_version", 16).unwrap();
}

#[test]
fn an_older_database_migrates_with_the_design_roles_unset_and_keeps_the_rest() {
    let db = scratch("migrate");
    let root = Path::new("/work/alpha");
    database_before_design_roles(&db, root);

    let read = Store::read_project_settings(&db, root).unwrap();
    assert_eq!(
        (
            read.settings.design_preparer,
            read.settings.design_reviewer,
            read.settings.design_verifier
        ),
        (None, None, None),
        "read without migrating"
    );
    let version: u32 = rusqlite::Connection::open(&db)
        .unwrap()
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 16, "the read-only lookup migrated nothing");

    let mut store = Store::open(&db).unwrap();
    let migrated = store.project_settings(root).unwrap();
    assert_eq!(
        migrated.settings.implementer,
        Some(agent("claude", "sonnet", Some("low"))),
        "the rest is kept"
    );
    let roles = (
        migrated.settings.design_preparer.clone(),
        migrated.settings.design_reviewer.clone(),
        migrated.settings.design_verifier.clone(),
    );
    assert_eq!(roles, (None, None, None));
    let with_reviewer = ProjectSettings {
        design_reviewer: Some(agent("pi", "openai/gpt-6.1-terra", Some("medium"))),
        ..migrated.settings
    };
    assert_eq!(
        store
            .save_project_settings(root, migrated.revision, &with_reviewer)
            .unwrap(),
        SaveOutcome::Saved {
            revision: migrated.revision + 1
        }
    );
    assert_eq!(
        store.project_settings(root).unwrap().settings,
        with_reviewer
    );
}
