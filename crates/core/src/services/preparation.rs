//! Assembling context and choosing a route.
//!
//! Both are *fakes with real shapes* in M4, which is deliberate:
//! `BUILD_ORDER.md` §12 puts checkpoint and export here precisely so that the
//! context manifest and routing decision records — the most expensive shapes
//! in the system to change late — are exercised by the durable stream, the
//! checkpoint, and the export bundle before eight more milestones depend on
//! them. M5 replaces the bodies of these two functions with real context
//! assembly and real routing; nothing that reads their output changes.
//!
//! What is honest about them today: the context genuinely describes what the
//! worker is given, and the routing decision genuinely names the only model
//! the process has. Neither invents a source or a provider that does not
//! exist.

use crate::contracts::{
    ContextCategory, ContextManifest, ContextSource, DataClass, DestinationClass,
    EffectiveConfiguration, ExecutionLocation, ModelDescriptor, ModelLocation, RoutedModel,
    RoutingDecision, RoutingMode, TaskId, Timestamp, routing_reason,
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
    configuration: Option<&EffectiveConfiguration>,
    created_at: Timestamp,
) -> ContextManifest {
    let mut sources = vec![ContextSource::included(
        "conversation.goal",
        ContextCategory::Conversation,
        DataClass::Project,
        goal,
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

/// Choose a model and a location for an assembled context.
///
/// With one provider there is nothing to weigh, and the decision says so
/// rather than inventing a preference it did not exercise.
#[must_use]
pub fn choose_route(
    decision_id: impl Into<String>,
    available: &[ModelDescriptor],
    context_manifest_id: &str,
    availability_snapshot_id: impl Into<String>,
) -> Option<RoutingDecision> {
    let chosen = available.first()?;
    let mut reasons = vec![routing_reason::SOLE_ELIGIBLE_ROUTE.to_owned()];
    if available.len() > 1 {
        // More than one was eligible and the first was taken. Until M5 weighs
        // them, saying "sole eligible route" would be false.
        reasons = vec![routing_reason::USER_SELECTED.to_owned()];
    }

    let mut decision = RoutingDecision::new(
        decision_id,
        RoutingMode::Automatic,
        RoutedModel {
            provider_id: chosen.provider_id.clone(),
            model_id: chosen.model_id.clone(),
        },
        location_of(chosen),
        availability_snapshot_id,
        reasons,
        context_manifest_id,
    );
    decision.fallback_order = available
        .iter()
        .skip(1)
        .map(|model| crate::contracts::RoutingFallback {
            provider_id: model.provider_id.clone(),
            model_id: model.model_id.clone(),
            location: location_of(model),
        })
        .collect();

    Some(decision)
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

/// Where a model runs, in the form a routing decision records.
fn location_of(model: &ModelDescriptor) -> ExecutionLocation {
    match model.location {
        ModelLocation::Local => ExecutionLocation::Local {
            target_id: "this-device".to_owned(),
            display_name: "This device".to_owned(),
        },
        ModelLocation::Cloud => ExecutionLocation::Cloud {
            provider_id: model.provider_id.clone(),
            region: None,
        },
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert on known-good literals; a panic is the failure report"
)]
mod tests {
    use crate::contracts::{Health, ModelCapability};

    use super::*;

    fn at() -> Timestamp {
        Timestamp::new("2026-01-01T00:00:00.000Z")
    }

    fn model(model_id: &str, location: ModelLocation) -> ModelDescriptor {
        ModelDescriptor {
            provider_id: "fake".to_owned(),
            model_id: model_id.to_owned(),
            display_name: model_id.to_owned(),
            location,
            capabilities: vec![ModelCapability::Text],
            context_limit: None,
            health: Health::Available,
            health_reason_code: None,
        }
    }

    #[test]
    fn the_goal_is_always_a_source() {
        let manifest =
            assemble_context("context-1", TaskId::new("task-1"), "say hello", None, at());
        assert_eq!(manifest.sources.len(), 1);
        assert_eq!(manifest.sources[0].category, ContextCategory::Conversation);
        assert!(manifest.sources[0].included);
        assert_eq!(manifest.total_bytes, "say hello".len() as u64);
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

    #[test]
    fn one_eligible_model_is_recorded_as_having_no_choice_to_make() {
        let decision = choose_route(
            "decision-1",
            &[model("a", ModelLocation::Local)],
            "context-1",
            "snap-1",
        )
        .unwrap();
        assert_eq!(
            decision.reason_codes,
            vec![routing_reason::SOLE_ELIGIBLE_ROUTE]
        );
        assert!(decision.fallback_order.is_empty());
        assert!(!decision.requires_disclosure);
    }

    #[test]
    fn a_second_model_becomes_a_fallback_and_changes_the_reason() {
        // Claiming "sole eligible route" with two models available would be
        // false, and a reason code nobody can trust is worse than none.
        let decision = choose_route(
            "decision-1",
            &[
                model("a", ModelLocation::Local),
                model("b", ModelLocation::Cloud),
            ],
            "context-1",
            "snap-1",
        )
        .unwrap();
        assert_ne!(
            decision.reason_codes,
            vec![routing_reason::SOLE_ELIGIBLE_ROUTE]
        );
        assert_eq!(decision.fallback_order.len(), 1);
        assert_eq!(decision.fallback_order[0].model_id, "b");
    }

    #[test]
    fn a_cloud_route_requires_disclosure() {
        let decision = choose_route(
            "decision-1",
            &[model("a", ModelLocation::Cloud)],
            "context-1",
            "snap-1",
        )
        .unwrap();
        assert!(decision.requires_disclosure);
        assert!(decision.leaves_the_device());
    }

    #[test]
    fn no_models_is_no_decision_rather_than_an_invented_one() {
        assert!(choose_route("decision-1", &[], "context-1", "snap-1").is_none());
    }
}
