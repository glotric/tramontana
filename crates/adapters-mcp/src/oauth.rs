//! Signing in to a remote App, for servers that ask for OAuth.
//!
//! ADR 0050 reaches a remote App with a bearer token the person stored by
//! hand, and deferred the flow that obtains one. This is that flow: the
//! authorization code grant with PKCE, which is what a native application is
//! supposed to use (RFC 8252), against the authorization server the App's own
//! metadata names (RFC 9728 and RFC 8414). ADR 0056.
//!
//! What it does *not* do is make any vendor's consumer sign-in usable here.
//! This serves MCP servers that publish OAuth metadata. Anthropic's terms
//! forbid third-party Claude.ai sign-in outright, and OpenAI's is a
//! registration-gated programme rather than an open endpoint; both are
//! reached with an API key.
//!
//! Every rule ADR 0050 set for the transport holds here too: HTTPS unless the
//! server is on this computer, no redirects followed, bounded reads, and a
//! timeout on every request. A token exists only in the credential store.

use std::time::Duration;

use serde::Deserialize;
use sha2::{Digest, Sha256};

/// Largest metadata or token response read, in bytes. These documents are a
/// few hundred bytes; anything at this size is not one.
const MAX_BODY: usize = 64 * 1024;

/// How long any one request to an authorization server may take.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// Anything that stopped a sign-in, in words a person can act on.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct OAuthError(pub String);

fn fail(message: impl Into<String>) -> OAuthError {
    OAuthError(message.into())
}

/// What an authorization server says it offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizationServer {
    /// Where the person is sent to approve.
    pub authorization_endpoint: String,
    /// Where a code is exchanged for a token.
    pub token_endpoint: String,
    /// Where a client may register itself, when the server allows it.
    pub registration_endpoint: Option<String>,
    /// The scopes the App's own metadata asks for, when it names any.
    pub scopes: Vec<String>,
}

/// The secret half of one sign-in attempt. Never leaves this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attempt {
    /// The PKCE verifier, sent only to the token endpoint.
    verifier: String,
    /// The value the authorization server must hand back unchanged.
    state: String,
}

impl Attempt {
    /// The `state` this attempt expects back from the redirect.
    #[must_use]
    pub fn state(&self) -> &str {
        &self.state
    }
}

/// What a token endpoint returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tokens {
    /// The bearer token the App is called with.
    pub access: String,
    /// A token that buys a new one without asking the person again.
    pub refresh: Option<String>,
    /// Seconds until the access token expires, when the server says.
    pub expires_in: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
struct ProtectedResource {
    #[serde(default)]
    authorization_servers: Vec<String>,
    #[serde(default)]
    scopes_supported: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
struct ServerMetadata {
    authorization_endpoint: String,
    token_endpoint: String,
    #[serde(default)]
    registration_endpoint: Option<String>,
    #[serde(default)]
    code_challenge_methods_supported: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
struct Registration {
    client_id: String,
}

/// An HTTP client with the same rules as the App transport's (ADR 0050).
fn client() -> Result<reqwest::Client, OAuthError> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .build()
        .map_err(|error| fail(format!("could not build an HTTP client: {error}")))
}

/// Refuse an address that is not `https://`, unless it is on this computer.
///
/// The same rule the App's own address passes (ADR 0050). A token must not be
/// sent, or a person's approval collected, over a plain connection to
/// somewhere else.
fn check_address(url: &str, what: &str) -> Result<(), OAuthError> {
    if url.starts_with("https://") || tramontana_core::contracts::is_loopback_url(url) {
        return Ok(());
    }
    Err(fail(format!(
        "the {what} address must be https, or on this computer: {url}"
    )))
}

/// Read a bounded body, and refuse anything larger.
async fn bounded(response: reqwest::Response, what: &str) -> Result<Vec<u8>, OAuthError> {
    let body = response
        .bytes()
        .await
        .map_err(|error| fail(format!("could not read the {what}: {error}")))?;
    if body.len() > MAX_BODY {
        return Err(fail(format!("the {what} was larger than {MAX_BODY} bytes")));
    }
    Ok(body.to_vec())
}

async fn get_json<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    url: &str,
    what: &str,
) -> Result<T, OAuthError> {
    check_address(url, what)?;
    let response = tokio::time::timeout(REQUEST_TIMEOUT, client.get(url).send())
        .await
        .map_err(|_| fail(format!("the {what} request timed out")))?
        .map_err(|error| fail(format!("could not reach the {what}: {error}")))?;
    if response.status().is_redirection() {
        return Err(fail(format!(
            "the {what} redirected elsewhere; Tramontana does not follow redirects"
        )));
    }
    if !response.status().is_success() {
        return Err(fail(format!(
            "the {what} answered {}",
            response.status().as_u16()
        )));
    }
    let body = bounded(response, what).await?;
    serde_json::from_slice(&body)
        .map_err(|error| fail(format!("the {what} was not valid: {error}")))
}

