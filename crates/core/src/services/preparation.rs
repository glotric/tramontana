//! Assembling the context a task runs against.
//!
//! A fake with a real shape, which is deliberate:
//! `BUILD_ORDER.md` §12 puts checkpoint and export here precisely so that the
//! context manifest and routing decision records — the most expensive shapes
//! in the system to change late — are exercised by the durable stream, the
//! checkpoint, and the export bundle before eight more milestones depend on
//! them. M5 replaced the routing half with a real deterministic router
//! (`services::routing`); real context assembly — selected files, repository
//! metadata, token budgets — arrives with the work that produces them.
//!
//! What is honest about it today: the manifest genuinely describes what the
//! worker is given. It does not invent a source that does not exist.

use crate::contracts::{
    ContextCategory, ContextManifest, ContextSource, DataClass, DestinationClass,
    EffectiveConfiguration, TaskId, Timestamp,
};

/// Assemble the context a task will run against.
///
/// The goal is the one source that always exists. A project task also carries
/// its effective configuration, named by the manifest hash the task already
/// records — the same value, so the two records cannot drift.
#[must_use]
pub fn assemble_context(
    context_manifest_id: impl Into<String>,
    task_id: TaskId,
    goal: &str,
    goal_redactions: Vec<String>,
    configuration: Option<&EffectiveConfiguration>,
    created_at: Timestamp,
) -> ContextManifest {
    // `goal` is already redacted — `TaskService::start_task` does it, so no
    // durable record ever held the original. The codes travel separately
    // because redacting twice would report nothing: the second pass has
    // nothing left to find, and the manifest would say a credential-free goal
    // had always been credential-free.
    debug_assert!(
        crate::services::redact(goal).text == goal,
        "the goal reached context assembly without being redacted at ingestion"
    );
    let mut sources = vec![ContextSource::redacted(
        "conversation.goal",
        ContextCategory::Conversation,
        DataClass::Project,
        goal,
        goal_redactions,
    )];

    if let Some(configuration) = configuration {
        for source in &configuration.sources {
            // Named by its project-relative key and its content hash, never by
            // its body: instruction text is project data, and this record is
            // displayed and exported.
            sources.push(ContextSource {
                source_id: source.source_id.clone(),
                category: category_of(source.kind),
                data_class: DataClass::Project,
                content_hash: source.content_hash.clone(),
                byte_count: source.byte_count,
                token_estimate: None,
                included: source.active,
                // An inactive source was gathered and deliberately not used;
                // saying so is the difference between a manifest and a list.
                exclusion_reason: (!source.active).then(|| "context.notActive".to_owned()),
                // Carried from resolution, where the file was redacted
                // before it was hashed. The hash below therefore names text
                // that may be released, and prompt assembly in M6 cannot send
                // the original and still match the record that describes it —
                // the release boundary refuses content no source accounts for.
                redactions: source.redactions.clone(),
            });
        }
    }

    ContextManifest::new(
        context_manifest_id,
        task_id,
        // Everything M4 runs is a local fake. M5 decides this per destination.
        DestinationClass::Local,
        sources,
        created_at,
    )
}

/// Which context category a configuration source belongs to.
///
/// Skills are their own category in `CORE_CONTRACTS.md` §10; everything else
/// a project authors is instruction content.
fn category_of(kind: crate::contracts::ConfigurationKind) -> ContextCategory {
    match kind {
        crate::contracts::ConfigurationKind::Skill => ContextCategory::Skill,
        _ => ContextCategory::Instruction,
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert on known-good literals; a panic is the failure report"
)]
mod tests {
    use super::*;

    fn at() -> Timestamp {
        Timestamp::new("2026-01-01T00:00:00.000Z")
    }

    #[test]
    fn the_goal_is_always_a_source() {
        let manifest = assemble_context(
            "context-1",
            TaskId::new("task-1"),
            "say hello",
            Vec::new(),
            None,
            at(),
        );
        assert_eq!(manifest.sources.len(), 1);
        assert_eq!(manifest.sources[0].category, ContextCategory::Conversation);
        assert!(manifest.sources[0].included);
        assert_eq!(manifest.total_bytes, "say hello".len() as u64);
    }

    #[test]
    fn what_was_redacted_from_the_goal_reaches_the_manifest() {
        // The codes travel with the already-redacted text. Recomputing them
        // here would find nothing, and the manifest would report that a goal
        // which did carry a credential never had one.
        let manifest = assemble_context(
            "context-1",
            TaskId::new("task-1"),
            "call with [redacted]",
            vec![crate::services::redaction_reason::PROVIDER_API_KEY.to_owned()],
            None,
            at(),
        );
        assert_eq!(
            manifest.redactions,
            vec![crate::services::redaction_reason::PROVIDER_API_KEY]
        );
        assert_eq!(
            manifest.sources[0].redactions,
            vec![crate::services::redaction_reason::PROVIDER_API_KEY]
        );
    }

    #[test]
    fn the_manifest_names_instruction_sources_without_copying_them() {
        let configuration = crate::config::resolve_with_selection(
            vec![crate::config::RawSource {
                relative_path: "AGENTS.md".to_owned(),
                kind: crate::contracts::ConfigurationKind::RootAgents,
                content: b"# Rules\n\nBe careful with the database password.".to_vec(),
            }],
            &crate::config::ConfigurationSelection::default(),
            at(),
        );

        let manifest = assemble_context(
            "context-1",
            TaskId::new("task-1"),
            "say hello",
            Vec::new(),
            Some(&configuration),
            at(),
        );
        let rendered = serde_json::to_string(&manifest).unwrap();
        assert!(
            !rendered.contains("database password"),
            "the manifest copied instruction content it was supposed to describe"
        );
        assert!(
            manifest
                .sources
                .iter()
                .any(|source| source.source_id == "AGENTS.md")
        );
    }
}
