//! Schema-checked, brokered steps and their canonical durable observations.
use super::*;
use crate::contracts::{Capability, MessagePart};
use crate::ports::ToolOutcome;
use crate::services::{
    PLACEHOLDER, REDACTED_OUTPUT, RegisteredTool, TOOL_INPUT_BYTES, ToolInputSchema, model_context,
    model_input, redact,
};
use serde_json::{Value, json};
use std::collections::BTreeSet;

const OUTPUT_BYTES: usize = 262_144;

impl BrokeredHost {
    pub(super) async fn run_tool_step(
        &self,
        step_id: &str,
        tool_id: &str,
        arguments: Value,
        cancel: &CancellationToken,
    ) -> Result<Value, ErrorEnvelope> {
        self.run_tool_step_with_answer(step_id, tool_id, arguments, cancel, None)
            .await
    }

    pub(super) async fn run_tool_step_with_answer(
        &self,
        step_id: &str,
        tool_id: &str,
        arguments: Value,
        cancel: &CancellationToken,
        answer: Option<&str>,
    ) -> Result<Value, ErrorEnvelope> {
        if tool_id.len() > 256 {
            return Err(self.tool_error(ErrorCode::LimitExceeded));
        }
        if step_id.is_empty()
            || step_id.len() > 128
            || !step_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        {
            return Err(self.tool_error(ErrorCode::InvalidRequest));
        }
        model_input::json_limits(&arguments)?;
        model_input::bounded_size(&arguments, TOOL_INPUT_BYTES)?;
        if model_context::json_has_secret(&arguments) {
            return Err(self.tool_error(ErrorCode::PermissionDenied));
        }
        let mut plan = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(self.tool_error(ErrorCode::Cancelled)),
            plan = self.tools.resume.lock() => plan,
        };
        if plan.uncertain.contains(step_id) {
            return Err(self.tool_error(ErrorCode::InterruptedUnsafe));
        }
        let hash = crate::services::checkpoint::request_hash(tool_id, &arguments);
        if plan
            .requests
            .get(step_id)
            .is_some_and(|previous| previous != &hash)
        {
            return Err(ErrorEnvelope::new(
                ErrorCode::InterruptedUnsafe,
                "A recorded step cannot be reused with a different tool or arguments",
                self.journal.correlation_id().clone(),
            ));
        }
        if let Some(completed) = plan.already_done(step_id) {
            self.journal
                .append(EventDraft::user(
                    TaskEventKind::DiagnosticRecorded,
                    EventPayload::DiagnosticRecorded {
                        code: "resume.stepSkipped".into(),
                        summary: "The logical step was already completed".into(),
                    },
                ))
                .await?;
            return Ok(
                json!({"skipped":true,"reason":"alreadyCompleted","summary":completed.summary}),
            );
        }
        let registered = self
            .tools
            .registry
            .get(tool_id)
            .ok_or_else(|| self.tool_error(ErrorCode::NotFound))?;
        ToolInputSchema::compile(&registered.descriptor.input_schema)?.validate(&arguments)?;
        // Invocation identifies one attempt. The logical step/hash, not this
        // ID or a provider call ID, supplies replay identity across attempts.
        let events = self.journal.replay().await?;
        let pending = events.iter().rev().find_map(|event| match &event.payload {
            EventPayload::ToolRequested { invocation_id, step_id: Some(step), .. } if step == step_id => Some(invocation_id.clone()),
            _ => None,
        }).filter(|id| !events.iter().any(|event| matches!(&event.payload,
            EventPayload::ToolCompleted { invocation_id, .. } | EventPayload::ToolFailed { invocation_id, .. } if invocation_id == id)));
        let invocation = pending
            .clone()
            .unwrap_or_else(|| InvocationId::new(self.journal.next_id("invocation")));
        if pending.is_none() {
            let mut call = self.tool_message(
                MessageRole::Assistant,
                MessagePart::ToolCall {
                    invocation_id: invocation.clone(),
                    tool_id: tool_id.into(),
                    arguments: arguments.clone(),
                },
            );
            if let Some(answer) = answer
                && let EventPayload::MessageCommitted { message } = &mut call.payload
            {
                message.parts.insert(
                    0,
                    MessagePart::Text {
                        text: answer.into(),
                    },
                );
            }
            self.journal
                .append_batch(vec![
                    EventDraft::user(
                        TaskEventKind::ToolRequested,
                        EventPayload::ToolRequested {
                            invocation_id: invocation.clone(),
                            tool_id: tool_id.into(),
                            arguments_summary: summarize_arguments(&arguments),
                            risk_class: registered.descriptor.risk_class,
                            step_id: Some(step_id.into()),
                            request_hash: Some(hash.clone()),
                        },
                    ),
                    call,
                ])
                .await?;
        }
        plan.requests.insert(step_id.into(), hash.clone());
        // The tool's declaration, not an inference from its side effect
        // (ADR 0019). Only a step declared safe may leave `uncertain` on failure.
        let replay_safe = registered.descriptor.replay_safety == ReplaySafety::Safe;
        let result = self
            .execute_tool(
                registered,
                &invocation,
                arguments,
                step_id,
                &mut plan,
                cancel,
            )
            .await;
        if self.tools.interrupt.is_cancelled() {
            return Err(self.tool_error(ErrorCode::InterruptedUnsafe));
        }
        match result {
            Ok(mut outcome) => {
                let (content, redactions, omitted) = sanitize_output(outcome.content);
                outcome.truncated |= omitted;
                let summary = if outcome.display_summary.len() <= 4096 {
                    redact(&outcome.display_summary).text
                } else {
                    "Tool completed (summary omitted)".into()
                };
                self.journal.append_batch(vec![
                    EventDraft::user(TaskEventKind::ToolCompleted, EventPayload::ToolCompleted {
                        invocation_id: invocation.clone(), display_summary: summary.clone(), side_effect: outcome.side_effect, truncated: outcome.truncated,
                    }),
                    self.tool_message(MessageRole::Tool, MessagePart::ToolResult {
                        invocation_id: invocation,
                        content: json!({"status":"completed","output":content,"redactions":redactions}), truncated: outcome.truncated,
                    }),
                ]).await?;
                plan.completed.insert(
                    step_id.into(),
                    crate::services::checkpoint::CompletedStep {
                        request_hash: hash,
                        summary,
                    },
                );
                plan.uncertain.remove(step_id);
                Ok(content)
            }
            Err(error) => {
                // Provider/approval errors can carry arbitrary bodies. Only
                // the stable code crosses into canonical records or workers.
                let error = self.tool_error(error.code);
                self.journal
                    .append_batch(vec![
                        EventDraft::user(
                            TaskEventKind::ToolFailed,
                            EventPayload::ToolFailed {
                                invocation_id: invocation.clone(),
                                error: error.clone(),
                            },
                        ),
                        self.tool_message(
                            MessageRole::Tool,
                            MessagePart::ToolResult {
                                invocation_id: invocation,
                                content: json!({"status":"failed","errorCode":error.code}),
                                truncated: false,
                            },
                        ),
                    ])
                    .await?;
                if replay_safe {
                    plan.uncertain.remove(step_id);
                }
                Err(error)
            }
        }
    }

    async fn execute_tool(
        &self,
        registered: &RegisteredTool,
        invocation: &InvocationId,
        arguments: Value,
        step_id: &str,
        plan: &mut ResumePlan,
        cancel: &CancellationToken,
    ) -> Result<ToolOutcome, ErrorEnvelope> {
        let descriptor = &registered.descriptor;
        // A recipe is resolved here, before anyone is asked: the prompt shows
        // exactly the command the tool will be handed, and a name the project
        // does not declare is refused without a question (ADR 0025).
        let (arguments, details) = if descriptor.tool_id == crate::services::RECIPE_TOOL_ID {
            let resolved =
                crate::services::recipe::resolve(self.tools.configuration.as_deref(), &arguments)
                    .map_err(|code| self.tool_error(code))?;
            (resolved.arguments, resolved.details)
        } else {
            (arguments, Vec::new())
        };
        let query = PermissionQuery {
            tool_id: descriptor.tool_id.clone(),
            capability: Capability::new(&descriptor.capability),
            risk_class: descriptor.risk_class,
            scope: self.tools.scope.clone(),
            persistable: descriptor.risk_class != RiskClass::Dangerous,
            details,
        };
        let now = self.tools.clock.now();
        let authorization = self.tools.permissions.authorize(
            &query,
            self.journal.task_id(),
            invocation,
            arguments,
            &now,
        );
        let (outcome, authorized) = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(self.tool_error(ErrorCode::Cancelled)),
            result = authorization => result.map_err(|_| self.tool_error(ErrorCode::PermissionDenied))?,
        };
        self.journal
            .append(EventDraft::user(
                TaskEventKind::ApprovalResolved,
                EventPayload::PermissionDecided {
                    invocation_id: invocation.clone(),
                    origin: self.tools.origin.clone(),
                    capability: query.capability.to_string(),
                    scope: query.scope,
                    allowed: outcome.is_allowed(),
                    reason_code: outcome.reason_code().into(),
                    grant_kind: authorized.as_ref().map(AuthorizedToolRequest::grant_kind),
                },
            ))
            .await?;
        let authorized = authorized.ok_or_else(|| self.tool_error(ErrorCode::PermissionDenied))?;
        if cancel.is_cancelled() {
            return Err(self.tool_error(ErrorCode::Cancelled));
        }
        // Recorded before execution, from the tool's own declaration. Until
        // M7 this was inferred — no side effect meant safe, anything else
        // unsafe — which could not express a write that is safe to replay
        // because it verifies the world first (ADR 0019).
        let replay_safety = descriptor.replay_safety;
        self.journal
            .append(EventDraft::user(
                TaskEventKind::ToolStarted,
                EventPayload::ToolStarted {
                    invocation_id: invocation.clone(),
                    tool_id: descriptor.tool_id.clone(),
                    replay_safety,
                },
            ))
            .await?;
        // Keep an unsafe effect uncertain until its completion/result batch
        // commits, even if the provider returned success before storage failed.
        if replay_safety != ReplaySafety::Safe {
            plan.uncertain.insert(step_id.into());
        }
        tokio::select! {
            biased;
            () = cancel.cancelled() => Err(self.tool_error(ErrorCode::Cancelled)),
            result = registered.provider.invoke(authorized, cancel.clone()) => result,
        }
    }

    fn tool_message(&self, role: MessageRole, part: MessagePart) -> EventDraft {
        let mut message = NormalizedMessage::text(
            MessageId::new(self.journal.next_id("message")),
            self.journal.conversation_id().cloned(),
            role,
            self.journal.now(),
            "",
        );
        message.task_id = Some(self.journal.task_id().clone());
        message.parts = vec![part];
        EventDraft::user(
            TaskEventKind::MessageCommitted,
            EventPayload::MessageCommitted { message },
        )
    }

    fn tool_error(&self, code: ErrorCode) -> ErrorEnvelope {
        ErrorEnvelope::new(
            code,
            "Tool invocation did not complete successfully",
            self.journal.correlation_id().clone(),
        )
    }
}

