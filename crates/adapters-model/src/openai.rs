//! An adapter for endpoints speaking the OpenAI chat-completions wire format.
//!
//! Which is most of them: `llama-server`, Ollama, LM Studio, vLLM, and OpenAI
//! itself. One adapter therefore covers the local-first path and the optional
//! cloud path, and the difference between them is a URL and whether a
//! credential is attached — not a second implementation with its own bugs.
//!
//! Two rules this file exists to keep:
//!
//! - **A credential never becomes a field.** It is resolved per request and
//!   dropped, and it travels in a header. Never a URL, never a process
//!   argument, never a log line.
//! - **Exactly one terminal event ends a stream.** Cancellation, a deadline, a
//!   transport failure, and a provider error all converge to one, because the
//!   core commits the coalesced answer on that signal.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt as _;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tramontana_core::contracts::{
    AuthorizedModelRequest, CorrelationId, ErrorCode, ErrorEnvelope, Health, ModelCapability,
    ModelDescriptor, ModelEvent, ModelLocation, ModelRequest, ProviderId, SecretRef, StopReason,
    Usage,
};
use tramontana_core::ports::{ModelProvider, SecretResolver};

use crate::sse::EventStreamDecoder;

/// How a request authenticates, when it must.
///
/// The reference and the resolver are held; the value is not. A local endpoint
/// usually needs none, which is why this is an enum rather than an
/// `Option<String>` that someone would eventually fill with a literal.
#[derive(Clone)]
pub enum ProviderAuth {
    /// No credential. The normal case for a model on this device.
    None,
    /// A bearer token resolved from a secret store for each request.
    Bearer {
        /// The opaque reference. Never the value.
        reference: SecretRef,
        /// What turns the reference into a value.
        resolver: Arc<dyn SecretResolver>,
    },
}

impl std::fmt::Debug for ProviderAuth {
    /// Prints the *shape*, never the reference key or the value.
    ///
    /// A debug format ends up in logs and panic messages, which is exactly
    /// where a credential must not be.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::None => f.write_str("ProviderAuth::None"),
            Self::Bearer { .. } => f.write_str("ProviderAuth::Bearer(..)"),
        }
    }
}

/// A provider talking the OpenAI chat-completions format.
pub struct OpenAiCompatibleProvider {
    provider_id: ProviderId,
    /// Base URL, without a trailing slash. `/v1/chat/completions` is appended.
    base_url: String,
    descriptor: ModelDescriptor,
    auth: ProviderAuth,
    client: reqwest::Client,
}

impl std::fmt::Debug for OpenAiCompatibleProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiCompatibleProvider")
            .field("provider_id", &self.provider_id)
            .field("base_url", &self.base_url)
            .finish_non_exhaustive()
    }
}

impl OpenAiCompatibleProvider {
    /// Build a provider for one model at one endpoint.
    ///
    /// The descriptor is supplied rather than discovered because
    /// [`ModelProvider::list_models`] is called by the router on every routing
    /// decision, and a router that made a network call to decide would fail
    /// closed the moment a machine went offline — which is precisely when a
    /// local model is most wanted.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::InvalidRequest`] when the endpoint is not a usable URL, or
    /// [`ErrorCode::Internal`] when an HTTP client cannot be constructed.
    pub fn new(
        provider_id: impl Into<String>,
        base_url: impl Into<String>,
        descriptor: ModelDescriptor,
        auth: ProviderAuth,
    ) -> Result<Self, ErrorEnvelope> {
        let base_url = base_url.into().trim_end_matches('/').to_owned();
        // Refused here, plainly, rather than failing obscurely at the first
        // request. This build has no TLS stack: the endpoints M5 targets are
        // on loopback, and the decision to add one belongs to the milestone
        // that ships a cloud adapter (`docs/decisions/0007-http-client-and-tls.md`).
        if base_url.starts_with("https://") {
            return Err(error(
                ErrorCode::CapabilityUnsupported,
                "this build reaches http endpoints only; https support arrives with the cloud adapters",
            ));
        }
        if !base_url.starts_with("http://") {
            return Err(error(
                ErrorCode::InvalidRequest,
                format!("model endpoint must be an http URL, got {base_url:?}"),
            ));
        }

        let client = reqwest::Client::builder()
            // Connection setup only. The whole-request bound is the deadline
            // carried on each `ModelRequest`, which a caller can vary; a fixed
            // client timeout would silently override it.
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|failure| {
                error(
                    ErrorCode::Internal,
                    format!("could not build an HTTP client: {failure}"),
                )
            })?;