/// Find the authorization server a remote App's address belongs to.
///
/// `resource_metadata` is the URL the App named in its `WWW-Authenticate`
/// header when it refused, if it named one; otherwise the well-known location
/// beside its own address is used (RFC 9728).
///
/// # Errors
///
/// When either document cannot be read, is not valid, or names no server, and
/// when the server does not support PKCE with SHA-256 — which this flow
/// requires and does not fall back from.
pub async fn discover(
    app_url: &str,
    resource_metadata: Option<&str>,
) -> Result<AuthorizationServer, OAuthError> {
    let client = client()?;
    let metadata_url = match resource_metadata {
        Some(named) => named.to_owned(),
        None => well_known(app_url, "oauth-protected-resource")?,
    };
    let resource: ProtectedResource =
        get_json(&client, &metadata_url, "App's protected-resource metadata").await?;
    let issuer = resource
        .authorization_servers
        .first()
        .ok_or_else(|| fail("the App names no authorization server to sign in with"))?;

    let metadata: ServerMetadata = get_json(
        &client,
        &well_known(issuer, "oauth-authorization-server")?,
        "authorization server's metadata",
    )
    .await?;

    // No downgrade: a server that cannot do S256 is refused rather than
    // signed in to with a weaker challenge, or none.
    if !metadata.code_challenge_methods_supported.is_empty()
        && !metadata
            .code_challenge_methods_supported
            .iter()
            .any(|method| method == "S256")
    {
        return Err(fail(
            "the authorization server does not support PKCE with SHA-256, which Tramontana requires",
        ));
    }
    check_address(&metadata.authorization_endpoint, "authorization endpoint")?;
    check_address(&metadata.token_endpoint, "token endpoint")?;
    Ok(AuthorizationServer {
        authorization_endpoint: metadata.authorization_endpoint,
        token_endpoint: metadata.token_endpoint,
        registration_endpoint: metadata.registration_endpoint,
        scopes: resource.scopes_supported,
    })
}

/// `https://host/.well-known/<name>`, from an address's origin.
fn well_known(url: &str, name: &str) -> Result<String, OAuthError> {
    let parsed =
        reqwest::Url::parse(url).map_err(|error| fail(format!("{url} is not a URL: {error}")))?;
    let origin = parsed.origin().ascii_serialization();
    Ok(format!("{origin}/.well-known/{name}"))
}

/// Register Tramontana with an authorization server that allows it
/// (RFC 7591), and return the client identifier it issued.
///
/// Only a public client: this application has no secret to keep, so it asks
/// for none and proves itself with PKCE instead.
///
/// # Errors
///
/// When the endpoint cannot be reached, refuses, or answers without an
/// identifier.
pub async fn register(
    registration_endpoint: &str,
    redirect_uri: &str,
) -> Result<String, OAuthError> {
    check_address(registration_endpoint, "registration endpoint")?;
    let client = client()?;
    let body = serde_json::json!({
        "client_name": "Tramontana",
        "redirect_uris": [redirect_uri],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
    });
    let body = serde_json::to_vec(&body)
        .map_err(|error| fail(format!("could not write the registration: {error}")))?;
    let response = tokio::time::timeout(
        REQUEST_TIMEOUT,
        client
            .post(registration_endpoint)
            .header("content-type", "application/json")
            .body(body)
            .send(),
    )
    .await
    .map_err(|_| fail("registering with the authorization server timed out"))?
    .map_err(|error| fail(format!("could not register: {error}")))?;
    if !response.status().is_success() {
        return Err(fail(format!(
            "the authorization server refused to register this application ({})",
            response.status().as_u16()
        )));
    }
    let body = bounded(response, "registration reply").await?;
    let registered: Registration = serde_json::from_slice(&body)
        .map_err(|error| fail(format!("the registration reply was not valid: {error}")))?;
    Ok(registered.client_id)
}

