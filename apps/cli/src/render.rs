//! Turning events into terminal output.
//!
//! Presentation only. If a line here needs a fact the event does not carry,
//! the fix is a richer event, not a lookup from the CLI — the interface and
//! this renderer must be able to say the same things from the same record.

use tramontana_core::contracts::{EventEnvelope, EventPayload, StreamFrame};

/// Emit one application value as stable, inspectable JSON.
pub(crate) fn json(value: &impl serde::Serialize) -> std::process::ExitCode {
    match serde_json::to_string_pretty(value) {
        Ok(serialized) => {
            println!("{serialized}");
            std::process::ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: could not serialize result: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// Render a normalized application error at the adapter boundary.
pub(crate) fn application_error(
    error: &tramontana_core::contracts::ErrorEnvelope,
) -> std::process::ExitCode {
    eprintln!("error: {:?}: {}", error.code, error.message);
    std::process::ExitCode::FAILURE
}

/// Render one durable event as a single line.
#[must_use]
pub(crate) fn event_line(event: &EventEnvelope) -> String {
    let detail = match &event.payload {
        EventPayload::TaskCreated { goal_summary, .. } => format!("goal: {goal_summary}"),
        EventPayload::TaskStateChanged {
            previous,
            current,
            reason_code,
        } => {
            format!("{previous:?} -> {current:?} ({reason_code})")
        }
        EventPayload::WorkerSelected {
            display_name,
            agent_id,
        } => {
            format!("{display_name} [{agent_id}]")
        }
        EventPayload::ModelStarted { model_id, .. } => format!("model {model_id}"),
        EventPayload::ConfigurationResolved { configuration } => {
            format!("manifest {}", configuration.manifest_hash)
        }
        EventPayload::MessageCommitted { message } => {
            format!(
                "{:?} message with {} part(s)",
                message.role,
                message.parts.len()
            )
        }
        EventPayload::ToolRequested {
            tool_id,
            arguments_summary,
            risk_class,
            ..
        } => {
            format!("{tool_id} ({risk_class:?}) — {arguments_summary}")
        }
        EventPayload::ToolCompleted {
            display_summary,
            side_effect,
            truncated,
            ..
        } => {
            let suffix = if *truncated { " (truncated)" } else { "" };
            format!("{display_summary} [{side_effect:?}]{suffix}")
        }
        EventPayload::TaskCompleted { summary } => summary.clone(),
        EventPayload::TaskFailed { error } => format!("{:?}: {}", error.code, error.message),
        EventPayload::TaskCancelled { origin } => format!("cancelled by {origin:?}"),
        // `EventPayload` is non-exhaustive, so this arm is mandatory rather
        // than defensive: a newer core may emit kinds this build has never
        // heard of, and the contract says show them and carry on
        // (`CORE_CONTRACTS.md` §2). The compiler enforces that we did.
        EventPayload::Unsupported | _ => "(unsupported event kind)".to_owned(),
    };

    format!(
        "{:>3}  {:<22}  {}",
        event.sequence,
        format!("{:?}", event.kind),
        detail
    )
}

/// Render one ephemeral stream frame.
///
/// Marked `~` rather than given a sequence, because a frame has none: it is
/// transport, it is never persisted, and a missing one is not data loss.
#[must_use]
pub(crate) fn frame_line(frame: &StreamFrame) -> String {
    format!("  ~  {:<22}  {}", "streamFrame", frame.text)
}
