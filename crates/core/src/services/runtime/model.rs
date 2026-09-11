//! Brokered text requests. Per-turn preparation belongs to the host, not the worker.
use super::*;
use crate::services::{ModelContentProvenance, ModelContextInput, prepare_model_context};

impl BrokeredHost {
    pub(super) async fn run_model_request(
        &self,
        prompt: &str,
        cancel: &CancellationToken,
    ) -> Result<String, ErrorEnvelope> {
        let deadline = tokio::time::Instant::now()
            + std::time::Duration::from_millis(
                crate::contracts::ModelLimits::default().deadline_ms,
            );
        let _guard = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(self.model_error(ErrorCode::Cancelled)),
            () = tokio::time::sleep_until(deadline) => return Err(self.model_error(ErrorCode::Timeout)),
            guard = self.model_gate.lock() => guard,
        };
        // The worker cannot introduce new content by asking the host to build
        // a manifest for it. Only the initially included, byte-matched goal is
        // eligible on this legacy text path. Tool observations need a separate
        // host-owned history path; concatenating them into this prompt fails.
        let Some(initial) = self.context.as_ref() else {
            return Err(ErrorEnvelope::new(
                ErrorCode::PermissionDenied,
                "A model request needs assembled context",
                self.journal.correlation_id().clone(),
            ));
        };
        if prompt.len() > crate::services::MODEL_INPUT_BYTES {
            return Err(self.model_error(ErrorCode::LimitExceeded));
        }
        let hash = crate::contracts::content_hash(prompt);
        let source = initial
            .sources
            .iter()
            .find(|source| {
                source.included
                    && source.source_id == "conversation.goal"
                    && source.content_hash == hash
                    && source.byte_count == prompt.len() as u64
            })
            .ok_or_else(|| self.model_error(ErrorCode::PermissionDenied))?;
        // IDs are unique for every call, including repeated calls in one task.
        let stream_id = self.journal.next_id("stream");
        let request = ModelRequest::new(
            stream_id.clone(),
            self.journal.task_id().clone(),
            vec![NormalizedMessage::text(
                MessageId::new(self.journal.next_id("message")),
                self.journal.conversation_id().cloned(),
                MessageRole::User,
                self.journal.now(),
                prompt,
            )],
            self.journal.next_id("context"),
        );
        let snapshot = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(self.model_error(ErrorCode::Cancelled)),
            () = tokio::time::sleep_until(deadline) => return Err(self.model_error(ErrorCode::Timeout)),
            models = async {
                let mut models = Vec::new();
                for provider in &self.models { models.extend(provider.list_models().await); }
                models
            } => AvailabilitySnapshot { snapshot_id: self.journal.next_id("availability"), models, taken_at: self.journal.now() },
        };
        let mut prepared = prepare_model_context(
            ModelContextInput {
                request,
                messages: vec![ModelContentProvenance {
                    source_id: source.source_id.clone(),
                    data_class: source.data_class,
                    redactions: source.redactions.clone(),
                }],
                tools: Vec::new(),
            },
            self.journal.next_id("decision"),
            self.routing_mode,
            snapshot,
            self.journal.now(),
        )?;
        // Preserve considered-but-not-transmitted sources for disclosure,
        // without admitting their bodies into this request.
        for other in &initial.sources {
            if other.source_id != source.source_id {
                let mut excluded = other.clone();
                excluded.included = false;
                excluded.exclusion_reason = Some(if other.included {
                    "release.notTransmitted".into()
                } else {
                    other
                        .exclusion_reason
                        .clone()
                        .unwrap_or_else(|| "context.notActive".into())
                });
                prepared.manifest.sources.push(excluded);
            }
        }
        prepared.manifest = ContextManifest::new(
            prepared.manifest.context_manifest_id.clone(),
            prepared.manifest.task_id.clone(),
            prepared.manifest.destination_class,
            std::mem::take(&mut prepared.manifest.sources),
            prepared.manifest.created_at.clone(),
        );
        let manifest = &prepared.manifest;
        let decision = &prepared.decision;
        let provider = self
            .models
            .iter()
            .find(|provider| provider.id().to_string() == decision.model.provider_id)
            .ok_or_else(|| self.model_error(ErrorCode::ProviderUnavailable))?;
        self.journal
            .append(EventDraft::user(
                TaskEventKind::ContextPrepared,
                EventPayload::ContextPrepared {
                    manifest: manifest.clone(),
                },
            ))
            .await?;
        self.journal
            .append(EventDraft::user(
                TaskEventKind::RoutingDecided,
                EventPayload::RoutingDecided {
                    decision: decision.clone(),
                },
            ))
            .await?;

        // Nothing reaches a provider without a release decision. For a local
        // destination that costs a record and no question; for a cloud one it
        // is the disclosure and the approval
        // (`SECURITY_AND_PERMISSIONS.md` §8).
        let release_context = ReleaseContext {
            manifest,
            decision,
            mode: self.routing_mode,
            scope: &self.scope,
        };
        let authorization = self.releases.authorize(
            self.journal.next_id("release"),
            prepared.request,
            &release_context,
            self.journal.now(),
        );
        let authorized = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(self.model_error(ErrorCode::Cancelled)),
            () = tokio::time::sleep_until(deadline) => return Err(self.model_error(ErrorCode::Timeout)),
            result = authorization => result.map_err(|error| self.release_refused(&error))?,
        };

        // Recorded before the request goes, not after: if the process dies
        // mid-stream the record still says what left the device.
        self.journal
            .append(EventDraft::user(
                TaskEventKind::ContextReleased,
                EventPayload::ContextReleased {
                    plan: Box::new(authorized.plan().clone()),
                },
            ))
            .await?;

        if cancel.is_cancelled() {
            return Err(self.model_error(ErrorCode::Cancelled));
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(self.model_error(ErrorCode::Timeout));
        }
        // Only now is it true that a model request has started. Emitted by the
        // host rather than the worker, and after the release rather than
        // before it, so a refused release cannot leave a record saying a model
        // ran (`CORE_CONTRACTS.md` §5.1).
        self.journal
            .append(EventDraft::user(
                TaskEventKind::ModelStarted,
                EventPayload::ModelStarted {
                    stream_id: stream_id.clone(),
                    model_id: decision.model.model_id.clone(),
                },
            ))
            .await?;

        let model_cancel = cancel.child_token();
        let _cancel_on_drop = model_cancel.clone().drop_guard();
        let mut stream = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(ErrorEnvelope::new(
                ErrorCode::Cancelled, "Model request cancelled", self.journal.correlation_id().clone())),
            () = tokio::time::sleep_until(deadline) => return Err(ErrorEnvelope::new(
                ErrorCode::Timeout, "Model request deadline exceeded", self.journal.correlation_id().clone())),
            result = provider.stream(authorized, model_cancel.clone()) => result?,
        };

        // Deltas are coalesced here and committed once. Persisting every token
        // would make the durable stream enormous and tell the reader nothing
        // the final message does not (`CORE_CONTRACTS.md` §5).
        let mut turn = ModelTurnAccumulator::new(
            stream_id.clone(),
            self.journal.correlation_id().clone(),
            ModelTurnLimits::default(),
        );
        let mut failure = None;
        loop {
            let event = tokio::select! {
                biased;
                () = cancel.cancelled() => {
                    failure = Some(ErrorEnvelope::new(ErrorCode::Cancelled,
                        "Model request cancelled", self.journal.correlation_id().clone()));
                    break;
                }
                () = tokio::time::sleep_until(deadline) => {
                    failure = Some(ErrorEnvelope::new(ErrorCode::Timeout,
                        "Model request deadline exceeded", self.journal.correlation_id().clone()));
                    break;
                }
                event = stream.recv() => event,
            };
            let Some(event) = event else { break };
            let offset = turn.text().len() as u64;
            if let Err(error) = turn.push(&event) {
                failure = Some(error);
                break;
            }
            if let ModelEvent::TextDelta { text, .. } = event {
                self.journal.publish_frame(StreamFrame {
                    stream_id: stream_id.clone(),
                    offset,
                    text,
                });
            }
        }
        let answer = turn.text().to_owned();
        let result = turn.finish();
        let stop_reason = result.as_ref().ok().map(|turn| turn.stop_reason);
        if failure.is_none() {
            failure = result.err();
        }

        // The frames are gone the moment they are delivered. Committing the
        // coalesced answer is what puts the model's reply in the durable
        // record at all — without it an exported task shows that a model was
        // asked and never what it said (`CORE_CONTRACTS.md` §5).
        let status = match (stop_reason, &failure) {
            (Some(StopReason::Stop), None) => MessageStatus::Committed,
            // Cut short: the content up to that point is kept and marked, so a
            // reader can tell a finished answer from a truncated one.
            _ => MessageStatus::Partial,
        };
        let mut message = NormalizedMessage::text(
            MessageId::new(self.journal.next_id("message")),
            self.journal.conversation_id().cloned(),
            MessageRole::Assistant,
            self.journal.now(),
            answer.clone(),
        );
        message.task_id = Some(self.journal.task_id().clone());
        message.status = status;

        self.journal
            .append(EventDraft::user(
                TaskEventKind::MessageCommitted,
                EventPayload::MessageCommitted { message },
            ))
            .await?;

        if let Some(error) = failure {
            return Err(error);
        }
        // The legacy fake worker asks for text, not model-selected tools. Do
        // not turn a tool request or truncated answer into apparent success.
        let refusal = match stop_reason {
            Some(StopReason::Stop) => None,
            Some(StopReason::ToolCall) => Some((
                ErrorCode::CapabilityUnsupported,
                "This worker does not support model-selected tool calls",
            )),
            Some(StopReason::Length) => {
                Some((ErrorCode::LimitExceeded, "Model output limit reached"))
            }
            Some(StopReason::Cancelled) => Some((ErrorCode::Cancelled, "Model request cancelled")),
            None => Some((
                ErrorCode::ProtocolError,
                "Model stream ended without an outcome",
            )),
        };
        if let Some((code, message)) = refusal {
            return Err(ErrorEnvelope::new(
                code,
                message,
                self.journal.correlation_id().clone(),
            ));
        }
        Ok(answer)
    }

    fn model_error(&self, code: ErrorCode) -> ErrorEnvelope {
        let message = match code {
            ErrorCode::Cancelled => "Model request cancelled",
            ErrorCode::Timeout => "Model request deadline exceeded",
            ErrorCode::PermissionDenied => "Model prompt is not included in the task context",
            ErrorCode::LimitExceeded => "Model input limit exceeded",
            _ => "Selected model provider is unavailable",
        };
        ErrorEnvelope::new(code, message, self.journal.correlation_id().clone())
    }
}
