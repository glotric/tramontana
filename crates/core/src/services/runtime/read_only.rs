//! One canonical, budgeted model/observation turn. Strategy repeats this port.
use super::*;
use crate::contracts::{EventEnvelope, MessagePart, TaskRecord};
use crate::ports::{ReadOnlyLimits, ReadOnlyTurn, ToolAdmission};
use crate::services::{
    CanonicalModelHistory, ModelToolCatalogue, ToolInputSchema, prepare_model_context,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

const BUDGET: &str = "reference.budget.v1";
const COMPLETE: &str = "reference.completed.v1";
const TOOL_BYTES: u64 = 262_144;
/// How many calls a turn may ask for. Only the first runs (ADR 0029).
const CALLS_PER_TURN: usize = 8;
const CALLS_NOT_RUN: &str = "reference.callsNotRun";

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Reservation {
    limits: ReadOnlyLimits,
    /// Recorded so a resume cannot change what the model may be offered.
    /// Reservations at M6's level omit it and keep their M6 shape.
    ///
    /// Omitted only when it *is* that level. `is_read_only` is not the test:
    /// a task that admits Apps and nothing else is read-only by that measure,
    /// and omitting its admission read it back without Apps, so its second
    /// turn saw the admission change and interrupted the task.
    #[serde(default, skip_serializing_if = "is_m6_level")]
    admission: ToolAdmission,
    input_bytes: u64,
    output_bytes: u64,
    output_tokens: u64,
    time_ms: u64,
    tools: u64,
}

/// Whether an admission is M6's reads-only level, which reservations omit.
fn is_m6_level(admission: &ToolAdmission) -> bool {
    *admission == ToolAdmission::READ_ONLY
}

impl BrokeredHost {
    pub(super) async fn run_turn(
        &self,
        tools: &[String],
        admission: ToolAdmission,
        limits: &ReadOnlyLimits,
        cancel: &CancellationToken,
    ) -> Result<ReadOnlyTurn, ErrorEnvelope> {
        validate_limits(limits)?;
        let deadline =
            tokio::time::Instant::now() + std::time::Duration::from_millis(limits.turn_ms);
        let _guard = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(error(ErrorCode::Cancelled)),
            () = tokio::time::sleep_until(deadline) => return Err(error(ErrorCode::Timeout)),
            guard = self.model_gate.lock() => guard,
        };
        let events = self.journal.replay().await?;
        if events.len() > 100_000 {
            return Err(error(ErrorCode::LimitExceeded));
        }
        let mut task = self.canonical_task(&events)?;
        task.last_sequence = events.len() as u64;
        let initial = self
            .context
            .as_ref()
            .ok_or_else(|| error(ErrorCode::PermissionDenied))?;
        // Validate the whole canonical prefix even when resuming an incomplete
        // exchange, for which selecting all message parts would be premature.
        CanonicalModelHistory::select(&task, initial, &events, &[])?;
        let names: Vec<_> = tools.iter().map(String::as_str).collect();
        let catalogue = ModelToolCatalogue::select(&self.tools.registry, &names, admission)?;
        if names.is_empty() {
            return Err(error(ErrorCode::CapabilityUnsupported));
        }

        // A completed answer can outlive the driver before its terminal event.
        // Use its canonical message, never call the model again in that window.
        if let Some(EventEnvelope { payload: EventPayload::DiagnosticRecorded { summary, .. }, .. }) = events.iter().rev().find(|event| matches!(&event.payload, EventPayload::DiagnosticRecorded { code, .. } if code == COMPLETE)) {
            let answer = events.iter().find_map(|event| match &event.payload {
                EventPayload::MessageCommitted { message } if message.message_id.as_str() == summary && message.status == MessageStatus::Committed && message.role == MessageRole::Assistant && message.task_id.as_ref() == Some(&task.task_id) && message.conversation_id == task.conversation_id => match message.parts.as_slice() { [MessagePart::Text { text }] => Some(text.clone()), _ => None },
                _ => None,
            }).ok_or_else(|| error(ErrorCode::InterruptedUnsafe))?;
            return Ok(ReadOnlyTurn::Completed { summary: crate::services::redact(&answer).text });
        }

        let mut selected = Vec::new();
        let mut calls = Vec::new();
        let mut results = BTreeSet::new();
        for event in &events {
            if let EventPayload::MessageCommitted { message } = &event.payload {
                if message.task_id.as_ref() != Some(&task.task_id)
                    || message.conversation_id != task.conversation_id
                {
                    return Err(error(ErrorCode::InterruptedUnsafe));
                }
                if message.status != MessageStatus::Committed {
                    continue;
                }
                selected.push(message.message_id.clone());
                for part in &message.parts {
                    match part {
                        MessagePart::ToolCall {
                            invocation_id,
                            tool_id,
                            arguments,
                        } => {
                            calls.push((invocation_id.clone(), tool_id.clone(), arguments.clone()))
                        }
                        MessagePart::ToolResult {
                            invocation_id,
                            content,
                            ..
                        } => {
                            // A failure the model could act on is part of its
                            // history (ADR 0029). Any other ends the work.
                            let failed = content.get("status").and_then(serde_json::Value::as_str)
                                == Some("failed");
                            let observable = content
                                .get("errorCode")
                                .cloned()
                                .and_then(|code| serde_json::from_value::<ErrorCode>(code).ok())
                                .is_some_and(super::tool::observable_failure);
                            if failed && !observable {
                                return Err(error(ErrorCode::ToolFailed));
                            }
                            results.insert(invocation_id.clone());
                        }
                        _ => {}
                    }
                }
            }
        }
        let pending: Vec<_> = calls
            .into_iter()
            .filter(|(id, _, _)| !results.contains(id))
            .collect();
        if pending.len() > 1 {
            return Err(error(ErrorCode::InterruptedUnsafe));
        }
        if let Some((id, name, arguments)) = pending.into_iter().next() {
            if !names.contains(&name.as_str()) {
                return Err(error(ErrorCode::CapabilityUnsupported));
            }
            let step = events
                .iter()
                .find_map(|event| match &event.payload {
                    EventPayload::ToolRequested {
                        invocation_id,
                        step_id,
                        ..
                    } if invocation_id == &id => step_id.clone(),
                    _ => None,
                })
                .ok_or_else(|| error(ErrorCode::InterruptedUnsafe))?;
            self.reserve(&events, limits, admission, 0, false).await?;
            self.turn_tool(&step, &name, arguments, cancel, None)
                .await?;
            return Ok(ReadOnlyTurn::Observed);
        }

        let history = CanonicalModelHistory::select(&task, initial, &events, &selected)?;
        let mut input = history.into_input(
            self.journal.next_id("stream"),
            self.journal.next_id("context"),
            catalogue,
        )?;
        input.request.limits.max_output_tokens = Some(u64::from(limits.turn_tokens));
        input.request.limits.deadline_ms = limits.turn_ms;
        // Reserve the full validated input allowance. Preparation adds derived
        // capabilities/classification metadata, so pre-preparation byte counts
        // would undercharge. Unused reservations are deliberately not refunded.
        let input_bytes = crate::services::MODEL_INPUT_BYTES as u64;
        // Charge before discovery, authorization or execution. A crash may
        // overcharge, but can never replenish a used budget on resume.
        let ordinal = self
            .reserve(&events, limits, admission, input_bytes, true)
            .await?;
        let snapshot = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(error(ErrorCode::Cancelled)),
            () = tokio::time::sleep_until(deadline) => return Err(error(ErrorCode::Timeout)),
            models = async {
                let mut all = Vec::new();
                for provider in &self.models { all.extend(provider.list_models().await); }
                all
            } => AvailabilitySnapshot { snapshot_id: self.journal.next_id("availability"), models, taken_at: self.journal.now() },
        };
        // Explicit capability facts, never provider or model name heuristics.
        if !snapshot.models.iter().any(|model| {
            model
                .capabilities
                .contains(&crate::contracts::ModelCapability::ToolCalls)
                && (self.routing_mode != RoutingMode::LocalOnly
                    || model.location == crate::contracts::ModelLocation::Local)
        }) {
            return Err(error(ErrorCode::CapabilityUnsupported));
        }
        let prepared = prepare_model_context(
            input,
            self.journal.next_id("decision"),
            self.routing_mode,
            snapshot,
            self.journal.now(),
        )?;
        let turn = self
            .dispatch_model(
                prepared,
                deadline,
                cancel,
                ModelTurnLimits {
                    content_bytes: limits.turn_bytes,
                    argument_bytes: limits.turn_bytes.min(crate::services::TOOL_INPUT_BYTES),
                    tool_calls: CALLS_PER_TURN,
                    ..ModelTurnLimits::default()
                },
                false,
            )
            .await?;
        if turn
            .usage
            .as_ref()
            .and_then(|usage| usage.output_tokens)
            .is_some_and(|tokens| tokens > u64::from(limits.turn_tokens))
        {
            return Err(error(ErrorCode::LimitExceeded));
        }
        match turn.stop_reason {
            StopReason::Stop => {
                let answer = self.answer_draft(&turn.text, MessageStatus::Committed);
                let EventPayload::MessageCommitted { message } = &answer.payload else {
                    return Err(error(ErrorCode::Internal));
                };
                let id = message.message_id.to_string();
                self.journal
                    .append_batch(vec![answer, diagnostic(COMPLETE, id)])
                    .await?;
                Ok(ReadOnlyTurn::Completed { summary: turn.text })
            }
            StopReason::ToolCall => {
                // One call at a time. The others never enter the canonical
                // history, so the model sees one call and its result, and asks
                // for the next step itself (ADR 0029).
                let mut calls = turn.tool_calls.into_iter();
                let call = calls
                    .next()
                    .ok_or_else(|| error(ErrorCode::ProtocolError))?;
                let not_run = calls.count();
                if !names.contains(&call.name.as_str()) {
                    return Err(error(ErrorCode::PermissionDenied));
                }
                let registered = self
                    .tools
                    .registry
                    .get(&call.name)
                    .ok_or_else(|| error(ErrorCode::NotFound))?;
                ToolInputSchema::compile(&registered.descriptor.input_schema)?
                    .validate(&call.arguments)?;
                if not_run > 0 {
                    // Counted, not named: the others' names are the model's
                    // words, never checked against the catalogue.
                    self.journal
                        .append(diagnostic(
                            CALLS_NOT_RUN,
                            format!(
                                "The model asked for {} tool calls in one turn. Only the first, {}, was run; it can ask for the others again.",
                                not_run + 1,
                                call.name
                            ),
                        ))
                        .await?;
                }
                // Complete the model action even for a tool-only answer. Tool
                // intent/result have their own M4 pending-action lifecycle.
                self.turn_tool(
                    &format!("reference-{ordinal}"),
                    &call.name,
                    call.arguments,
                    cancel,
                    Some(&turn.text),
                )
                .await?;
                Ok(ReadOnlyTurn::Observed)
            }
            StopReason::Length => {
                self.commit_answer(&turn.text, MessageStatus::Partial)
                    .await?;
                Err(error(ErrorCode::LimitExceeded))
            }
            StopReason::Cancelled => Err(error(ErrorCode::Cancelled)),
        }
    }

    fn canonical_task(&self, events: &[EventEnvelope]) -> Result<TaskRecord, ErrorEnvelope> {
        let first = events
            .first()
            .ok_or_else(|| error(ErrorCode::InterruptedUnsafe))?;
        let EventPayload::TaskCreated { goal_summary, .. } = &first.payload else {
            return Err(error(ErrorCode::InterruptedUnsafe));
        };
        Ok(TaskRecord::new(
            self.journal.task_id().clone(),
            self.journal.conversation_id().cloned(),
            self.scope.project_id.clone(),
            goal_summary,
            first.occurred_at.clone(),
        ))
    }

    async fn reserve(
        &self,
        events: &[EventEnvelope],
        limits: &ReadOnlyLimits,
        admission: ToolAdmission,
        input_bytes: u64,
        model: bool,
    ) -> Result<u64, ErrorEnvelope> {
        let next = Reservation {
            limits: limits.clone(),
            admission,
            input_bytes,
            output_bytes: TOOL_BYTES + if model { limits.turn_bytes as u64 } else { 0 },
            output_tokens: if model {
                u64::from(limits.turn_tokens)
            } else {
                0
            },
            time_ms: limits.turn_ms,
            tools: 1,
        };
        let mut totals = [
            1,
            next.tools,
            next.time_ms,
            input_bytes,
            next.output_bytes,
            next.output_tokens,
        ];
        for event in events {
            if let EventPayload::DiagnosticRecorded { code, summary } = &event.payload {
                if code != BUDGET {
                    continue;
                }
                let prior: Reservation = serde_json::from_str(summary)
                    .map_err(|_| error(ErrorCode::InterruptedUnsafe))?;
                // What the model may be offered belongs to the session, like
                // its limits. Resuming under another level would change it.
                if &prior.limits != limits || prior.admission != admission {
                    return Err(error(ErrorCode::InterruptedUnsafe));
                }
                let model_reservation =
                    prior.input_bytes == crate::services::MODEL_INPUT_BYTES as u64;
                if (prior.input_bytes != 0 && !model_reservation)
                    || prior.tools != 1
                    || prior.time_ms != limits.turn_ms
                    || prior.output_bytes
                        != TOOL_BYTES
                            + if model_reservation {
                                limits.turn_bytes as u64
                            } else {
                                0
                            }
                    || prior.output_tokens
                        != if model_reservation {
                            u64::from(limits.turn_tokens)
                        } else {
                            0
                        }
                {
                    return Err(error(ErrorCode::InterruptedUnsafe));
                }
                for (total, value) in totals.iter_mut().zip([
                    1,
                    prior.tools,
                    prior.time_ms,
                    prior.input_bytes,
                    prior.output_bytes,
                    prior.output_tokens,
                ]) {
                    *total = total.saturating_add(value);
                }
            }
        }
        if totals
            .iter()
            .zip([
                limits.steps,
                limits.tools,
                limits.time_ms,
                limits.input_bytes,
                limits.output_bytes,
                limits.output_tokens,
            ])
            .any(|(used, limit)| *used > limit)
        {
            self.journal
                .append(diagnostic(
                    "reference.budgetExceeded",
                    "The worker used up its budget".into(),
                ))
                .await?;
            return Err(error(ErrorCode::LimitExceeded));
        }
        self.journal
            .append(diagnostic(
                BUDGET,
                serde_json::to_string(&next).map_err(|_| error(ErrorCode::Internal))?,
            ))
            .await?;
        Ok(totals[0])
    }

    async fn turn_tool(
        &self,
        step: &str,
        name: &str,
        arguments: serde_json::Value,
        cancel: &CancellationToken,
        answer: Option<&str>,
    ) -> Result<(), ErrorEnvelope> {
        // Not under the turn's deadline, which bounds the model call. Waiting
        // for a person is untimed, since cancelling still ends it, and the run
        // is bounded by the tool itself, backed by the broker's
        // `TOOL_RUN_LIMIT` (ADR 0027).
        let child = cancel.child_token();
        let _cancel_on_drop = child.clone().drop_guard();
        let result = self
            .run_tool_step_with_answer(step, name, arguments, &child, answer)
            .await;
        match result {
            Ok(_) => Ok(()),
            // A failure the model can act on, recorded as the call's result,
            // is an observation: the model sees it on its next turn (ADR
            // 0029). A failure before the call was recorded has no result to
            // show, and ends the task like any other.
            Err(failure) => {
                if super::tool::observable_failure(failure.code)
                    && self.failure_recorded(step).await
                {
                    Ok(())
                } else {
                    Err(failure)
                }
            }
        }
    }

    /// Whether the latest attempt at `step` was recorded as a failed call.
    async fn failure_recorded(&self, step: &str) -> bool {
        let Ok(events) = self.journal.replay().await else {
            return false;
        };
        let Some(invocation) = events.iter().rev().find_map(|event| match &event.payload {
            EventPayload::ToolRequested {
                invocation_id,
                step_id: Some(id),
                ..
            } if id == step => Some(invocation_id.clone()),
            _ => None,
        }) else {
            return false;
        };
        events.iter().any(|event| {
            matches!(&event.payload, EventPayload::ToolFailed { invocation_id, .. } if *invocation_id == invocation)
        })
    }
}

