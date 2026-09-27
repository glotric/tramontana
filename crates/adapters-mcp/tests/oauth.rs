//! Signing in to a remote App (ADR 0056), against an authorization server
//! written here by hand so it can also misbehave.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests assert on known-good values; a panic is the failure report"
)]

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tramontana_adapters_mcp::{authorize, discover, exchange, refresh, register};

/// How the fake authorization server behaves.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Publishes metadata, registers clients, and issues tokens.
    Ok,
    /// Says it can only do PKCE with the plain challenge method.
    PlainOnly,
    /// Redirects every request somewhere else.
    Redirect,
    /// Answers the token request with an error document.
    RefusesTheCode,
}

struct Server {
    url: String,
    /// The bodies of every POST it received, in order.
    posts: Arc<Mutex<Vec<String>>>,
}

async fn serve(mode: Mode) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let url = format!("http://{address}");
    let posts = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&posts);
    let origin = url.clone();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let log = Arc::clone(&log);
            let origin = origin.clone();
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut buffer = [0_u8; 2048];
                loop {
                    let read = socket.read(&mut buffer).await.unwrap_or(0);
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..read]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let text = String::from_utf8_lossy(&request).into_owned();
                let line = text.lines().next().unwrap_or_default().to_owned();
                let body = text
                    .split_once("\r\n\r\n")
                    .map(|(_, rest)| rest)
                    .unwrap_or("");
                if line.starts_with("POST") {
                    log.lock().unwrap().push(body.to_owned());
                }

                if mode == Mode::Redirect {
                    let _ = socket
                        .write_all(
                            b"HTTP/1.1 302 Found\r\nlocation: https://elsewhere.example/\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                        )
                        .await;
                    return;
                }

                let (status, payload) = if line.contains("/.well-known/oauth-protected-resource") {
                    (
                        200,
                        json!({
                            "resource": origin,
                            "authorization_servers": [origin],
                            "scopes_supported": ["search:read"],
                        }),
                    )
                } else if line.contains("/.well-known/oauth-authorization-server") {
                    (
                        200,
                        json!({
                            "issuer": origin,
                            "authorization_endpoint": format!("{origin}/authorize"),
                            "token_endpoint": format!("{origin}/token"),
                            "registration_endpoint": format!("{origin}/register"),
                            "code_challenge_methods_supported":
                                if mode == Mode::PlainOnly { ["plain"] } else { ["S256"] },
                        }),
                    )
                } else if line.contains("/register") {
                    (200, json!({ "client_id": "issued-client" }))
                } else if line.contains("/token") {
                    if mode == Mode::RefusesTheCode {
                        (400, json!({ "error": "invalid_grant" }))
                    } else if body.contains("grant_type=refresh_token") {
                        (
                            200,
                            json!({
                                "access_token": "refreshed-token",
                                "refresh_token": "next-refresh",
                                "expires_in": 3600,
                                "token_type": "Bearer",
                            }),
                        )
                    } else {
                        (
                            200,
                            json!({
                                "access_token": "the-token",
                                "refresh_token": "the-refresh",
                                "expires_in": 600,
                                "token_type": "Bearer",
                            }),
                        )
                    }
                } else {
                    (404, json!({ "error": "not_found" }))
                };

                let payload = serde_json::to_vec(&payload).unwrap();
                let head = format!(
                    "HTTP/1.1 {status} OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    payload.len()
                );
                let _ = socket.write_all(head.as_bytes()).await;
                let _ = socket.write_all(&payload).await;
            });
        }
    });
    Server { url, posts }
}

/// One form field out of a urlencoded body.
fn field(body: &str, name: &str) -> Option<String> {
    body.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == name).then(|| value.to_owned())
    })
}

#[tokio::test]
async fn a_sign_in_registers_proves_itself_with_pkce_and_collects_a_token() {
    let server = serve(Mode::Ok).await;
    let redirect = "http://127.0.0.1:9999/callback";

    let found = discover(&server.url, None).await.unwrap();
    assert_eq!(
        found.authorization_endpoint,
        format!("{}/authorize", server.url)
    );
    assert_eq!(found.scopes, vec!["search:read".to_owned()]);

    let client_id = register(found.registration_endpoint.as_deref().unwrap(), redirect)
        .await
        .unwrap();
    assert_eq!(client_id, "issued-client");

    let (address, attempt) = authorize(&found, &client_id, redirect);
    assert!(address.contains("code_challenge_method=S256"));
    assert!(address.contains(&format!("state={}", attempt.state())));

    let tokens = exchange(&found, &client_id, redirect, "the-code", &attempt)
        .await
        .unwrap();
    assert_eq!(tokens.access, "the-token");
    assert_eq!(tokens.refresh.as_deref(), Some("the-refresh"));
    assert_eq!(tokens.expires_in, Some(600));

    let posts = server.posts.lock().unwrap().clone();
    let registration = posts.first().unwrap();
    assert!(
        registration.contains("\"token_endpoint_auth_method\":\"none\""),
        "a public client asks for no secret: {registration}"
    );
    let token_request = posts.last().unwrap();
    assert_eq!(
        field(token_request, "grant_type").as_deref(),
        Some("authorization_code")
    );
    assert_eq!(field(token_request, "code").as_deref(), Some("the-code"));
    assert!(
        field(token_request, "code_verifier").is_some(),
        "the verifier reaches the token endpoint and nowhere else: {token_request}"
    );
    assert!(
        !address.contains(&field(token_request, "code_verifier").unwrap()),
        "and never the address the person opened"
    );
}

