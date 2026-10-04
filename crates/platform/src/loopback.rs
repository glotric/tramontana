//! Catching an OAuth redirect on this computer, once.
//!
//! A native application signing in gets its authorization code back on a
//! loopback address rather than a custom scheme (RFC 8252 §7.3): the operating
//! system decides who may listen on a port, and nothing leaves the machine.
//!
//! Deliberately small. It binds `127.0.0.1` only, serves exactly one request,
//! reads a bounded amount, gives up after a deadline, and returns the code to
//! its caller and to nobody else — the page the browser is left on says only
//! that it worked (ADR 0056).

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Most bytes read from the browser before the request is refused. A redirect
/// is a request line and a few headers.
const MAX_REQUEST: usize = 16 * 1024;

/// Why a redirect was not received.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct RedirectError(pub String);

fn fail(message: impl Into<String>) -> RedirectError {
    RedirectError(message.into())
}

/// A one-shot listener for an authorization redirect.
#[derive(Debug)]
pub struct Redirect {
    listener: TcpListener,
    uri: String,
}

impl Redirect {
    /// Listen on a free port of this computer's loopback address.
    ///
    /// # Errors
    ///
    /// When the port cannot be bound.
    pub async fn bind() -> Result<Self, RedirectError> {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(|error| fail(format!("could not listen for the redirect: {error}")))?;
        let port = listener
            .local_addr()
            .map_err(|error| fail(format!("could not read the redirect port: {error}")))?
            .port();
        Ok(Self {
            listener,
            uri: format!("http://127.0.0.1:{port}/callback"),
        })
    }

    /// The address to register as the redirect, and to send in the request.
    #[must_use]
    pub fn uri(&self) -> &str {
        &self.uri
    }

    /// Wait for the browser to arrive, and return the code it carried.
    ///
    /// The `state` must come back exactly as it was sent, or the redirect is
    /// refused: a code arriving with someone else's state is not an answer to
    /// this request.
    ///
    /// Connections that are not the redirect — a browser's speculative
    /// connection that sends nothing, a request for `/favicon.ico`, a
    /// callback carrying another sign-in's state — are answered and the wait
    /// goes on. Only the deadline, the authorization server's own refusal, or
    /// a callback for this sign-in ends it, so a stray connection cannot
    /// fail a sign-in the person is in the middle of.
    ///
    /// # Errors
    ///
    /// When nothing arrives before `within`, when the authorization server
    /// reported an error instead of a code, or when the redirect for this
    /// sign-in carried no code.
    pub async fn wait(self, state: &str, within: Duration) -> Result<String, RedirectError> {
        let deadline = tokio::time::Instant::now() + within;
        loop {
            let (mut stream, peer) = tokio::time::timeout_at(deadline, self.listener.accept())
                .await
                .map_err(|_| fail("nothing came back before the sign-in timed out"))?
                .map_err(|error| fail(format!("the redirect could not be accepted: {error}")))?;
            if !peer.ip().is_loopback() {
                continue;
            }
            let request = match read_head(&mut stream, deadline).await {
                Ok(request) => request,
                Err(error) => {
                    respond(&mut stream, 400, &error.0).await;
                    continue;
                }
            };
            if !is_callback(&request) {
                respond(&mut stream, 404, "Not found.").await;
                continue;
            }
            let outcome = outcome_of(&request, state);
            if outcome.is_err() && !answers_this_sign_in(&request, state) {
                // Someone else's state, or none: not this sign-in's answer.
                respond(
                    &mut stream,
                    400,
                    "This is not the sign-in Tramontana is waiting for.",
                )
                .await;
                continue;
            }
            match &outcome {
                Ok(_) => {
                    respond(
                        &mut stream,
                        200,
                        "Signed in. You can close this tab and go back to Tramontana.",
                    )
                    .await;
                }
                Err(error) => respond(&mut stream, 400, &error.0).await,
            }
            return outcome;
        }
    }
}

