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
    /// # Errors
    ///
    /// When nothing arrives before `within`, when the authorization server
    /// reported an error instead of a code, when the state does not match, or
    /// when the request is malformed or oversized.
    pub async fn wait(self, state: &str, within: Duration) -> Result<String, RedirectError> {
        let (mut stream, peer) = tokio::time::timeout(within, self.listener.accept())
            .await
            .map_err(|_| fail("nothing came back before the sign-in timed out"))?
            .map_err(|error| fail(format!("the redirect could not be accepted: {error}")))?;
        if !peer.ip().is_loopback() {
            return Err(fail("the redirect did not come from this computer"));
        }

        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        // The request line is the whole point, and it arrives first; reading
        // to the end of the headers is enough and bounds the wait.
        loop {
            let read = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut buffer))
                .await
                .map_err(|_| fail("the browser's request did not finish"))?
                .map_err(|error| {
                    fail(format!("the browser's request could not be read: {error}"))
                })?;
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

        let outcome = outcome_of(&request, state);
        let page = match &outcome {
            Ok(_) => "Signed in. You can close this tab and go back to Tramontana.",
            Err(error) => &error.0,
        };
        let response = format!(
            "HTTP/1.1 {} OK\r\ncontent-type: text/plain; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{page}",
            if outcome.is_ok() { 200 } else { 400 },
            page.len()
        );
        let _ = stream.write_all(response.as_bytes()).await;
        let _ = stream.shutdown().await;
        outcome
    }
}

/// The code in a redirect request, or why there is none.
fn outcome_of(request: &[u8], state: &str) -> Result<String, RedirectError> {
    let text = std::str::from_utf8(request)
        .map_err(|_| fail("the browser's request was not text"))?
        .lines()
        .next()
        .ok_or_else(|| fail("the browser sent an empty request"))?
        .to_owned();
    // `GET /callback?code=…&state=… HTTP/1.1`
    let target = text
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| fail("the browser's request had no address"))?;
    let query = target.split_once('?').map(|(_, query)| query).unwrap_or("");
    let mut code = None;
    let mut returned = None;
    let mut error = None;
    for pair in query.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        match key {
            "code" => code = Some(unescape(value)),
            "state" => returned = Some(unescape(value)),
            "error" => error = Some(unescape(value)),
            _ => {}
        }
    }

    if let Some(error) = error {
        return Err(fail(format!("the authorization server refused: {error}")));
    }
    // Compared before the code is read out of the request, and never reported
    // back to the browser.
    if returned.as_deref() != Some(state) {
        return Err(fail(
            "the redirect carried a different sign-in than the one that was started",
        ));
    }
    code.filter(|code| !code.is_empty())
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
