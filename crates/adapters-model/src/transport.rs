//! The transport every model adapter shares.
//!
//! Cancellation, deadlines, status mapping, and the rule that a stream ends
//! with **exactly one** terminal event are the same for every provider, and
//! they are the parts that are easy to get subtly wrong. There is one
//! implementation of them here, and each provider contributes only what is
//! genuinely different: the shape of its request and the meaning of its
//! frames.
//!
//! Two adapters with two copies of this logic would be two chances to end a
//! stream without a terminal event, and the second copy is always the one
//! nobody re-reads.

use std::time::Duration;

use futures_util::StreamExt;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tramontana_core::contracts::{CorrelationId, ErrorCode, ErrorEnvelope, ModelEvent, StopReason};

use crate::sse::EventStreamDecoder;

/// What one decoded frame means for the stream as a whole.
///
/// A wire format reports a failure *here* rather than by producing a
/// [`ModelEvent::Failed`] of its own. The transport owns the terminal event,
/// and it owns it exclusively: a decoder that could emit one too would end a
/// stream with two, which is what happened the first time this was written —
/// an Anthropic `error` frame produced a `failed` and then the transport
/// appended a `completed` behind it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Signal {
    /// Nothing that ends the stream.
    Continue,
    /// The provider reported why generation stopped.
    Stopped(StopReason),
    /// The provider signalled end of stream.
    Done,
    /// The provider reported a failure inside the stream.
    Failed(ErrorEnvelope),
}

/// One provider's idea of what its frames mean.
///
/// A pure function from a decoded payload to events: no IO, no channel, no
/// cancellation. That is what makes a wire format testable on its own, and it
/// is why the tricky asynchronous parts are not in here.
pub(crate) trait WireFormat: Send + Sync + 'static {
    /// Translate one decoded `data:` payload.
    ///
    /// `offset` is the running frame counter the caller owns; an
    /// implementation increments it once per event it produces, so offsets are
    /// contiguous across event kinds.
    fn decode(&self, payload: &str, stream_id: &str, offset: &mut u64)
    -> (Vec<ModelEvent>, Signal);
}

/// Run the request and forward its events, returning the terminal one.
///
/// Written to have a single exit that produces the terminal event, so no
/// branch can end the stream without one.
/// The HTTP client every model adapter sends with.
pub(crate) fn http_client() -> Result<reqwest::Client, ErrorEnvelope> {
    reqwest::Client::builder()
        // Connection setup only. The whole-request bound is the deadline
        // carried on each `ModelRequest`, which a caller can vary; a fixed
        // client timeout would silently override it.
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
        })
}

/// Stream `builder`'s response through `wire` on its own task.
pub(crate) fn spawn_stream(
    builder: reqwest::RequestBuilder,
    wire: impl WireFormat,
    stream_id: String,
    deadline: Duration,
    cancel: CancellationToken,
) -> mpsc::Receiver<ModelEvent> {
    let (sender, receiver) = mpsc::channel(32);
    tokio::spawn(async move {
        let outcome = pump(builder, &wire, &stream_id, deadline, &cancel, &sender).await;
        // Exactly one terminal event, whatever happened above — including
        // when the consumer has already gone away, in which case the send
        // fails harmlessly.
        let _ = sender.send(outcome).await;
    });
    receiver
}

pub(crate) async fn pump(
    builder: reqwest::RequestBuilder,
    wire: &dyn WireFormat,
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
            match emit(wire, &payload, stream_id, &mut offset, sender).await {
                Emitted::Continue => {}
                Emitted::Stopped(reason) => stop_reason = reason,
                Emitted::Done => return ModelEvent::Completed { stop_reason },
                Emitted::Failed(error) => return ModelEvent::Failed { error },
                Emitted::ConsumerGone => return cancelled,
            }
        }
    }

    if let Some(payload) = decoder.finish() {
        match emit(wire, &payload, stream_id, &mut offset, sender).await {
            Emitted::Stopped(reason) => stop_reason = reason,
            Emitted::Failed(error) => return ModelEvent::Failed { error },
            Emitted::ConsumerGone => return cancelled,
            _ => {}
        }
    }

    ModelEvent::Completed { stop_reason }
}

/// What sending one frame's events did.
enum Emitted {
    /// Nothing that ends the stream.
    Continue,
    /// The provider reported why generation stopped.
    Stopped(StopReason),
    /// The provider signalled end of stream.
    Done,
    /// The provider reported a failure inside the stream.
    Failed(ErrorEnvelope),
    /// Nobody is listening any more.
    ConsumerGone,
}

