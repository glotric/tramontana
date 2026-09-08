use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::contracts::{
    CorrelationId, ErrorCode, ErrorEnvelope, Health, ModelCapability, ModelDescriptor,
    ModelLocation, ProviderId,
};
use crate::ports::{ModelEvent, ModelProvider, ModelRequest, StopReason};

/// What a fake model should do when asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FakeModelScript {
    /// Emit each chunk in order, then complete.
    Chunks(Vec<String>),
    /// Fail immediately with this code.
    Fail(ErrorCode),
}

/// A model provider that emits a scripted response.
///
/// Deterministic by construction: no network, no timing, and the same input
/// always produces the same event sequence. Cancellation is honored between
/// chunks, which is what lets a test cancel *during* a stream without racing a
/// real clock.
#[derive(Debug)]
pub struct FakeModelProvider {
    script: FakeModelScript,
    /// Pause between chunks.
    ///
    /// Zero in tests, which keeps them fast and deterministic. A demonstration
    /// sets it so streaming and cancellation are observable in real time —
    /// without it the fake finishes before any cancel can be requested, which
    /// makes the cancellation path impossible to *see* even though it works.
    chunk_delay: std::time::Duration,
}

impl FakeModelProvider {
    /// A provider that streams `chunks` as fast as possible, then completes.
    #[must_use]
    pub fn streaming(chunks: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            script: FakeModelScript::Chunks(chunks.into_iter().map(Into::into).collect()),
            chunk_delay: std::time::Duration::ZERO,
        }
    }

    /// A provider that pauses `delay_ms` between chunks.
    #[must_use]
    pub fn streaming_paced(
        chunks: impl IntoIterator<Item = impl Into<String>>,
        delay_ms: u64,
    ) -> Self {
        Self {
            chunk_delay: std::time::Duration::from_millis(delay_ms),
            ..Self::streaming(chunks)
        }
    }

    /// A provider that always fails with `code`.
    #[must_use]
    pub fn failing(code: ErrorCode) -> Self {
        Self {
            script: FakeModelScript::Fail(code),
            chunk_delay: std::time::Duration::ZERO,
        }
    }

    /// The descriptor this fake reports.
    #[must_use]
    pub fn descriptor() -> ModelDescriptor {
        ModelDescriptor {
            provider_id: "fake".to_owned(),
            model_id: "fake.deterministic".to_owned(),
            display_name: "Deterministic fake".to_owned(),
            location: ModelLocation::Local,
            capabilities: vec![
                ModelCapability::Text,
                ModelCapability::Streaming,
                ModelCapability::Cancellation,
            ],
            context_limit: Some(8_192),
            health: Health::Available,
            health_reason_code: None,
        }
    }
}

#[async_trait]
impl ModelProvider for FakeModelProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("fake")
    }

    async fn list_models(&self) -> Vec<ModelDescriptor> {
        vec![Self::descriptor()]
    }

    async fn stream(
        &self,
        _request: ModelRequest,
        cancel: CancellationToken,
    ) -> Result<mpsc::Receiver<ModelEvent>, ErrorEnvelope> {
        let script = self.script.clone();
        let chunk_delay = self.chunk_delay;
        // Bounded so a slow consumer applies backpressure rather than letting
        // an unbounded queue grow, which is how a real provider must behave.
        let (sender, receiver) = mpsc::channel(16);

        tokio::spawn(async move {
            match script {
                FakeModelScript::Fail(code) => {
                    let error = ErrorEnvelope::new(
                        code,
                        "the fake model was scripted to fail",
                        CorrelationId::new("fake"),
                    );
                    let _ = sender.send(ModelEvent::Failed { error }).await;
                }
                FakeModelScript::Chunks(chunks) => {
                    for (offset, text) in chunks.into_iter().enumerate() {
                        if !chunk_delay.is_zero() {
                            // Cancellation wins the race rather than waiting
                            // out the pause, so a cancel is felt immediately.
                            tokio::select! {
                                () = tokio::time::sleep(chunk_delay) => {}
                                () = cancel.cancelled() => {}
                            }
                        }
                        if cancel.is_cancelled() {
                            // Exactly one terminal event, even when cancelled
                            // mid-stream (`CORE_CONTRACTS.md` §8).
                            let _ = sender
                                .send(ModelEvent::Completed {
                                    stop_reason: StopReason::Cancelled,
                                })
                                .await;
                            return;
                        }
                        let event = ModelEvent::TextDelta {
                            offset: offset as u64,
                            text,
                        };
                        if sender.send(event).await.is_err() {
                            return; // The consumer went away.
                        }
                    }
                    let _ = sender
                        .send(ModelEvent::Completed {
                            stop_reason: StopReason::Stop,
                        })
                        .await;
                }
            }
        });

        Ok(receiver)
    }
}
