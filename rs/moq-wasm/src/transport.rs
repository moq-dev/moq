//! Open a browser WebTransport connection for `moq-net`.
//!
//! `web-transport-wasm` implements the poll traits `moq-net` requires, so all that
//! is left here is the dial: the ALPN list, and the browser's two trust modes.

use url::Url;
use web_transport_wasm::{ClientBuilder, Error};

/// The connected browser WebTransport session `moq-net` runs over.
pub use web_transport_wasm::Session;

/// Options for a browser WebTransport connection.
///
/// Build via [`Default`] and set fields; new knobs are added here rather than
/// as new [`connect`] parameters.
#[derive(Default)]
#[non_exhaustive]
pub struct Options {
	/// Trust only these sha-256 certificate hashes instead of the system roots
	/// (serverless dev, matching the browser's `serverCertificateHashes`).
	pub server_certificate_hashes: Vec<Vec<u8>>,
}

/// Advertise every version `moq-net` supports, so the peer picks one via ALPN.
/// Without this the browser negotiates no subprotocol and the session has no
/// version to start from.
fn builder() -> ClientBuilder {
	ClientBuilder::new().with_protocols(moq_net::ALPNS.iter().copied())
}

/// Open a browser WebTransport connection to `url`.
pub async fn connect(url: Url, options: Options) -> Result<Session, Error> {
	let client = builder();
	let client = match options.server_certificate_hashes.is_empty() {
		true => client.with_system_roots(),
		false => client.with_server_certificate_hashes(options.server_certificate_hashes),
	};
	client.connect(url).await
}
