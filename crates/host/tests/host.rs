//! A host end to end: real TLS with a pinned self-signed certificate, a real
//! WebSocket, a real profile on disk, and the deterministic fake model.
//!
//! The certificate is made per run with the `openssl` command, as a host's
//! image makes its own (ADR 0074 §2), so no private key is committed. Where
//! `openssl` is missing the tests say so and skip, unless
//! `TRAMONTANA_REQUIRE_OPENSSL` is set, which makes that a failure.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests assert on known-good values; a panic is the failure report"
)]

use std::path::Path;
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio_native_tls::TlsStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tramontana_core::contracts::{ErrorCode, TaskUpdate};
use tramontana_core::ports::{Approver, SecretResolver};
use tramontana_core::testing::InMemorySecretVault;
use tramontana_host::{ClientFrame, ConnectError, Devices, Host, NobodyToAsk, ServerFrame, Token};

type Socket = WebSocketStream<TlsStream<TcpStream>>;

struct Running {
    address: String,
    pin: String,
    devices: Arc<Devices>,
    _dir: tempfile::TempDir,
}

/// A self-signed certificate and PKCS#8 key, or `None` without `openssl`.
fn certificate(dir: &Path) -> Option<(Vec<u8>, Vec<u8>)> {
    let made = std::process::Command::new("openssl")
        .args([
            "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
        ])
        .args(["-subj", "/CN=tramontana-host"])
        .arg("-keyout")
        .arg(dir.join("key.pem"))
        .arg("-out")
        .arg(dir.join("cert.pem"))
        .output();
    match made {
        Ok(output) if output.status.success() => Some((
            std::fs::read(dir.join("cert.pem")).unwrap(),
            std::fs::read(dir.join("key.pem")).unwrap(),
        )),
        _ => {
            assert!(
                std::env::var_os("TRAMONTANA_REQUIRE_OPENSSL").is_none(),
                "TRAMONTANA_REQUIRE_OPENSSL is set, but `openssl` could not make a certificate"
            );
            println!("SKIPPED: no `openssl` to make a test certificate");
            None
        }
    }
}

async fn start(queue: usize) -> Option<Running> {
    let dir = tempfile::tempdir().unwrap();
    let (cert, key) = certificate(dir.path())?;
    let plane = tramontana_composition::open_service_plane(
        &dir.path().join("host.sqlite"),
        Arc::new(NobodyToAsk) as Arc<dyn Approver>,
        Arc::new(InMemorySecretVault::new()) as Arc<dyn SecretResolver>,
        0,
    )
    .await
    .unwrap();
    let devices = Arc::new(Devices::open(dir.path().join("devices.json")).unwrap());
    let host = Arc::new(Host::new(Arc::new(plane.service), Arc::clone(&devices)).with_queue(queue));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let acceptor = tramontana_host::acceptor(&cert, &key).unwrap();
    tokio::spawn(host.serve(listener, acceptor));
    Some(Running {
        address,
        pin: tramontana_host::fingerprint(&cert).unwrap(),
        devices,
        _dir: dir,
    })
}

async fn send(socket: &mut Socket, frame: &ClientFrame) {
    socket
        .send(Message::text(serde_json::to_string(frame).unwrap()))
        .await
        .unwrap();
}

/// The next frame, or `None` once the host closed the connection.
async fn receive(socket: &mut Socket) -> Option<ServerFrame> {
    while let Some(Ok(message)) = socket.next().await {
        if let Message::Text(text) = message {
            return Some(serde_json::from_str(text.as_str()).unwrap());
        }
    }
    None
}

/// Pair a device as the owner and return its token.
async fn pair(running: &Running) -> Token {
    let code = running.devices.invite("owner", now()).unwrap();
    let mut socket = tramontana_host::connect(&running.address, &running.pin)
        .await
        .unwrap();
    send(
        &mut socket,
        &ClientFrame::Pair {
            schema_version: tramontana_core::SCHEMA_VERSION,
            code,
            device_name: "laptop".to_owned(),
        },
    )
    .await;
    match receive(&mut socket).await {
        Some(ServerFrame::Paired {
            principal, token, ..
        }) => {
            assert_eq!(principal, "owner");
            token
        }
        other => panic!("not paired: {other:?}"),
    }
}

async fn hello(running: &Running, token: &Token) -> Socket {
    let mut socket = tramontana_host::connect(&running.address, &running.pin)
        .await
        .unwrap();
    send(
        &mut socket,
        &ClientFrame::Hello {
            schema_version: tramontana_core::SCHEMA_VERSION,
            token: token.clone(),
        },
    )
    .await;
    assert!(matches!(
        receive(&mut socket).await,
        Some(ServerFrame::Welcome { .. })
    ));
    socket
}

