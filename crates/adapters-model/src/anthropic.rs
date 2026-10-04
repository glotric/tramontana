//! An adapter for the Anthropic Messages API.
//!
//! A second wire format, not a second transport. Cancellation, deadlines,
//! status mapping, and the exactly-one-terminal-event rule live in
//! `crate::transport` and are shared with the chat-completions adapter; what
//! is here is only what Anthropic genuinely does differently.
//!
//! Four things it does differently, each of which would be a silent bug if
//! copied from the OpenAI adapter:
//!
//! - **The system prompt is a top-level field**, not a message with
//!   `role: "system"`. Sending a system-role message is a request error.
//! - **`max_tokens` is required.** There is no server default, so a request
//!   without one is rejected outright.
//! - **Authentication is `x-api-key`**, not `Authorization: Bearer`, and the
//!   API version is a required header.
//! - **There is no `[DONE]` sentinel.** The stream ends with `message_stop`,
//!   and every frame names its own type.

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

/// The API version this adapter is written against.
///
/// Pinned rather than tracked: Anthropic versions its API by date, and a
/// request that omits the header is rejected. Moving it is a deliberate change
/// with its own testing, not something that should drift with the calendar.
const API_VERSION: &str = "2023-06-01";

/// The default output bound, when a request does not set one.
///
/// The API requires `max_tokens` and has no server-side default, so something
/// has to be chosen. It is here rather than buried in the body builder because
/// it is a product decision — the point at which a long answer is cut off —
/// and it should be visible.
const DEFAULT_MAX_OUTPUT_TOKENS: u64 = 4096;

/// Anthropic's Messages API.
pub struct AnthropicProvider {
    provider_id: ProviderId,
    base_url: String,
    descriptor: ModelDescriptor,
    credential: SecretRef,
    resolver: Arc<dyn SecretResolver>,
    client: reqwest::Client,
}

impl std::fmt::Debug for AnthropicProvider {
    /// Names the endpoint and the credential *reference*, never a value.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnthropicProvider")
            .field("provider_id", &self.provider_id)
            .field("base_url", &self.base_url)
            .field("credential", &self.credential.key)
            .finish_non_exhaustive()
    }
}

impl AnthropicProvider {
    /// Point the adapter at an endpoint.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::InvalidRequest`] for an endpoint that would send the
    /// credential in the clear, or [`ErrorCode::Internal`] when an HTTP client
    /// cannot be built.
    pub fn new(
        provider_id: impl Into<String>,
        base_url: impl Into<String>,
        descriptor: ModelDescriptor,
        credential: SecretRef,
        resolver: Arc<dyn SecretResolver>,
    ) -> Result<Self, ErrorEnvelope> {
        let base_url = base_url.into().trim_end_matches('/').to_owned();
        check_endpoint(&base_url)?;

        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            // Never followed: a redirect would replay the prompt, and any
            // credential header the library does not know to strip, to a
            // host the routing decision never named — over plain HTTP, if
            // that is where it points (ADR 0066).
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|failure| {
                error(
                    ErrorCode::Internal,
                    format!("could not build an HTTP client: {failure}"),
                )
            })?;

        Ok(Self {
            // The configured endpoint's identifier, not a fixed vendor name:
            // two Anthropic endpoints in one profile are two providers, and a
            // routing decision has to be able to name which one it chose.
            provider_id: ProviderId::new(provider_id),
            base_url,
            descriptor,
            credential,
            resolver,
            client,
        })
    }

    /// A descriptor for a model reached over the network.
    ///
    /// `Cloud`, which is not cosmetic: the router treats it as a prohibition
    /// under Local Only, and the release boundary requires a disclosure and an
    /// approval before anything reaches it.
    #[must_use]
    pub fn cloud_descriptor(
        provider_id: &str,
        model_id: impl Into<String>,
        display_name: impl Into<String>,
        context_limit: Option<u64>,
    ) -> ModelDescriptor {
        ModelDescriptor {
            provider_id: provider_id.to_owned(),
            model_id: model_id.into(),
            display_name: display_name.into(),
            location: ModelLocation::Cloud,
            capabilities: vec![
                ModelCapability::Text,
                ModelCapability::Streaming,
                ModelCapability::Cancellation,
                ModelCapability::ToolCalls,
            ],
            context_limit,
            health: Health::Available,
            health_reason_code: None,
        }
    }

    /// The messages endpoint.
    fn messages_url(&self) -> String {
        format!("{}/v1/messages", self.base_url)
    }
}