/// The address to send the person to, and the secret to finish with.
///
/// The verifier and the state are fresh for every attempt, and the challenge
/// is always S256 (RFC 7636).
#[must_use]
pub fn authorize(
    server: &AuthorizationServer,
    client_id: &str,
    redirect_uri: &str,
) -> (String, Attempt) {
    let verifier = format!("{}{}", random(), random());
    let state = random();
    let challenge = base64_url(&Sha256::digest(verifier.as_bytes()));
    let mut url = format!(
        "{}{}response_type=code&client_id={}&redirect_uri={}&code_challenge={challenge}&code_challenge_method=S256&state={state}",
        server.authorization_endpoint,
        if server.authorization_endpoint.contains('?') {
            "&"
        } else {
            "?"
        },
        escape(client_id),
        escape(redirect_uri),
    );
    if !server.scopes.is_empty() {
        url.push_str(&format!("&scope={}", escape(&server.scopes.join(" "))));
    }
    (url, Attempt { verifier, state })
}

/// Exchange the code the redirect carried for a token.
///
/// # Errors
///
/// When the endpoint cannot be reached, refuses the code, or answers with
/// something that is not a token.
pub async fn exchange(
    server: &AuthorizationServer,
    client_id: &str,
    redirect_uri: &str,
    code: &str,
    attempt: &Attempt,
) -> Result<Tokens, OAuthError> {
    post_token(
        server,
        &[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", client_id),
            ("code_verifier", &attempt.verifier),
        ],
    )
    .await
}

/// Buy a new token with a refresh token, without asking the person again.
///
/// # Errors
///
/// As [`exchange`]. A refused refresh means signing in again.
pub async fn refresh(
    server: &AuthorizationServer,
    client_id: &str,
    refresh_token: &str,
) -> Result<Tokens, OAuthError> {
    post_token(
        server,
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", client_id),
        ],
    )
    .await
}

async fn post_token(
    server: &AuthorizationServer,
    form: &[(&str, &str)],
) -> Result<Tokens, OAuthError> {
    check_address(&server.token_endpoint, "token endpoint")?;
    let client = client()?;
    // Written out rather than taken from a feature: the encoding of a
    // request that carries a code and a verifier is worth being able to read.
    let body = form
        .iter()
        .map(|(key, value)| format!("{}={}", escape(key), escape(value)))
        .collect::<Vec<_>>()
        .join("&");
    let response = tokio::time::timeout(
        REQUEST_TIMEOUT,
        client
            .post(&server.token_endpoint)
            .header("content-type", "application/x-www-form-urlencoded")
            .body(body)
            .send(),
    )
    .await
    .map_err(|_| fail("the token request timed out"))?
    .map_err(|error| fail(format!("could not reach the token endpoint: {error}")))?;
    if response.status().is_redirection() {
        return Err(fail(
            "the token endpoint redirected elsewhere; Tramontana does not follow redirects",
        ));
    }
    let status = response.status();
    let body = bounded(response, "token reply").await?;
    if !status.is_success() {
        // The body of a failed token request is an error document, not a
        // secret, and its `error` field is the only useful thing to say.
        let described = serde_json::from_slice::<serde_json::Value>(&body)
            .ok()
            .and_then(|value| {
                value
                    .get("error")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| status.as_u16().to_string());
        return Err(fail(format!(
            "the authorization server refused: {described}"
        )));
    }
    let tokens: TokenResponse = serde_json::from_slice(&body)
        .map_err(|error| fail(format!("the token reply was not valid: {error}")))?;
    Ok(Tokens {
        access: tokens.access_token,
        refresh: tokens.refresh_token,
        expires_in: tokens.expires_in,
    })
}

/// 32 hex characters of randomness: unreserved, and well past the 43-character
/// minimum once two are joined (RFC 7636 §4.1).
fn random() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// Percent-encode everything a query parameter may not carry literally.
fn escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                escaped.push(char::from(byte));
            }
            _ => escaped.push_str(&format!("%{byte:02X}")),
        }
    }
    escaped
}