        Ok(Self {
            provider_id: ProviderId::new(provider_id),
            base_url,
            descriptor,
            auth,
            client,
        })
    }

    /// A descriptor for a model on this device.
    #[must_use]
    pub fn local_descriptor(
        provider_id: &str,
        model_id: impl Into<String>,
        display_name: impl Into<String>,
    ) -> ModelDescriptor {
        ModelDescriptor {
            provider_id: provider_id.to_owned(),
            model_id: model_id.into(),
            display_name: display_name.into(),
            location: ModelLocation::Local,
            capabilities: vec![
                ModelCapability::Text,
                ModelCapability::Streaming,
                ModelCapability::Cancellation,
            ],
            context_limit: None,
            health: Health::Available,
            health_reason_code: None,
        }
    }

    /// The chat-completions endpoint.
    fn completions_url(&self) -> String {
        format!("{}/v1/chat/completions", self.base_url)
    }

    /// The request body, in the wire format.
    fn body(&self, request: &ModelRequest) -> serde_json::Value {
        let messages: Vec<serde_json::Value> = request
            .messages
            .iter()
            .map(|message| {
                let text: String = message
                    .parts
                    .iter()
                    .filter_map(|part| match part {
                        tramontana_core::contracts::MessagePart::Text { text } => {
                            Some(text.as_str())
                        }
                        // Tool calls and results reach a model in M6, when a
                        // worker can produce them. Sending a placeholder now
                        // would put text in a prompt that no worker wrote.
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                serde_json::json!({ "role": role_of(message.role), "content": text })
            })
            .collect();

        let mut body = serde_json::json!({
            "model": self.descriptor.model_id,
            "messages": messages,
            "stream": true,
            // Asked for explicitly: without it most servers omit usage from a
            // streamed response, and a cost shown as unknown is worse than one
            // shown as estimated.
            "stream_options": { "include_usage": true },
        });
        if let Some(limit) = request.limits.max_output_tokens
            && let Some(object) = body.as_object_mut()
        {
            object.insert("max_tokens".to_owned(), limit.into());
        }
        body
    }
}

/// The wire name for a normalized role.
fn role_of(role: tramontana_core::contracts::MessageRole) -> &'static str {
    use tramontana_core::contracts::MessageRole;
    match role {
        MessageRole::System => "system",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::Tool => "tool",
    }
}

/// Build a normalized error.
fn error(code: ErrorCode, message: impl Into<String>) -> ErrorEnvelope {
    ErrorEnvelope::new(code, message, CorrelationId::new("model.openai"))
}

/// Map an HTTP status onto the normalized taxonomy.
///
/// The distinction that matters to a caller is whether retrying could help and
/// whether the user has to do something. A 401 is a configuration problem, a
/// 429 is a wait, a 5xx is the provider's.
fn code_for_status(status: u16) -> ErrorCode {
    match status {
        401 | 403 => ErrorCode::ProviderAuthentication,
        408 => ErrorCode::Timeout,
        429 => ErrorCode::ProviderRateLimited,
        400..=499 => ErrorCode::InvalidRequest,
        _ => ErrorCode::ProviderUnavailable,
    }
}

#[async_trait]
impl ModelProvider for OpenAiCompatibleProvider {
    fn id(&self) -> ProviderId {
        self.provider_id.clone()
    }

    async fn list_models(&self) -> Vec<ModelDescriptor> {
        vec![self.descriptor.clone()]
    }

