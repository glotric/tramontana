//! Assembling a handoff from canonical records.
//!
//! `PROJECT_FORMAT.md` §4: a checkpoint is sufficient to explain and safely
//! continue work *without depending on a worker's private session*. That is a
//! claim about a boundary, so this module is written to make crossing it
//! impossible rather than discouraged: nothing here reads a worker session,
//! and the one field that could carry one — `sessionStateRef` — is recorded as
//! an omission instead of being passed along.
//!
//! What a receiving worker gets is the goal, the normalized history, what is
//! already done, and what was in flight. If that turns out not to be enough,
//! the honest conclusion is that the canonical-context claim needs narrowing —
//! which is precisely what validation gate V2 exists to find out.

use crate::contracts::{
    EventEnvelope, EventPayload, HandoffBundle, HandoffEntry, Omission, OmissionCategory,
    TaskCheckpoint, TaskRecord, Timestamp, Visibility,
};

/// Build a handoff for a task at the point `checkpoint` describes.
///
/// Events after the checkpoint's sequence are ignored: the handoff describes
/// the point being handed over, not whatever happened afterwards.
#[must_use]
pub fn build_handoff(
    record: &TaskRecord,
    events: &[EventEnvelope],
    checkpoint: &TaskCheckpoint,
    created_at: Timestamp,
) -> HandoffBundle {
    let mut handoff = HandoffBundle::new(
        record.task_id.clone(),
        record.goal_summary.clone(),
        created_at,
    );
    handoff.project_id = record.project_id.clone();
    handoff.conversation_id = record.conversation_id.clone();
    handoff.event_sequence = checkpoint.event_sequence;
    handoff.completed_step_ids = checkpoint.completed_step_ids.clone();
    handoff.pending_action = checkpoint.pending_action.clone();
    handoff.configuration_manifest_hash = checkpoint.configuration_manifest_hash.clone();
    handoff.previous_worker = checkpoint
        .worker
        .as_ref()
        .map(|worker| worker.descriptor.clone());

    let mut internal = 0u64;
    for event in events
        .iter()
        .filter(|event| event.sequence <= checkpoint.event_sequence)
    {
        if matches!(event.visibility, Visibility::Secret) {
            internal += 1;
            continue;
        }
        if let Some(summary) = summarize(&event.payload) {
            handoff.history.push(HandoffEntry {
                sequence: event.sequence,
                occurred_at: event.occurred_at.clone(),
                kind: format!("{:?}", event.kind),
                summary,
            });
        }
    }

    if internal > 0 {
        handoff.omissions.push(Omission {
            category: OmissionCategory::InternalEvent,
            reason: "Internal audit markers are not part of a worker's context.".to_owned(),
            count: internal,
        });
    }
    if checkpoint
        .worker
        .as_ref()
        .is_some_and(|worker| worker.session_state_ref.is_some())
    {
        handoff.omissions.push(Omission {
            category: OmissionCategory::WorkerPrivateState,
            reason: "The previous worker's continuation data is deliberately not carried; this handoff is built from canonical state alone.".to_owned(),
            count: 1,
        });
    }

    handoff
}

/// One readable line for an event, or nothing when it adds no narrative.
///
/// State changes are omitted on purpose: a receiving worker needs to know what
/// *happened*, and a list of internal lifecycle transitions is noise it would
/// have to filter anyway.
fn summarize(payload: &EventPayload) -> Option<String> {
    match payload {
        EventPayload::TaskCreated { goal_summary, .. } => Some(format!("Goal: {goal_summary}")),
        EventPayload::WorkerSelected { descriptor } => Some(format!(
            "{} took the task ({:?} tool control, {:?} network).",
            descriptor.display_name, descriptor.tool_control, descriptor.network_control
        )),
        EventPayload::ContextPrepared { manifest } => {
            let withheld = manifest.exclusions().count();
            Some(format!(
                "Context assembled for a {:?} destination: {} source(s), {} byte(s) released{}.",
                manifest.destination_class,
                manifest.sources.len(),
                manifest.total_bytes,
                if withheld == 0 {
                    String::new()
                } else {
                    format!(", {withheld} withheld")
                }
            ))
        }
        EventPayload::RoutingDecided { decision } => Some(format!(
            "Routed to {}/{} ({}){}.",
            decision.model.provider_id,
            decision.model.model_id,
            decision.reason_codes.join(", "),
            if decision.requires_disclosure {
                " — this left the device"
            } else {
                ""
            }
        )),
        EventPayload::MessageCommitted { message } => {
            let text: String = message
                .parts
                .iter()
                .filter_map(|part| match part {
                    crate::contracts::MessagePart::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" ");
            Some(format!("{:?}: {text}", message.role))
        }
        EventPayload::ToolRequested {
            tool_id,
            arguments_summary,
            risk_class,
            ..
        } => Some(format!(
            "Asked to run {tool_id} ({risk_class:?}) with {arguments_summary}."
        )),
        EventPayload::PermissionDecided {
            capability,
            allowed,
            reason_code,
            ..
        } => Some(format!(
            "{capability} was {} ({reason_code}).",
            if *allowed { "allowed" } else { "refused" }
        )),
        EventPayload::ToolCompleted {
            display_summary,
            side_effect,
            truncated,
            ..
        } => Some(format!(
            "Result: {display_summary} [{side_effect:?}]{}",
            if *truncated { " (truncated)" } else { "" }
        )),
        EventPayload::ToolFailed { error, .. } => {
            Some(format!("Tool failed: {:?}: {}", error.code, error.message))
        }
        EventPayload::DiagnosticRecorded { code, summary } => Some(format!("{code}: {summary}")),
        EventPayload::ArtifactRecorded { artifact } => Some(format!("{}.", artifact.summary())),
        EventPayload::TaskCompleted { summary, .. } => Some(format!("Completed: {summary}")),
        EventPayload::TaskFailed { error } => {
            Some(format!("Failed: {:?}: {}", error.code, error.message))
        }
        EventPayload::TaskCancelled { origin } => Some(format!("Cancelled by {origin:?}.")),
        _ => None,
    }
}
