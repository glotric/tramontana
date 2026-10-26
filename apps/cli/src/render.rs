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
/// Print a service result: the value as JSON, or the error.
pub(crate) fn result(
    result: Result<impl serde::Serialize, tramontana_core::contracts::ErrorEnvelope>,
) -> std::process::ExitCode {
    match result {
        Ok(value) => json(&value),
        Err(error) => application_error(&error),
    }
}

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
        EventPayload::TaskCreated {
            goal_summary,
            attachments,
            ..
        } => {
            if attachments.is_empty() {
                format!("goal: {goal_summary}")
            } else {
                let names: Vec<_> = attachments.iter().map(|file| file.name.as_str()).collect();
                format!("goal: {goal_summary} (attached: {})", names.join(", "))
            }
        }
        EventPayload::TaskStateChanged {
            previous,
            current,
            reason_code,
        } => {
            format!("{previous:?} -> {current:?} ({reason_code})")
        }
        EventPayload::WorkerSelected { descriptor } => {
            format!("{} [{}]", descriptor.display_name, descriptor.agent_id)
        }
        EventPayload::ContextPrepared { manifest } => {
            let withheld = manifest.exclusions().count();
            format!(
                "{} source(s), {} byte(s) to {:?}{}",
                manifest.sources.len(),
                manifest.total_bytes,
                manifest.destination_class,
                if withheld == 0 {
                    String::new()
                } else {
                    format!(", {withheld} withheld")
                }
            )
        }
        EventPayload::RoutingDecided { decision } => format!(
            "{} ({}){}",
            decision.model.model_id,
            decision.reason_codes.join(", "),
            if decision.requires_disclosure {
                " — LEAVES THE DEVICE"
            } else {
                ""
            }
        ),
        EventPayload::ContextReleased { plan } => {
            let classes: Vec<String> = plan
                .data_classes
                .iter()
                .map(|class| format!("{class:?}").to_lowercase())
                .collect();
            format!(
                "{} byte(s) of {} context to {} ({}){}{} — {}",
                plan.total_bytes,
                classes.join(", "),
                plan.destination.model_id,
                plan.destination.provider_id,
                if plan.requires_disclosure {
                    " — LEFT THE DEVICE"
                } else {
                    ""
                },
                {
                    // A redaction is the visible half of "your key did not
                    // go". Counting it silently would be the same as not
                    // doing it, from the reader's side.
                    let mut notes = Vec::new();
                    if !plan.exclusions.is_empty() {
                        notes.push(format!("{} withheld", plan.exclusions.len()));
                    }
                    if !plan.redactions.is_empty() {
                        notes.push(format!("redacted: {}", plan.redactions.join(", ")));
                    }
                    if notes.is_empty() {
                        String::new()
                    } else {
                        format!(", {}", notes.join(", "))
                    }
                },
                plan.authorization
                    .as_ref()
                    .map_or("proposed", |authority| authority.reason_code.as_str()),
            )
        }
        EventPayload::CheckpointSaved {
            checkpoint_id,
            completed_step_count,
            resume,
            ..
        } => format!(
            "{checkpoint_id} — {completed_step_count} step(s) done, resume {}",
            if resume.eligible {
                "offered"
            } else {
                "refused"
            }
        ),
        EventPayload::DiagnosticRecorded { code, summary } => format!("{code}: {summary}"),
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
        EventPayload::PermissionDecided {
            capability,
            allowed,
            reason_code,
            origin,
            ..
        } => {
            let verdict = if *allowed { "allowed" } else { "DENIED" };
            format!(
                "{capability} {verdict} for {} ({reason_code})",
                describe_origin(origin)
            )
        }
        EventPayload::ToolStarted { tool_id, .. } => format!("running {tool_id}"),
        EventPayload::ToolFailed { error, .. } => {
            format!("{:?}: {}", error.code, error.message)
        }
        EventPayload::ToolCompleted {
            display_summary,
            side_effect,
            truncated,
            ..
        } => {
            let suffix = if *truncated { " (truncated)" } else { "" };
            format!("{display_summary}{}{suffix}", describe_effect(*side_effect))
        }
        EventPayload::ArtifactRecorded { artifact } => artifact.summary(),
        EventPayload::TaskCompleted { summary, .. } => summary.clone(),
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

/// Who asked for a permission, in words rather than as an internal value.
fn describe_origin(origin: &tramontana_core::contracts::PermissionOrigin) -> String {
    use tramontana_core::contracts::PermissionOrigin;
    match origin {
        PermissionOrigin::User => "you".to_owned(),
        PermissionOrigin::Worker { agent_id } => format!("worker {agent_id}"),
        PermissionOrigin::Workflow { workflow_id } => format!("workflow {workflow_id}"),
        // A newer core may name an origin this build does not know.
        _ => "an origin this build does not know".to_owned(),
    }
}

/// What a tool's result changed, as a short note; nothing for a read.
fn describe_effect(effect: tramontana_core::contracts::SideEffect) -> &'static str {
    use tramontana_core::contracts::SideEffect;
    match effect {
        SideEffect::None => "",
        SideEffect::LocalReversible => " [changed files; can be undone]",
        SideEffect::LocalIrreversible => " [changed files; cannot be undone]",
        SideEffect::External => " [changed something off this device]",
    }
}

#[cfg(test)]
mod tests {
    use tramontana_core::contracts::{PermissionOrigin, SideEffect};

    use super::{describe_effect, describe_origin};

    #[test]
    fn an_origin_is_named_in_words() {
        // V3 printed `Worker { agent_id: "builtin.reference" }` here.
        let worker = PermissionOrigin::Worker {
            agent_id: "builtin.reference".to_owned(),
        };
        assert_eq!(describe_origin(&worker), "worker builtin.reference");
        assert_eq!(describe_origin(&PermissionOrigin::User), "you");
    }

    #[test]
    fn a_read_adds_no_note_and_a_write_says_what_it_changed() {
        // V3 printed `[None]` after every read.
        assert_eq!(describe_effect(SideEffect::None), "");
        assert_eq!(
            describe_effect(SideEffect::LocalReversible),
            " [changed files; can be undone]"
        );
    }
}