    async fn stream(
        &self,
        authorized: AuthorizedModelRequest,
        cancel: CancellationToken,
    ) -> Result<mpsc::Receiver<ModelEvent>, ErrorEnvelope> {
        // The adapter reads the payload and never re-derives it. What may be
        // sent was decided in the core; an adapter that could widen the
        // request would make the disclosure a description of something else.
        let request = authorized.request();
        let mut builder = self
            .client
            .post(self.completions_url())
            .header("content-type", "application/json")
            .header("accept", "text/event-stream")
            .json(&self.body(request));

        // Resolved here and moved into the header. It is never stored on the
        // provider, never in the URL, and never in a process argument.
        if let ProviderAuth::Bearer {
            reference,
            resolver,
        } = &self.auth
        {
            let value = resolver.resolve(reference).await?;
            builder = builder.bearer_auth(value);
        }

        let stream_id = request.request_id.clone();
        let deadline = Duration::from_millis(request.limits.deadline_ms);
        let (sender, receiver) = mpsc::channel(32);

        tokio::spawn(async move {
            let outcome = pump(builder, &stream_id, deadline, &cancel, &sender).await;
            // Exactly one terminal event, whatever happened above — including
            // when the consumer has already gone away, in which case the send
            // fails harmlessly.
            let _ = sender.send(outcome).await;
        });

        Ok(receiver)
    }
}

/// Run the request and forward its events, returning the terminal one.
///
/// Written to have a single exit that produces the terminal event, so no
/// branch can end the stream without one.
async fn pump(
    builder: reqwest::RequestBuilder,
    stream_id: &str,
    deadline: Duration,
    cancel: &CancellationToken,
    sender: &mpsc::Sender<ModelEvent>,
) -> ModelEvent {
    let cancelled = ModelEvent::Completed {
        stop_reason: StopReason::Cancelled,
    };

    let response = tokio::select! {
        () = cancel.cancelled() => return cancelled,
        () = tokio::time::sleep(deadline) => {
            return ModelEvent::Failed {
                error: error(ErrorCode::Timeout, "the model did not respond within the deadline"),
            };
        }
        result = builder.send() => result,
    };

    let response = match response {
        Ok(response) => response,
        Err(failure) => {
            return ModelEvent::Failed {
                // The URL is deliberately not interpolated: a base URL can
                // carry a query string a user pasted from a provider console.
                error: error(
                    ErrorCode::ProviderUnavailable,
                    format!(
                        "could not reach the model endpoint: {}",
                        transport_reason(&failure)
                    ),
                ),
            };
        }
    };

    let status = response.status();
    if !status.is_success() {
        return ModelEvent::Failed {
            error: error(
                code_for_status(status.as_u16()),
                format!("the model endpoint returned {status}"),
            ),
        };
    }

    let mut decoder = EventStreamDecoder::new();
    let mut body = response.bytes_stream();
    let mut offset = 0u64;
    let mut stop_reason = StopReason::Stop;
    let started = tokio::time::Instant::now();

    loop {
        let chunk = tokio::select! {
            () = cancel.cancelled() => return cancelled,
            () = tokio::time::sleep_until(started + deadline) => {
                return ModelEvent::Failed {
                    error: error(
                        ErrorCode::Timeout,
                        "the model stream did not finish within the deadline",
                    ),
                };
            }
            next = body.next() => next,
        };

        let Some(chunk) = chunk else { break };
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(failure) => {
                return ModelEvent::Failed {
                    error: error(
                        ErrorCode::ProviderUnavailable,
                        format!(
                            "the model stream ended early: {}",
                            transport_reason(&failure)
                        ),
                    ),
                };
            }
        };

        for payload in decoder.push(&chunk) {
            match forward(&payload, stream_id, &mut offset, sender).await {
                Forwarded::Continue => {}
                Forwarded::Stopped(reason) => stop_reason = reason,
                Forwarded::Done => return ModelEvent::Completed { stop_reason },
                Forwarded::ConsumerGone => return cancelled,
            }
        }
    }

    if let Some(payload) = decoder.finish() {
        match forward(&payload, stream_id, &mut offset, sender).await {
            Forwarded::Stopped(reason) => stop_reason = reason,
            Forwarded::ConsumerGone => return cancelled,
            _ => {}
        }
    }

    ModelEvent::Completed { stop_reason }
}