fn diagnostic(code: &str, summary: String) -> EventDraft {
    EventDraft::user(
        TaskEventKind::DiagnosticRecorded,
        EventPayload::DiagnosticRecorded {
            code: code.into(),
            summary,
        },
    )
}
fn error(code: ErrorCode) -> ErrorEnvelope {
    let message = match code {
        // The worker is the same whatever the task may do, so none of these
        // calls it read-only: a task allowed to edit fails with them too.
        ErrorCode::CapabilityUnsupported => {
            "This work needs an eligible model endpoint whose tool calls are verified"
        }
        ErrorCode::LimitExceeded => {
            "The worker used up its budget before finishing; its activity shows how far it got"
        }
        ErrorCode::InterruptedUnsafe => {
            "Canonical history, limits or tool admission changed; this work cannot be resumed safely"
        }
        _ => "The worker stopped; its activity shows how far it got",
    };
    ErrorEnvelope::new(
        code,
        message,
        crate::contracts::CorrelationId::new("reference-worker"),
    )
}
fn validate_limits(limits: &ReadOnlyLimits) -> Result<(), ErrorEnvelope> {
    let max = ReadOnlyLimits::default();
    if [
        limits.steps,
        limits.tools,
        limits.time_ms,
        limits.input_bytes,
        limits.output_bytes,
        limits.output_tokens,
        limits.turn_ms,
        limits.turn_bytes as u64,
        u64::from(limits.turn_tokens),
    ]
    .into_iter()
    .zip([
        max.steps,
        max.tools,
        max.time_ms,
        max.input_bytes,
        max.output_bytes,
        max.output_tokens,
        max.turn_ms,
        max.turn_bytes as u64,
        u64::from(max.turn_tokens),
    ])
    .any(|(value, bound)| value == 0 || value > bound)
    {
        return Err(error(ErrorCode::InvalidRequest));
    }
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert on known-good literals; a panic is the failure report"
)]
mod tests {
    use super::*;