#[tokio::test]
async fn a_refresh_token_buys_another_without_the_person() {
    let server = serve(Mode::Ok).await;
    let found = discover(&server.url, None).await.unwrap();

    let tokens = refresh(&found, "issued-client", "the-refresh")
        .await
        .unwrap();

    assert_eq!(tokens.access, "refreshed-token");
    assert_eq!(tokens.refresh.as_deref(), Some("next-refresh"));
    let posts = server.posts.lock().unwrap().clone();
    let request = posts.last().unwrap();
    assert_eq!(
        field(request, "grant_type").as_deref(),
        Some("refresh_token")
    );
    assert!(
        !request.contains("client_secret"),
        "there is no secret to send: {request}"
    );
}

#[tokio::test]
async fn a_server_that_cannot_do_sha256_is_refused_rather_than_downgraded() {
    let server = serve(Mode::PlainOnly).await;

    let refused = discover(&server.url, None).await.unwrap_err();

    assert!(
        refused.0.contains("SHA-256"),
        "signing in with a weaker challenge is not an option: {refused}"
    );
}

#[tokio::test]
async fn metadata_that_redirects_elsewhere_is_not_followed() {
    let server = serve(Mode::Redirect).await;

    let refused = discover(&server.url, None).await.unwrap_err();

    assert!(
        refused.0.contains("does not follow redirects"),
        "a redirect could send the approval somewhere else: {refused}"
    );
}

#[tokio::test]
async fn a_refused_code_says_what_the_server_said() {
    let server = serve(Mode::Ok).await;
    let found = discover(&server.url, None).await.unwrap();
    let refusing = serve(Mode::RefusesTheCode).await;
    let refusing = tramontana_adapters_mcp::AuthorizationServer {
        token_endpoint: format!("{}/token", refusing.url),
        ..found.clone()
    };
    let (_, attempt) = authorize(&found, "issued-client", "http://127.0.0.1:9999/callback");

    let refused = exchange(
        &refusing,
        "issued-client",
        "http://127.0.0.1:9999/callback",
        "stale-code",
        &attempt,
    )
    .await
    .unwrap_err();

    assert!(
        refused.0.contains("invalid_grant"),
        "the server's own reason is the useful one: {refused}"
    );
}

#[tokio::test]
async fn an_authorization_server_reached_over_plain_http_elsewhere_is_refused() {
    // The fake server is on this computer, which is allowed. A public address
    // without TLS is not, because an approval and a token would cross it.
    let refused = discover("http://apps.example/mcp", None).await.unwrap_err();
    assert!(refused.0.contains("must be https"), "{refused}");
}

/// A metadata document larger than the limit is refused rather than read.
#[tokio::test]
async fn oversized_metadata_is_not_read() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buffer = [0_u8; 2048];
                let _ = socket.read(&mut buffer).await;
                // Valid JSON, and far past what a metadata document is.
                let padding = "a".repeat(128 * 1024);
                let payload =
                    serde_json::to_vec(&json!({ "authorization_servers": [], "pad": padding }))
                        .unwrap();
                let head = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    payload.len()
                );
                let _ = socket.write_all(head.as_bytes()).await;
                let _ = socket.write_all(&payload).await;
            });
        }
    });

    let refused = discover(&url, None).await.unwrap_err();

    assert!(refused.0.contains("larger than"), "{refused}");
}

/// The metadata address the App itself named is used when it names one.
#[tokio::test]
async fn the_app_s_own_metadata_address_is_honoured() {
    let server = serve(Mode::Ok).await;
    let named = format!("{}/.well-known/oauth-protected-resource", server.url);

    let found = discover("https://somewhere.else.example/mcp", Some(&named))
        .await
        .unwrap();

    assert_eq!(found.token_endpoint, format!("{}/token", server.url));
    let _: Value = json!({});
}