/// What happened to one decoded payload.
enum Forwarded {
    /// Nothing that ends the stream.
    Continue,
    /// The provider reported why generation stopped.
    Stopped(StopReason),
    /// The provider signalled end of stream.
    Done,
    /// Nobody is listening any more.
    ConsumerGone,
}

/// Translate one wire payload into events and send them.
async fn forward(
    payload: &str,
    stream_id: &str,
    offset: &mut u64,
    sender: &mpsc::Sender<ModelEvent>,
) -> Forwarded {
    if payload.trim() == "[DONE]" {
        return Forwarded::Done;
    }

    let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) else {
        // A frame that is not JSON is a provider bug, not the user's problem,
        // and dropping it is better than failing an answer that is otherwise
        // arriving. It is visible in a diagnostic, not in the answer.
        tracing::debug!(stream_id, "ignoring a model frame that is not JSON");
        return Forwarded::Continue;
    };

    let mut result = Forwarded::Continue;

    if let Some(usage) = value.get("usage").filter(|usage| !usage.is_null()) {
        let event = ModelEvent::Usage {
            usage: Usage {
                input_tokens: usage
                    .get("prompt_tokens")
                    .and_then(serde_json::Value::as_u64),
                output_tokens: usage
                    .get("completion_tokens")
                    .and_then(serde_json::Value::as_u64),
                // Counted by the provider, not approximated here.
                estimated: false,
            },
        };
        if sender.send(event).await.is_err() {
            return Forwarded::ConsumerGone;
        }
    }

    let Some(choice) = value.get("choices").and_then(|c| c.get(0)) else {
        return result;
    };

    if let Some(delta) = choice.get("delta") {
        if let Some(text) = delta.get("content").and_then(serde_json::Value::as_str)
            && !text.is_empty()
        {
            let event = ModelEvent::TextDelta {
                stream_id: stream_id.to_owned(),
                offset: *offset,
                text: text.to_owned(),
            };
            *offset += 1;
            if sender.send(event).await.is_err() {
                return Forwarded::ConsumerGone;
            }
        }

        if let Some(calls) = delta
            .get("tool_calls")
            .and_then(serde_json::Value::as_array)
        {
            for call in calls {
                let Some(arguments) = call
                    .get("function")
                    .and_then(|function| function.get("arguments"))
                    .and_then(serde_json::Value::as_str)
                else {
                    continue;
                };
                let call_id = call
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .map_or_else(
                        || {
                            // Providers omit the id on continuation frames and
                            // identify the call by its index instead.
                            call.get("index")
                                .and_then(serde_json::Value::as_u64)
                                .map_or_else(
                                    || "call-0".to_owned(),
                                    |index| format!("call-{index}"),
                                )
                        },
                        str::to_owned,
                    );
                let event = ModelEvent::ToolCallDelta {
                    stream_id: stream_id.to_owned(),
                    offset: *offset,
                    call_id,
                    delta: arguments.to_owned(),
                };
                *offset += 1;
                if sender.send(event).await.is_err() {
                    return Forwarded::ConsumerGone;
                }
            }
        }
    }

    if let Some(reason) = choice
        .get("finish_reason")
        .and_then(serde_json::Value::as_str)
    {
        result = Forwarded::Stopped(match reason {
            "length" => StopReason::Length,
            "tool_calls" | "function_call" => StopReason::ToolCall,
            _ => StopReason::Stop,
        });
    }

    result
}

/// A transport failure described without echoing the URL.
///
/// A base URL can carry a query string a user pasted from a provider console,
/// and `reqwest`'s own `Display` includes the URL.
fn transport_reason(failure: &reqwest::Error) -> &'static str {
    if failure.is_timeout() {
        "timed out"
    } else if failure.is_connect() {
        "could not connect"
    } else if failure.is_decode() {
        "sent a response that could not be decoded"
    } else if failure.is_body() {
        "sent a truncated body"
    } else {
        "failed"
    }
}