/// base64url without padding (RFC 4648 §5), which is what a PKCE challenge is.
///
/// Written out rather than taken as a dependency: it is twenty lines, and a
/// new package in a security path costs more to justify than it saves.
fn base64_url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let (a, b, c) = (
            u32::from(chunk[0]),
            chunk.get(1).map_or(0, |byte| u32::from(*byte)),
            chunk.get(2).map_or(0, |byte| u32::from(*byte)),
        );
        let triple = (a << 16) | (b << 8) | c;
        let indices = [
            (triple >> 18) & 63,
            (triple >> 12) & 63,
            (triple >> 6) & 63,
            triple & 63,
        ];
        for (position, index) in indices.iter().enumerate() {
            if position <= chunk.len() {
                out.push(char::from(ALPHABET[*index as usize]));
            }
        }
    }
    out
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "tests assert on known-good literals; a panic is the failure report"
)]
mod tests {
    use super::*;

    #[test]
    fn the_challenge_matches_the_published_example() {
        // RFC 7636 appendix B: this verifier must produce this challenge, or
        // Tramontana's PKCE is not PKCE.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            base64_url(&Sha256::digest(verifier.as_bytes())),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn base64_url_has_no_padding_and_no_unsafe_characters() {
        assert_eq!(base64_url(b""), "");
        assert_eq!(base64_url(b"f"), "Zg");
        assert_eq!(base64_url(b"fo"), "Zm8");
        assert_eq!(base64_url(b"foo"), "Zm9v");
        assert_eq!(base64_url(b"foob"), "Zm9vYg");
        assert_eq!(base64_url(&[251, 255, 190]), "-_--");
        assert!(!base64_url(&[251, 255, 190]).contains(['+', '/', '=']));
    }

    #[test]
    fn every_attempt_is_its_own() {
        let server = AuthorizationServer {
            authorization_endpoint: "https://auth.example/authorize".to_owned(),
            token_endpoint: "https://auth.example/token".to_owned(),
            registration_endpoint: None,
            scopes: vec!["read".to_owned()],
        };
        let (first, one) = authorize(&server, "client", "http://127.0.0.1:9/callback");
        let (second, two) = authorize(&server, "client", "http://127.0.0.1:9/callback");

        assert_ne!(one.state(), two.state(), "a reused state proves nothing");
        assert_ne!(first, second);
        assert!(first.contains("code_challenge_method=S256"));
        assert!(
            !first.contains(&one.verifier),
            "the verifier is never put in the address the person opens"
        );
        assert!(first.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A9%2Fcallback"));
        assert!(first.contains("scope=read"));
    }

    #[test]
    fn a_plain_http_address_that_is_not_this_computer_is_refused() {
        assert!(check_address("http://auth.example", "authorization endpoint").is_err());
        assert!(check_address("https://auth.example", "authorization endpoint").is_ok());
        assert!(check_address("http://127.0.0.1:8080", "token endpoint").is_ok());
        assert!(check_address("http://localhost:8080", "token endpoint").is_ok());
    }

    #[test]
    fn well_known_addresses_are_built_from_the_origin_only() {
        assert_eq!(
            well_known(
                "https://apps.example/mcp/v1?x=1",
                "oauth-protected-resource"
            )
            .unwrap(),
            "https://apps.example/.well-known/oauth-protected-resource"
        );
    }
}
