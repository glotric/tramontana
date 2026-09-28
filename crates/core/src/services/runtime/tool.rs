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

/// The capability namespaces that act on a project's roots (ADR 0057).
const PROJECT_CAPABILITIES: [&str; 3] = ["filesystem.", "git.", "command."];

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
                        // What the tool says it changed, under the same
                        // redaction as its summary (ADR 0026).
                        changes: outcome
                            .changes
                            .iter()
                            .cloned()
                            .map(|mut change| {
                                change.path = redact(&change.path).text;
                                change.from = change.from.map(|from| redact(&from).text);
                                change
                            })
                            .collect(),
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
                // The exception is the message of a failure the model can act
                // on, which is tool output: redacted and bounded (ADR 0029).
                let explanation =
                    observable_failure(error.code).then(|| explanation(&error.message));
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
                                content: match explanation {
                                    Some(message) => json!({"status":"failed","errorCode":error.code,"message":message}),
                                    None => json!({"status":"failed","errorCode":error.code}),
                                },
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
            let resolved = crate::services::recipe::resolve(
                self.tools.configuration.as_deref(),
                &arguments,
                self.routing_mode == RoutingMode::LocalOnly,
            )
            .map_err(|code| self.tool_error(code))?;
            (resolved.arguments, resolved.details)
        } else {
            let details = Self::named_paths(&arguments);
            (arguments, details)
        };
        // Defense in depth: never offered under Local Only, and refused if
        // asked for anyway (ADR 0050).
        if self.reaches_off_device_under_local_only(&descriptor.tool_id) {
            return Err(ErrorEnvelope::new(
                ErrorCode::PermissionDenied,
                "This task runs Local Only, and this App is not on this computer",
                self.journal.correlation_id().clone(),
            ));
        }
        // What the call acts on, where only its provider knows: the site of
        // the page a browser click lands on (ADR 0047). Resolved before
        // anyone is asked, and a call that cannot be resolved is refused
        // without a question, in words the model can act on.
        let target = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(self.tool_error(ErrorCode::Cancelled)),
            target = registered.provider.target(self.journal.task_id(), &descriptor.tool_id, &arguments) => target?,
        };
        let details = details
            .into_iter()
            .chain(
                target
                    .details
                    .into_iter()
                    .map(|detail| crate::contracts::ApprovalDetail {
                        label: redact(&detail.label).text,
                        value: redact(&detail.value).text,
                    }),
            )
            .collect();
        let query = PermissionQuery {
            tool_id: descriptor.tool_id.clone(),
            capability: Capability::new(&descriptor.capability),
            risk_class: descriptor.risk_class,
            scope: self.tools.scope.clone(),
            persistable: descriptor.risk_class != RiskClass::Dangerous,
            details,
            site: target.site,
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
        // A tool bounds its own run: the file and Git tools at 15 seconds, a
        // recipe at ten minutes. The backstop only stops one that does not,
        // and the tool is told to stop as well as abandoned (ADR 0027).
        let run = cancel.child_token();
        let _stop_on_exit = run.clone().drop_guard();
        tokio::select! {
            biased;
            () = cancel.cancelled() => Err(self.tool_error(ErrorCode::Cancelled)),
            result = registered.provider.invoke(authorized, run.clone()) => result,
            () = tokio::time::sleep(crate::ports::TOOL_RUN_LIMIT) => Err(self.tool_error(ErrorCode::Timeout)),
        }
    }

    /// Whether `tool_id` belongs to a provider off this computer, in a task
    /// that runs Local Only (ADR 0050).
    pub(super) fn reaches_off_device_under_local_only(&self, tool_id: &str) -> bool {
        self.routing_mode == RoutingMode::LocalOnly
            && self
                .tools
                .registry
                .get(tool_id)
                .and_then(|tool| tool.provider.remote_site())
                .is_some_and(|site| !crate::contracts::is_loopback_url(&site))
    }

    /// Whether `tool_id` can do anything for a task with no project
    /// (ADR 0057): by the capability it declares, never its name. Reading or
    /// writing files, reading Git, and running a project's recipes all act on
    /// a project's roots, and a projectless task has none. An unknown tool is
    /// not offered.
    pub(super) fn needs_no_project(&self, tool_id: &str) -> bool {
        self.tools.registry.get(tool_id).is_some_and(|tool| {
            let capability = tool.descriptor.capability.as_str();
            !PROJECT_CAPABILITIES
                .iter()
                .any(|namespace| capability.starts_with(namespace))
        })
    }

    /// The files a request names, for its approval prompt.
    ///
    /// A person approves an edit to a file, not a tool's name: V3 found write
    /// prompts that said which tool wanted to write and not where. Only the
    /// paths are shown, redacted like every other detail, and never content,
    /// which a prompt must not carry (`SECURITY_AND_PERMISSIONS.md` §7).
    fn named_paths(arguments: &Value) -> Vec<crate::contracts::ApprovalDetail> {
        [("path", "Path"), ("from", "From"), ("to", "To")]
            .into_iter()
            .filter_map(|(key, label)| {
                arguments.get(key).and_then(Value::as_str).map(|value| {
                    crate::contracts::ApprovalDetail {
                        label: label.to_owned(),
                        value: crate::services::redact(value).text,
                    }
                })
            })
            .collect()
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

/// Whether a tool's failure goes back to the model as the call's result
/// rather than ending the task: the failures a model can act on (ADR 0029).
pub(super) fn observable_failure(code: ErrorCode) -> bool {
    matches!(
        code,
        ErrorCode::InvalidRequest
            | ErrorCode::NotFound
            | ErrorCode::Conflict
            | ErrorCode::ToolFailed
            | ErrorCode::Timeout
    )
}

/// A failure's message as the model may see it: redacted, and at most a short
/// paragraph.
fn explanation(message: &str) -> String {
    const LIMIT: usize = 512;
    let text = redact(message).text;
    if text.len() <= LIMIT {
        return text;
    }
    let mut cut = LIMIT;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…", &text[..cut])
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
