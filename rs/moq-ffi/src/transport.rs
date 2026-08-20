//! Open a browser WebTransport connection for `moq-net`.
//!
//! `web-transport-wasm` implements the poll traits `moq-net` requires, so all that
//! is left here is the dial: the ALPN list, and the browser's two trust modes.

use url::Url;
use web_transport_wasm::{ClientBuilder, Error, Session};

/// Advertise every version `moq-net` supports, so the peer picks one via ALPN.
/// Without this the browser negotiates no subprotocol and the session has no
/// version to start from.
fn builder() -> ClientBuilder {
	ClientBuilder::new().with_protocols(moq_net::ALPNS.iter().copied())
}

/// Open a browser WebTransport connection to `url`, trusting the system roots.
pub async fn connect(url: Url) -> Result<Session, Error> {
	builder().with_system_roots().connect(url).await
}

/// Connect, trusting only the given sha-256 certificate hashes (serverless dev,
/// matching the browser's `serverCertificateHashes` option).
pub async fn connect_with_hashes(url: Url, hashes: Vec<Vec<u8>>) -> Result<Session, Error> {
	builder().with_server_certificate_hashes(hashes).connect(url).await
}