/// Decode one payload and send whatever it produced.
async fn emit(
    wire: &dyn WireFormat,
    payload: &str,
    stream_id: &str,
    offset: &mut u64,
    sender: &mpsc::Sender<ModelEvent>,
) -> Emitted {
    let (events, signal) = wire.decode(payload, stream_id, offset);
    for event in events {
        if sender.send(event).await.is_err() {
            return Emitted::ConsumerGone;
        }
    }
    match signal {
        Signal::Continue => Emitted::Continue,
        Signal::Stopped(reason) => Emitted::Stopped(reason),
        Signal::Done => Emitted::Done,
        Signal::Failed(error) => Emitted::Failed(error),
    }
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

pub(crate) fn error(code: ErrorCode, message: impl Into<String>) -> ErrorEnvelope {
    // One correlation id for the transport, which is now shared by every
    // adapter. Naming a single provider here would mislabel the other's errors.
    ErrorEnvelope::new(code, message, CorrelationId::new("model.adapter"))
}

/// Map an HTTP status onto the normalized taxonomy.
///
/// The distinction that matters to a caller is whether retrying could help and
/// whether the user has to do something. A 401 is a configuration problem, a
/// 429 is a wait, a 5xx is the provider's.
pub(crate) fn code_for_status(status: u16) -> ErrorCode {
    match status {
        401 | 403 => ErrorCode::ProviderAuthentication,
        408 => ErrorCode::Timeout,
        429 => ErrorCode::ProviderRateLimited,
        400..=499 => ErrorCode::InvalidRequest,
        _ => ErrorCode::ProviderUnavailable,
    }
}

/// Refuse an endpoint that would send a credential in the clear.
///
/// Plain HTTP is allowed only to loopback. Everything else must be `https://`.
/// Before TLS existed the restriction was implicit — there was no way to reach
/// a remote host at all — and now that it exists it has to be enforced rather
/// than assumed: a key sent to a remote host over plain HTTP is disclosed to
/// every hop between here and there, and a typo in a configuration field
/// should not be able to do that quietly
/// (`docs/decisions/0009-tls-for-cloud-adapters.md`).
///
/// # Errors
///
/// [`ErrorCode::InvalidRequest`] for a scheme that is neither, or for plain
/// HTTP to anything but loopback.
pub(crate) fn check_endpoint(base_url: &str) -> Result<(), ErrorEnvelope> {
    if base_url.starts_with("https://") {
        return Ok(());
    }
    let Some(rest) = base_url.strip_prefix("http://") else {
        return Err(error(
            ErrorCode::InvalidRequest,
            format!("model endpoint must be an http or https URL, got {base_url:?}"),
        ));
    };
    let host = host_of(rest);
    if matches!(host, "localhost" | "127.0.0.1" | "::1") {
        return Ok(());
    }
    Err(error(
        ErrorCode::InvalidRequest,
        format!(
            "plain http is allowed only to loopback; use https for {host:?} so \
             the credential is not sent in the clear"
        ),
    ))
}

/// The host in an authority, with any userinfo, port, and IPv6 brackets gone.
///
/// Written out step by step rather than as one expression, because this is the
/// check that decides whether a credential may travel unencrypted. A parser
/// that is hard to read is hard to be sure of, and the interesting inputs here
/// are the adversarial ones: `http://evil.test/@localhost` and
/// `http://localhost@evil.test/` must not both be read as loopback.
fn host_of(after_scheme: &str) -> &str {
    // The authority ends at the first `/`, `?`, or `#`. Anything after that is
    // path or query and cannot change which host is contacted.
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    // Userinfo comes *before* the last `@` in the authority.
    let host_port = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    // An IPv6 literal is bracketed, and only a colon after the closing bracket
    // is a port separator.
    if let Some(rest) = host_port.strip_prefix('[') {
        return rest.split(']').next().unwrap_or_default();
    }
    // A bare host has at most one colon, and it introduces the port.
    host_port.split(':').next().unwrap_or_default()
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests assert on known-good literals; a panic is the failure report"
)]
mod endpoint_tests {
    use super::*;

    #[test]
    fn loopback_may_be_plain_http() {
        for allowed in [
            "http://localhost:8080",
            "http://127.0.0.1:11434",
            "http://[::1]:1234",
            "http://localhost",
            "http://127.0.0.1/v1",
        ] {
            assert!(
                check_endpoint(allowed).is_ok(),
                "refused loopback: {allowed}"
            );
        }
    }

    #[test]
    fn a_remote_host_over_plain_http_is_refused() {
        // The whole point: a credential must not be sent in the clear because
        // a configuration field had the wrong scheme in it.
        for refused in [
            "http://api.openai.com",
            "http://192.168.1.10:8080",
            "http://example.test/v1",
        ] {
            let error = check_endpoint(refused).unwrap_err();
            assert_eq!(error.code, ErrorCode::InvalidRequest, "for {refused}");
            assert!(error.message.contains("loopback"), "{}", error.message);
        }
    }

    #[test]
    fn a_hostname_that_merely_contains_loopback_is_not_loopback() {
        // `localhost.evil.test` and friends. A `contains` check would pass
        // these, which is why the host is parsed rather than searched.
        for refused in [
            "http://localhost.evil.test",
            "http://notlocalhost",
            "http://127.0.0.1.evil.test",
            "http://evil.test/localhost",
            "http://evil.test/?h=127.0.0.1",
            "http://evil.test#localhost",
        ] {
            assert!(
                check_endpoint(refused).is_err(),
                "accepted a non-loopback host: {refused}"
            );
        }
    }

    #[test]
    fn userinfo_cannot_disguise_a_remote_host_as_loopback() {
        // `http://localhost@evil.test/` contacts evil.test. Reading the host
        // as the text before the `@` would get this exactly backwards.
        assert!(check_endpoint("http://localhost@evil.test/").is_err());
        assert!(check_endpoint("http://127.0.0.1:80@evil.test/").is_err());
        // And the mirror image is genuinely loopback.
        assert!(check_endpoint("http://user@localhost:8080/").is_ok());
        assert!(check_endpoint("http://user@evil.test@localhost/").is_ok());
    }

    #[test]
    fn https_is_accepted_for_any_host() {
        assert!(check_endpoint("https://api.anthropic.com").is_ok());
        assert!(check_endpoint("https://localhost:8443").is_ok());
    }

    #[test]
    fn a_scheme_that_is_neither_is_refused() {
        for refused in [
            "ftp://example.test",
            "file:///etc/passwd",
            "example.test",
            "",
        ] {
            let error = check_endpoint(refused).unwrap_err();
            assert_eq!(error.code, ErrorCode::InvalidRequest, "for {refused}");
        }
    }
}
