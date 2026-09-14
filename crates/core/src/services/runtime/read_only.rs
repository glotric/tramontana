//! One canonical, budgeted model/observation turn. Strategy repeats this port.
use super::*;
use crate::contracts::{EventEnvelope, MessagePart, TaskRecord};
use crate::ports::{ReadOnlyLimits, ReadOnlyTurn};
use crate::services::{
    CanonicalModelHistory, ModelToolCatalogue, ToolInputSchema, prepare_model_context,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

const BUDGET: &str = "reference.budget.v1";
const COMPLETE: &str = "reference.completed.v1";
const TOOL_BYTES: u64 = 262_144;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Reservation {
    limits: ReadOnlyLimits,
    input_bytes: u64,
    output_bytes: u64,
    output_tokens: u64,
    time_ms: u64,
    tools: u64,
}

impl BrokeredHost {
    pub(super) async fn run_read_only_turn(
        &self,
        tools: &[String],
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
        let catalogue = ModelToolCatalogue::select_read_only(&self.tools.registry, &names)?;
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
                            if content.get("status").and_then(serde_json::Value::as_str)
                                == Some("failed")
                            {
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
            self.reserve(&events, limits, 0, false).await?;
            self.read_tool(&step, &name, arguments, deadline, cancel, None)
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
        let ordinal = self.reserve(&events, limits, input_bytes, true).await?;
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
                    tool_calls: 1,
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
                let call = turn
                    .tool_calls
                    .into_iter()
                    .next()
                    .ok_or_else(|| error(ErrorCode::ProtocolError))?;
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
                // Complete the model action even for a tool-only answer. Tool
                // intent/result have their own M4 pending-action lifecycle.
                self.read_tool(
                    &format!("reference-{ordinal}"),
                    &call.name,
                    call.arguments,
                    deadline,
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
        input_bytes: u64,
        model: bool,
    ) -> Result<u64, ErrorEnvelope> {
        let next = Reservation {
            limits: limits.clone(),
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
                if &prior.limits != limits {
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
                    "Read-only worker reached its durable reservation budget".into(),
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

    async fn read_tool(
        &self,
        step: &str,
        name: &str,
        arguments: serde_json::Value,
        deadline: tokio::time::Instant,
        cancel: &CancellationToken,
        answer: Option<&str>,
    ) -> Result<(), ErrorEnvelope> {
        if tokio::time::Instant::now() >= deadline {
            return Err(error(ErrorCode::Timeout));
        }
        let child = cancel.child_token();
        let _cancel_on_drop = child.clone().drop_guard();
        let timer_cancel = child.clone();
        let timer = tokio::spawn(async move {
            tokio::time::sleep_until(deadline).await;
            timer_cancel.cancel();
        });
        let result = self
            .run_tool_step_with_answer(step, name, arguments, &child, answer)
            .await;
        timer.abort();
        if !cancel.is_cancelled() && tokio::time::Instant::now() >= deadline {
            return Err(error(ErrorCode::Timeout));
        }
        result.map(|_| ())
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
        ErrorCode::CapabilityUnsupported => {
            "Read-only work requires explicit tool-call support on an eligible model endpoint"
        }
        ErrorCode::LimitExceeded => {
            "Read-only worker reached a content or durable reservation limit; inspect task activity"
        }
        ErrorCode::InterruptedUnsafe => {
            "Canonical history or limits changed; this work cannot be resumed safely"
        }
        _ => "Read-only worker stopped; inspect task activity and budget reservations",
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