/// Read a request's head: up to the blank line, bounded in size and time.
async fn read_head(
    stream: &mut tokio::net::TcpStream,
    deadline: tokio::time::Instant,
) -> Result<Vec<u8>, RedirectError> {
    // One connection never holds the wait past its own few seconds.
    let until = deadline.min(tokio::time::Instant::now() + Duration::from_secs(10));
    let mut request = Vec::new();
    let mut buffer = [0_u8; 1024];
    loop {
        let read = tokio::time::timeout_at(until, stream.read(&mut buffer))
            .await
            .map_err(|_| fail("the browser's request did not finish"))?
            .map_err(|error| fail(format!("the browser's request could not be read: {error}")))?;
        if read == 0 {
            break;
        }
        request.extend_from_slice(&buffer[..read]);
        if request.len() > MAX_REQUEST {
            return Err(fail("the browser's request was too large to be a redirect"));
        }
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    Ok(request)
}

/// Answer the browser, and close.
async fn respond(stream: &mut tokio::net::TcpStream, status: u16, page: &str) {
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        _ => "Bad Request",
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: text/plain; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{page}",
        page.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// The request target's path and query, from the request line.
fn target_of(request: &[u8]) -> Option<(&str, &str)> {
    let line = std::str::from_utf8(request).ok()?.lines().next()?;
    let target = line.split_whitespace().nth(1)?;
    Some(target.split_once('?').unwrap_or((target, "")))
}

/// Whether the request is for the redirect address at all.
fn is_callback(request: &[u8]) -> bool {
    target_of(request).is_some_and(|(path, _)| path == "/callback")
}

/// The decoded values of `key` in the request's query.
fn query_value(request: &[u8], key: &str) -> Option<String> {
    let (_, query) = target_of(request)?;
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(name, _)| *name == key)
        .map(|(_, value)| unescape(value))
}

/// Whether a callback is this sign-in's to answer: it carries this state, or
/// the authorization server's refusal.
fn answers_this_sign_in(request: &[u8], state: &str) -> bool {
    query_value(request, "error").is_some()
        || query_value(request, "state").as_deref() == Some(state)
}

/// The code in a redirect request, or why there is none.
fn outcome_of(request: &[u8], state: &str) -> Result<String, RedirectError> {
    if target_of(request).is_none() {
        return Err(fail("the browser's request had no address"));
    }
    if let Some(error) = query_value(request, "error") {
        return Err(fail(format!("the authorization server refused: {error}")));
    }
    // Compared before the code is read out of the request, and never reported
    // back to the browser.
    if query_value(request, "state").as_deref() != Some(state) {
        return Err(fail(
            "the redirect carried a different sign-in than the one that was started",
        ));
    }
    query_value(request, "code")
        .filter(|code| !code.is_empty())
        .ok_or_else(|| fail("the redirect carried no authorization code"))
}

/// Undo percent-encoding, and `+` for a space, in one query value.
fn unescape(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' if index + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(byte) => {
                        out.push(byte);
                        index += 3;
                    }
                    Err(_) => {
                        out.push(b'%');
                        index += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert on known-good literals; a panic is the failure report"
)]
mod tests {
    use super::*;

    fn get(target: &str) -> Vec<u8> {
        format!("GET {target} HTTP/1.1\r\nhost: 127.0.0.1\r\n\r\n").into_bytes()
    }

    #[test]
    fn the_code_is_read_from_the_request_line() {
        assert_eq!(
            outcome_of(&get("/callback?code=abc123&state=s1"), "s1").unwrap(),
            "abc123"
        );
        assert_eq!(
            outcome_of(&get("/callback?state=s1&code=a%2Fb+c"), "s1").unwrap(),
            "a/b c"
        );
    }

    #[test]
    fn a_redirect_carrying_someone_else_s_state_is_refused() {
        let refused = outcome_of(&get("/callback?code=abc123&state=other"), "s1").unwrap_err();
        assert!(refused.0.contains("different sign-in"));
        // And one carrying no state at all.
        assert!(outcome_of(&get("/callback?code=abc123"), "s1").is_err());
    }

    #[test]
    fn the_server_s_own_refusal_is_reported_rather_than_read_as_a_code() {
        let refused = outcome_of(&get("/callback?error=access_denied&state=s1"), "s1").unwrap_err();
        assert!(refused.0.contains("access_denied"));
    }