    fn reservation(admission: ToolAdmission) -> Reservation {
        Reservation {
            limits: ReadOnlyLimits::default(),
            admission,
            input_bytes: 0,
            output_bytes: TOOL_BYTES,
            output_tokens: 0,
            time_ms: ReadOnlyLimits::default().turn_ms,
            tools: 1,
        }
    }

    #[test]
    fn every_admission_reads_back_as_it_was_recorded() {
        // A resume compares the recorded admission with the task's own, so a
        // record that loses any part of it interrupts the task.
        for admission in [
            ToolAdmission::READ_ONLY,
            ToolAdmission::READ_ONLY.with_apps(),
            ToolAdmission::READ_WRITE,
            ToolAdmission::READ_ONLY.with_recipes(),
            ToolAdmission::READ_WRITE.with_recipes().with_apps(),
        ] {
            let recorded = serde_json::to_string(&reservation(admission)).unwrap();
            let read: Reservation = serde_json::from_str(&recorded).unwrap();
            assert_eq!(read.admission, admission, "{recorded}");
        }
    }

    #[test]
    fn a_reads_only_reservation_keeps_its_m6_shape() {
        let recorded = serde_json::to_string(&reservation(ToolAdmission::READ_ONLY)).unwrap();
        assert!(!recorded.contains("admission"), "{recorded}");
    }
}
