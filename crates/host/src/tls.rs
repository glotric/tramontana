//! TLS with a pinned certificate (ADR 0074 §2).
//!
//! A host on a home or office network has no domain and no certificate
//! authority, so its certificate is self-signed and the desktop pins it: the
//! invite link carries the certificate's SHA-256, and a desktop that sees
//! any other certificate closes the connection before sending anything.
//! Skipping CA validation is only ever done together with that check.

use sha2::{Digest, Sha256};
use tokio::net::TcpStream;
use tokio_native_tls::TlsStream;
use tokio_tungstenite::WebSocketStream;

/// The server side: the host's certificate and key, both PEM, the key in
/// PKCS#8 (`-----BEGIN PRIVATE KEY-----`).
///
/// # Errors
///
/// When the certificate or key cannot be read by the platform's TLS.
pub fn acceptor(
    certificate_pem: &[u8],
    key_pem: &[u8],
) -> Result<tokio_native_tls::TlsAcceptor, native_tls::Error> {
    let identity = native_tls::Identity::from_pkcs8(certificate_pem, key_pem)?;
    Ok(native_tls::TlsAcceptor::new(identity)?.into())
}

/// The pin for a certificate: SHA-256 of its DER encoding, lowercase hex.
///
/// # Errors
///
/// When `certificate_pem` is not a certificate.
pub fn fingerprint(certificate_pem: &[u8]) -> Result<String, native_tls::Error> {
    Ok(der_fingerprint(
        &native_tls::Certificate::from_pem(certificate_pem)?.to_der()?,
    ))
}

fn der_fingerprint(der: &[u8]) -> String {
    Sha256::digest(der)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Why a desktop did not connect.
#[derive(Debug)]
pub enum ConnectError {
    /// The host could not be reached, or TLS or WebSocket setup failed.
    Unreachable(String),
    /// The host's certificate is not the pinned one. Nothing was sent.
    WrongHost,
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreachable(reason) => write!(f, "the host could not be reached: {reason}"),
            Self::WrongHost => {
                f.write_str("this is not the host the invite named; its certificate does not match")
            }
        }
    }
}

impl std::error::Error for ConnectError {}

/// The desktop side: connect to `address` (`host:port`) and accept only the
/// certificate whose fingerprint is `pin`.
///
/// # Errors
///
/// [`ConnectError::WrongHost`] before anything is sent when the certificate
/// does not match; [`ConnectError::Unreachable`] otherwise.
pub async fn connect(
    address: &str,
    pin: &str,
) -> Result<WebSocketStream<TlsStream<TcpStream>>, ConnectError> {
    let unreachable = |error: &dyn std::fmt::Display| ConnectError::Unreachable(error.to_string());
    let tcp = TcpStream::connect(address)
        .await
        .map_err(|error| unreachable(&error))?;
    // Certificate authorities are not consulted: the pin below is the check.
    let connector = native_tls::TlsConnector::builder()
        .danger_accept_invalid_certs(true)
        .danger_accept_invalid_hostnames(true)
        .build()
        .map_err(|error| unreachable(&error))?;
    let tls = tokio_native_tls::TlsConnector::from(connector)
        .connect("tramontana-host", tcp)
        .await
        .map_err(|error| unreachable(&error))?;
    let presented = tls
        .get_ref()
        .peer_certificate()
        .ok()
        .flatten()
        .and_then(|certificate| certificate.to_der().ok())
        .map(|der| der_fingerprint(&der));
    if presented.as_deref() != Some(pin) {
        return Err(ConnectError::WrongHost);
    }
    let (socket, _) = tokio_tungstenite::client_async(format!("wss://{address}/"), tls)
        .await
        .map_err(|error| unreachable(&error))?;
    Ok(socket)
}