    #[test]
    fn a_redirect_with_no_code_is_not_a_sign_in() {
        assert!(outcome_of(&get("/callback?state=s1"), "s1").is_err());
        assert!(outcome_of(&get("/callback?code=&state=s1"), "s1").is_err());
        assert!(outcome_of(&get("/callback"), "s1").is_err());
    }

    #[tokio::test]
    async fn a_browser_arriving_at_the_address_hands_the_code_over_once() {
        let redirect = Redirect::bind().await.unwrap();
        let uri = redirect.uri().to_owned();
        assert!(uri.starts_with("http://127.0.0.1:"));

        let waiting =
            tokio::spawn(async move { redirect.wait("state-1", Duration::from_secs(5)).await });
        // What the browser does with the address it was sent to.
        let answered = reqwest_like(&format!("{uri}?code=the-code&state=state-1")).await;

        assert_eq!(waiting.await.unwrap().unwrap(), "the-code");
        assert!(
            answered.contains("Signed in"),
            "the page says it worked and nothing else: {answered}"
        );
        assert!(
            !answered.contains("the-code"),
            "the code is never written back to the browser"
        );
    }

    #[tokio::test]
    async fn a_sign_in_nobody_finishes_gives_up() {
        let redirect = Redirect::bind().await.unwrap();
        let refused = redirect
            .wait("state-1", Duration::from_millis(50))
            .await
            .unwrap_err();
        assert!(refused.0.contains("timed out"));
    }

    #[tokio::test]
    async fn stray_connections_do_not_end_the_wait() {
        // A browser's speculative connection that sends nothing, a favicon
        // request, and a callback for another sign-in all arrived first. Any
        // one of them used to fail the sign-in.
        let redirect = Redirect::bind().await.unwrap();
        let uri = redirect.uri().to_owned();
        let authority = uri
            .trim_start_matches("http://")
            .split('/')
            .next()
            .unwrap()
            .to_owned();
        let waiting =
            tokio::spawn(async move { redirect.wait("state-1", Duration::from_secs(5)).await });

        drop(tokio::net::TcpStream::connect(&authority).await.unwrap());
        let favicon = reqwest_like(&format!("http://{authority}/favicon.ico")).await;
        assert!(favicon.starts_with("HTTP/1.1 404 Not Found"), "{favicon}");
        let forged = reqwest_like(&format!("{uri}?code=forged&state=other")).await;
        assert!(forged.starts_with("HTTP/1.1 400 Bad Request"), "{forged}");

        let answered = reqwest_like(&format!("{uri}?code=the-code&state=state-1")).await;
        assert!(answered.starts_with("HTTP/1.1 200 OK"), "{answered}");
        assert_eq!(waiting.await.unwrap().unwrap(), "the-code");
    }

    #[tokio::test]
    async fn the_server_s_refusal_ends_the_wait() {
        let redirect = Redirect::bind().await.unwrap();
        let uri = redirect.uri().to_owned();
        let waiting =
            tokio::spawn(async move { redirect.wait("state-1", Duration::from_secs(5)).await });
        let answered = reqwest_like(&format!("{uri}?error=access_denied&state=state-1")).await;
        assert!(
            answered.starts_with("HTTP/1.1 400 Bad Request"),
            "{answered}"
        );
        assert!(
            waiting
                .await
                .unwrap()
                .unwrap_err()
                .0
                .contains("access_denied")
        );
    }

    /// One plain GET, without depending on an HTTP client in this crate.
    async fn reqwest_like(url: &str) -> String {
        let rest = url.trim_start_matches("http://");
        let (authority, target) = rest.split_once('/').unwrap();
        let mut stream = tokio::net::TcpStream::connect(authority).await.unwrap();
        stream
            .write_all(format!("GET /{target} HTTP/1.1\r\nhost: {authority}\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut answer = Vec::new();
        stream.read_to_end(&mut answer).await.unwrap();
        String::from_utf8_lossy(&answer).into_owned()
    }
}