fn sanitize_output(mut value: Value) -> (Value, Vec<String>, bool) {
    let omitted = || (Value::String(REDACTED_OUTPUT.into()), Vec::new(), true);
    if model_input::json_limits(&value).is_err()
        || model_input::bounded_size(&value, OUTPUT_BYTES).is_err()
    {
        return omitted();
    }
    let mut codes = BTreeSet::new();
    fn visit(value: &mut Value, codes: &mut BTreeSet<String>) -> bool {
        match value {
            Value::String(text) => {
                let result = redact(text);
                *text = result.text;
                codes.extend(result.reason_codes);
            }
            Value::Array(items) => {
                for item in items {
                    if !visit(item, codes) {
                        return false;
                    }
                }
            }
            Value::Object(fields) => {
                for (key, value) in fields {
                    // Renaming hostile keys could silently collide. Omit the
                    // result instead; never persist the original key.
                    if redact(key).text != *key {
                        return false;
                    }
                    let scalar = match value {
                        Value::String(text) => Some(text.clone()),
                        Value::Number(number) => Some(number.to_string()),
                        _ => None,
                    };
                    if let Some(scalar) = scalar {
                        let assignment = format!("{key}={scalar}");
                        let redacted = redact(&assignment);
                        if redacted.text != assignment {
                            *value = Value::String(PLACEHOLDER.into());
                            codes.extend(redacted.reason_codes);
                        }
                    }
                    if !visit(value, codes) {
                        return false;
                    }
                }
            }
            _ => {}
        }
        true
    }
    if !visit(&mut value, &mut codes)
        || model_input::bounded_size(&value, OUTPUT_BYTES).is_err()
        || model_context::json_has_secret(&value)
    {
        return omitted();
    }
    (value, codes.into_iter().collect(), false)
}