#[async_trait]
impl ModelProvider for AnthropicProvider {
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
        let request = authorized.request();
        tramontana_core::services::validate_model_capabilities(request, &self.descriptor)?;
        let names = crate::request::WireNames::new(request);
        let body = crate::request::anthropic_body(
            request,
            &self.descriptor.model_id,
            DEFAULT_MAX_OUTPUT_TOKENS,
            &names,
        )?;

        // Resolved here and moved straight into the header: never a field on
        // the provider, never a URL, never a process argument.
        let key = self.resolver.resolve(&self.credential).await?;
        let builder = self
            .client
            .post(self.messages_url())
            .header("content-type", "application/json")
            .header("accept", "text/event-stream")
            .header("anthropic-version", API_VERSION)
            .header("x-api-key", key)
            .json(&body);

        let stream_id = request.request_id.clone();
        let deadline = Duration::from_millis(request.limits.deadline_ms);
        let (sender, receiver) = mpsc::channel(32);

        tokio::spawn(async move {
            let outcome = pump(
                builder,
                &crate::request::NamedWire {
                    format: Messages,
                    names,
                },
                &stream_id,
                deadline,
                &cancel,
                &sender,
            )
            .await;
            let _ = sender.send(outcome).await;
        });

        Ok(receiver)
    }
}

/// Anthropic's streaming frame format.
///
/// Every frame names its own `type`, and the stream ends with `message_stop`
/// rather than a sentinel. Usage arrives split across two frames — input
/// tokens on `message_start`, output tokens on `message_delta` — which is why
/// both are read.
pub(crate) struct Messages;

impl WireFormat for Messages {
    fn decode(
        &self,
        payload: &str,
        stream_id: &str,
        offset: &mut u64,
    ) -> (Vec<ModelEvent>, Signal) {
        let mut events = Vec::new();

        let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) else {
            tracing::debug!(stream_id, "ignoring a model frame that is not JSON");
            return (events, Signal::Continue);
        };
        let kind = value
            .get("type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();

        match kind {
            // A keep-alive. Carries nothing.
            "ping" => (events, Signal::Continue),

            "message_start" => {
                if let Some(usage) = value
                    .get("message")
                    .and_then(|message| message.get("usage"))
                {
                    events.push(usage_event(usage));
                }
                (events, Signal::Continue)
            }

            "content_block_start" if value["content_block"]["type"] == "tool_use" => {
                let Some(call_index) = value.get("index").and_then(serde_json::Value::as_u64)
                else {
                    return (
                        events,
                        Signal::Failed(error(
                            ErrorCode::ProtocolError,
                            "A streamed tool call has no valid index",
                        )),
                    );
                };
                let block = &value["content_block"];
                if !block.get("input").is_some_and(serde_json::Value::is_object) {
                    return (
                        events,
                        Signal::Failed(error(
                            ErrorCode::ProtocolError,
                            "A streamed tool call has invalid initial input",
                        )),
                    );
                }
                // The opening empty object is a placeholder, not a fragment
                // to concatenate before the actual input_json_delta bytes.
                let delta = block
                    .get("input")
                    .filter(|input| input.as_object().is_none_or(|object| !object.is_empty()))
                    .map_or_else(String::new, serde_json::Value::to_string);
                events.push(ModelEvent::ToolCallDelta {
                    stream_id: stream_id.to_owned(),
                    offset: *offset,
                    call_index,
                    call_id: block
                        .get("id")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned),
                    name: block
                        .get("name")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned),
                    delta,
                });
                *offset += 1;
                (events, Signal::Continue)
            }

