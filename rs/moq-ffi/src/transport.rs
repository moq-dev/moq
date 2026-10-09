//! Open a browser WebTransport connection for `moq-net`.
//!
//! The browser backend is adapted to moq-net's owned poll traits here.

mod adapter;

use url::Url;
use web_transport_wasm::{ClientBuilder, Error};

/// Advertise the ALPN of every allowed version, so the peer picks one the session
/// will accept. Without this the browser negotiates no subprotocol and the session
/// has no version to start from.
fn builder(versions: &moq_net::Versions) -> ClientBuilder {
	ClientBuilder::new().with_protocols(versions.alpns())
}

/// Open a browser WebTransport connection to `url`, trusting the system roots.
pub async fn connect(url: Url, versions: &moq_net::Versions) -> Result<Session, Error> {
	builder(versions)
		.with_system_roots()
		.connect(url)
		.await
		.map(Session::new)
}

/// Connect, trusting only the given sha-256 certificate hashes (serverless dev,
/// matching the browser's `serverCertificateHashes` option).
pub async fn connect_with_hashes(
	url: Url,
	versions: &moq_net::Versions,
	hashes: Vec<Vec<u8>>,
) -> Result<Session, Error> {
	builder(versions)
		.with_server_certificate_hashes(hashes)
		.connect(url)
		.await
		.map(Session::new)
}

/// A browser session implementing moq-net's owned poll interface.
type Session = adapter::Session<web_transport_wasm::Session>;