async fn command(
    socket: &mut Socket,
    id: u64,
    name: &str,
    args: Value,
) -> Result<Value, ErrorCode> {
    send(
        socket,
        &ClientFrame::Command {
            id,
            name: name.to_owned(),
            args,
        },
    )
    .await;
    loop {
        match receive(socket).await {
            Some(ServerFrame::Reply {
                id: replied,
                result,
                error,
            }) if replied == id => {
                return match error {
                    Some(error) => Err(error.code),
                    None => Ok(result.unwrap_or(Value::Null)),
                };
            }
            Some(ServerFrame::Update { .. }) => {}
            other => panic!("expected the reply to {id}: {other:?}"),
        }
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

#[tokio::test]
async fn a_paired_device_is_welcomed_and_its_commands_answered() {
    let Some(running) = start(1_024).await else {
        return;
    };
    let token = pair(&running).await;
    let mut socket = hello(&running, &token).await;

    let info = command(&mut socket, 1, "core_info", Value::Null)
        .await
        .unwrap();
    assert_eq!(
        info["schemaVersion"],
        json!(tramontana_core::SCHEMA_VERSION)
    );
    assert_eq!(
        command(&mut socket, 2, "list_projects", Value::Null).await,
        Ok(json!([]))
    );
    // Unknown commands are refused, and the connection carries on.
    assert_eq!(
        command(&mut socket, 3, "erase_everything", Value::Null).await,
        Err(ErrorCode::InvalidRequest)
    );
    assert_eq!(
        command(&mut socket, 4, "start_task", json!({ "goal": "   " })).await,
        Err(ErrorCode::InvalidRequest)
    );
    assert!(
        command(&mut socket, 5, "list_tasks", Value::Null)
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn a_task_started_remotely_is_watched_to_the_end_once_in_order() {
    let Some(running) = start(1_024).await else {
        return;
    };
    let token = pair(&running).await;
    let mut socket = hello(&running, &token).await;

    let task_id = command(&mut socket, 1, "start_task", json!({ "goal": "say hello" }))
        .await
        .unwrap();
    let watch = command(
        &mut socket,
        2,
        "watch_task",
        json!({ "taskId": task_id, "afterSequence": 0 }),
    )
    .await
    .unwrap();
    let watch_id = watch["watchId"].as_u64().unwrap();

    let mut sequences = Vec::new();
    loop {
        match receive(&mut socket).await {
            Some(ServerFrame::Update { watch, update }) => {
                assert_eq!(watch, watch_id);
                if let TaskUpdate::Event { event } = *update {
                    sequences.push(event.sequence);
                    if event.is_terminal_outcome() {
                        break;
                    }
                }
            }
            other => panic!("the stream ended before the task did: {other:?}"),
        }
    }
    assert_eq!(
        sequences,
        (1..=*sequences.last().unwrap()).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn an_unknown_token_or_a_spent_invite_is_refused_and_closed() {
    let Some(running) = start(1_024).await else {
        return;
    };
    let mut socket = tramontana_host::connect(&running.address, &running.pin)
        .await
        .unwrap();
    send(
        &mut socket,
        &ClientFrame::Hello {
            schema_version: tramontana_core::SCHEMA_VERSION,
            token: Token::new("guessed"),
        },
    )
    .await;
    assert!(matches!(
        receive(&mut socket).await,
        Some(ServerFrame::Refused { .. })
    ));
    assert!(
        receive(&mut socket).await.is_none(),
        "still open after refusing"
    );

    // A command before any greeting is refused the same way.
    let mut socket = tramontana_host::connect(&running.address, &running.pin)
        .await
        .unwrap();
    send(
        &mut socket,
        &ClientFrame::Command {
            id: 1,
            name: "list_projects".to_owned(),
            args: Value::Null,
        },
    )
    .await;
    assert!(matches!(
        receive(&mut socket).await,
        Some(ServerFrame::Refused { .. })
    ));

    // An invite works once.
    let code = running.devices.invite("owner", now()).unwrap();
    for expected_paired in [true, false] {
        let mut socket = tramontana_host::connect(&running.address, &running.pin)
            .await
            .unwrap();
        send(
            &mut socket,
            &ClientFrame::Pair {
                schema_version: tramontana_core::SCHEMA_VERSION,
                code: code.clone(),
                device_name: "laptop".to_owned(),
            },
        )
        .await;
        let paired = matches!(receive(&mut socket).await, Some(ServerFrame::Paired { .. }));
        assert_eq!(paired, expected_paired);
    }
}

#[tokio::test]
async fn a_host_with_another_certificate_is_refused_before_anything_is_sent() {
    let Some(running) = start(1_024).await else {
        return;
    };
    let wrong_pin = "0".repeat(64);
    match tramontana_host::connect(&running.address, &wrong_pin).await {
        Err(ConnectError::WrongHost) => {}
        other => panic!(
            "connected to a host with the wrong certificate: {:?}",
            other.err()
        ),
    }
}

#[tokio::test]
async fn a_desktop_too_slow_for_its_queue_is_disconnected() {
    // A queue of two: the welcome, a reply, and then the task's events
    // overflow it while this test reads nothing.
    let Some(running) = start(2).await else {
        return;
    };
    let token = pair(&running).await;
    let mut socket = hello(&running, &token).await;
    let task_id = command(&mut socket, 1, "start_task", json!({ "goal": "say hello" }))
        .await
        .unwrap();
    send(
        &mut socket,
        &ClientFrame::Command {
            id: 2,
            name: "watch_task".to_owned(),
            args: json!({ "taskId": task_id, "afterSequence": 0 }),
        },
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let mut frames = 0;
    while receive(&mut socket).await.is_some() {
        frames += 1;
        assert!(frames < 1_000, "the connection was never closed");
    }
}