            "content_block_delta" => {
                if let Some(text) = value
                    .get("delta")
                    .filter(|delta| {
                        delta.get("type").and_then(serde_json::Value::as_str) == Some("text_delta")
                    })
                    .and_then(|delta| delta.get("text"))
                    .and_then(serde_json::Value::as_str)
                    && !text.is_empty()
                {
                    events.push(ModelEvent::TextDelta {
                        stream_id: stream_id.to_owned(),
                        offset: *offset,
                        text: text.to_owned(),
                    });
                    *offset += 1;
                }
                // A tool call arrives as `input_json_delta` fragments against
                // the block opened by `content_block_start`.
                if value["delta"]["type"] == "input_json_delta"
                    && !value["delta"]["partial_json"].is_string()
                {
                    return (
                        events,
                        Signal::Failed(error(
                            ErrorCode::ProtocolError,
                            "A streamed tool call has invalid argument fragments",
                        )),
                    );
                }
                if let Some(partial) = value
                    .get("delta")
                    .filter(|delta| {
                        delta.get("type").and_then(serde_json::Value::as_str)
                            == Some("input_json_delta")
                    })
                    .and_then(|delta| delta.get("partial_json"))
                    .and_then(serde_json::Value::as_str)
                {
                    let Some(call_index) = value.get("index").and_then(serde_json::Value::as_u64)
                    else {
                        return (
                            events,
                            Signal::Failed(error(
                                ErrorCode::ProtocolError,
                                "A streamed tool call has no valid index",
                            )),
                        );
                    };
                    events.push(ModelEvent::ToolCallDelta {
                        stream_id: stream_id.to_owned(),
                        offset: *offset,
                        call_index,
                        call_id: None,
                        name: None,
                        delta: partial.to_owned(),
                    });
                    *offset += 1;
                }
                (events, Signal::Continue)
            }

            "message_delta" => {
                if let Some(usage) = value.get("usage") {
                    events.push(usage_event(usage));
                }
                let signal = value
                    .get("delta")
                    .and_then(|delta| delta.get("stop_reason"))
                    .and_then(serde_json::Value::as_str)
                    .map_or(Signal::Continue, |reason| {
                        Signal::Stopped(match reason {
                            "max_tokens" => StopReason::Length,
                            "tool_use" => StopReason::ToolCall,
                            _ => StopReason::Stop,
                        })
                    });
                (events, signal)
            }

            // The end of the stream. There is no `[DONE]`.
            "message_stop" => (events, Signal::Done),

            // An error delivered inside the stream rather than as a status.
            // It has to become the terminal failure, or the caller would see a
            // truncated answer reported as a success.
            "error" => {
                let detail = value
                    .get("error")
                    .and_then(|error| error.get("type"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown");
                // Reported as a signal, not as an event: the transport owns
                // the single terminal event, and pushing a `failed` here would
                // have one appended behind it.
                (
                    events,
                    Signal::Failed(error(
                        ErrorCode::ProviderUnavailable,
                        format!("the model endpoint reported an error: {detail}"),
                    )),
                )
            }

            _ => (events, Signal::Continue),
        }
    }
}

/// Usage as the contract records it.
///
/// Anthropic reports input and output tokens in separate frames, and omits
/// whichever it has nothing to say about. Absent stays absent rather than
/// becoming zero: zero means "none were used", which is a different claim.
fn usage_event(usage: &serde_json::Value) -> ModelEvent {
    ModelEvent::Usage {
        usage: Usage {
            input_tokens: usage
                .get("input_tokens")
                .and_then(serde_json::Value::as_u64),
            output_tokens: usage
                .get("output_tokens")
                .and_then(serde_json::Value::as_u64),
            estimated: false,
        },
    }
}
