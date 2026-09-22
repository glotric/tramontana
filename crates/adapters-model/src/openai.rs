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
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tramontana_core::contracts::{
    AuthorizedModelRequest, ErrorCode, ErrorEnvelope, Health, ModelCapability, ModelDescriptor,
    ModelEvent, ModelLocation, ProviderId, SecretRef, StopReason, Usage,
};
use tramontana_core::ports::{ModelProvider, SecretResolver};

use crate::transport::{Signal, WireFormat, check_endpoint, error, pump};

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
    /// A key this process made up for a runtime it started, so nothing else
    /// on the machine — a web page in a browser least of all — can use that
    /// runtime. Held in memory for the runtime's life; never stored, never
    /// printed.
    Session(String),
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
            Self::Session(_) => f.write_str("ProviderAuth::Session(..)"),
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
    /// What to put in a request's `model` field, when the server needs
    /// something other than the model's own identifier.
    wire_model: Option<String>,
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
        check_endpoint(&base_url)?;

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
            wire_model: None,
        })
    }

    /// Name the model to the server as `name` rather than by its identifier.
    ///
    /// `mlx_lm.server` loads whatever a request's `model` field names, from
    /// Hugging Face when it is not a local path; `default_model` is its name
    /// for the model it was started with. Routing and records keep using the
    /// model's own identifier.
    #[must_use]
    pub fn with_wire_model(mut self, name: impl Into<String>) -> Self {
        self.wire_model = Some(name.into());
        self
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
        tramontana_core::services::validate_model_capabilities(request, &self.descriptor)?;
        let names = crate::request::WireNames::new(request);
        let model = self
            .wire_model
            .as_deref()
            .unwrap_or(&self.descriptor.model_id);
        let body = crate::request::openai_body(request, model, &names)?;
        let mut builder = self
            .client
            .post(self.completions_url())
            .header("content-type", "application/json")
            .header("accept", "text/event-stream")
            .json(&body);

        // Resolved here and moved into the header. It is never stored on the
        // provider, never in the URL, and never in a process argument.
        match &self.auth {
            ProviderAuth::None => {}
            ProviderAuth::Bearer {
                reference,
                resolver,
            } => {
                let value = resolver.resolve(reference).await?;
                builder = builder.bearer_auth(value);
            }
            ProviderAuth::Session(key) => builder = builder.bearer_auth(key),
        }

        let stream_id = request.request_id.clone();
        let deadline = Duration::from_millis(request.limits.deadline_ms);
        let (sender, receiver) = mpsc::channel(32);

        tokio::spawn(async move {
            let outcome = pump(
                builder,
                &crate::request::NamedWire {
                    format: ChatCompletions,
                    names,
                },
                &stream_id,
                deadline,
                &cancel,
                &sender,
            )
            .await;
            // Exactly one terminal event, whatever happened above — including
            // when the consumer has already gone away, in which case the send
            // fails harmlessly.
            let _ = sender.send(outcome).await;
        });

        Ok(receiver)
    }
}

/// The chat-completions frame format.
///
/// A pure translation from one decoded `data:` payload to events. Keeping it
/// free of IO is what lets a frame shape be tested directly, and it keeps the
/// cancellation and terminal-event rules in exactly one place
/// (`crate::transport`).
pub(crate) struct ChatCompletions;

impl WireFormat for ChatCompletions {
    fn decode(
        &self,
        payload: &str,
        stream_id: &str,
        offset: &mut u64,
    ) -> (Vec<ModelEvent>, Signal) {
        let mut events = Vec::new();

        if payload.trim() == "[DONE]" {
            return (events, Signal::Done);
        }

        let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) else {
            // A frame that is not JSON is a provider bug, not the user's
            // problem, and dropping it is better than failing an answer that
            // is otherwise arriving. It is visible in a diagnostic, not in the
            // answer.
            tracing::debug!(stream_id, "ignoring a model frame that is not JSON");
            return (events, Signal::Continue);
        };

        if let Some(usage) = value.get("usage").filter(|usage| !usage.is_null()) {
            events.push(ModelEvent::Usage {
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
            });
        }

        let Some(choice) = value.get("choices").and_then(|choices| choices.get(0)) else {
            return (events, Signal::Continue);
        };

        if let Some(delta) = choice.get("delta") {
            if let Some(text) = delta.get("content").and_then(serde_json::Value::as_str)
                && !text.is_empty()
            {
                events.push(ModelEvent::TextDelta {
                    stream_id: stream_id.to_owned(),
                    offset: *offset,
                    text: text.to_owned(),
                });
                *offset += 1;
            }

            if let Some(calls) = delta
                .get("tool_calls")
                .and_then(serde_json::Value::as_array)
            {
                for call in calls {
                    let Some(call_index) = call.get("index").and_then(serde_json::Value::as_u64)
                    else {
                        return (
                            events,
                            Signal::Failed(error(
                                ErrorCode::ProtocolError,
                                "A streamed tool call has no valid index",
                            )),
                        );
                    };
                    let function = call.get("function");
                    if function.is_some_and(|value| !value.is_null() && !value.is_object())
                        || [
                            call.get("id"),
                            function.and_then(|value| value.get("name")),
                            function.and_then(|value| value.get("arguments")),
                        ]
                        .into_iter()
                        .flatten()
                        .any(|value| !value.is_null() && !value.is_string())
                    {
                        return (
                            events,
                            Signal::Failed(error(
                                ErrorCode::ProtocolError,
                                "A streamed tool call has invalid field types",
                            )),
                        );
                    }
                    let arguments = function
                        .and_then(|function| function.get("arguments"))
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default();
                    let name = function
                        .and_then(|function| function.get("name"))
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned);
                    let call_id = call
                        .get("id")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned);
                    events.push(ModelEvent::ToolCallDelta {
                        stream_id: stream_id.to_owned(),
                        offset: *offset,
                        call_index,
                        call_id,
                        name,
                        delta: arguments.to_owned(),
                    });
                    *offset += 1;
                }
            }
        }

        let signal = choice
            .get("finish_reason")
            .and_then(serde_json::Value::as_str)
            .map_or(Signal::Continue, |reason| {
                Signal::Stopped(match reason {
                    "length" => StopReason::Length,
                    "tool_calls" | "function_call" => StopReason::ToolCall,
                    _ => StopReason::Stop,
                })
            });

        (events, signal)
    }
}
