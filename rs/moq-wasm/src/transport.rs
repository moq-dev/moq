//! Adapt `web-transport-wasm` (the browser WebTransport API) to the
//! `web-transport-trait` abstraction that `moq-net` is generic over.
//!
//! Both the trait and the wasm types are foreign, so the orphan rule forces
//! newtype wrappers. The shapes line up closely; the only real impedance
//! mismatches are documented inline (sync-vs-async datagrams, byte-count vs
//! write-all, string-vs-code stream resets).

use bytes::Bytes;
use url::Url;
use web_transport_trait as wtt;

/// A connected browser WebTransport session, usable by `moq-net`.
#[derive(Clone)]
pub struct Session(web_transport_wasm::Session);

/// Open a browser WebTransport connection to `url`.
pub async fn connect(url: Url) -> Result<Session, Error> {
	let client = web_transport_wasm::ClientBuilder::new().with_system_roots();
	let session = client.connect(url).await.map_err(Error)?;
	Ok(Session(session))
}

/// Connect, trusting only the given sha-256 certificate hashes (serverless dev,
/// matching the browser's `serverCertificateHashes` option).
pub async fn connect_with_hashes(url: Url, hashes: Vec<Vec<u8>>) -> Result<Session, Error> {
	let client = web_transport_wasm::ClientBuilder::new().with_server_certificate_hashes(hashes);
	let session = client.connect(url).await.map_err(Error)?;
	Ok(Session(session))
}

pub struct SendStream(web_transport_wasm::SendStream);
pub struct RecvStream(web_transport_wasm::RecvStream);

/// Wraps `web_transport_wasm::Error` so we can implement the foreign error trait.
#[derive(Debug, Clone, thiserror::Error)]
#[error(transparent)]
pub struct Error(web_transport_wasm::Error);

impl wtt::Error for Error {
	fn session_error(&self) -> Option<(u32, String)> {
		self.0.code().map(|code| (code as u32, self.0.to_string()))
	}

	fn stream_error(&self) -> Option<u32> {
		self.0.code().map(|c| c as u32)
	}
}

impl wtt::Session for Session {
	type SendStream = SendStream;
	type RecvStream = RecvStream;
	type Error = Error;

	async fn accept_uni(&self) -> Result<Self::RecvStream, Self::Error> {
		Ok(RecvStream(self.0.accept_uni().await.map_err(Error)?))
	}

	async fn accept_bi(&self) -> Result<(Self::SendStream, Self::RecvStream), Self::Error> {
		let (s, r) = self.0.accept_bi().await.map_err(Error)?;
		Ok((SendStream(s), RecvStream(r)))
	}

	async fn open_bi(&self) -> Result<(Self::SendStream, Self::RecvStream), Self::Error> {
		let (s, r) = self.0.open_bi().await.map_err(Error)?;
		Ok((SendStream(s), RecvStream(r)))
	}

	async fn open_uni(&self) -> Result<Self::SendStream, Self::Error> {
		Ok(SendStream(self.0.open_uni().await.map_err(Error)?))
	}

	fn send_datagram(&self, payload: Bytes) -> Result<(), Self::Error> {
		// The browser datagram API is async; the trait is sync. moq drives all
		// control/media over streams, so fire-and-forget is acceptable here.
		let session = self.0.clone();
		web_async::spawn(async move {
			let _ = session.send_datagram(payload).await;
		});
		Ok(())
	}

	async fn recv_datagram(&self) -> Result<Bytes, Self::Error> {
		self.0.recv_datagram().await.map_err(Error)
	}

	fn max_datagram_size(&self) -> usize {
		// The browser doesn't expose this; use the conservative QUIC default.
		1200
	}

	fn protocol(&self) -> Option<&str> {
		self.0.protocol()
	}

	fn close(&self, code: u32, reason: &str) {
		self.0.close(code, reason);
	}

	async fn closed(&self) -> Self::Error {
		Error(self.0.closed().await)
	}
}

impl wtt::SendStream for SendStream {
	type Error = Error;

	async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
		// The wasm writer writes the whole slice or errors.
		self.0.write(buf).await.map_err(Error)?;
		Ok(buf.len())
	}

	fn set_priority(&mut self, order: u8) {
		self.0.set_priority(order as i32);
	}

	fn finish(&mut self) -> Result<(), Self::Error> {
		self.0.finish().map_err(Error)
	}

	fn reset(&mut self, code: u32) {
		self.0.reset(&code.to_string());
	}

	async fn closed(&mut self) -> Result<(), Self::Error> {
		self.0.closed().await.map(|_| ()).map_err(Error)
	}
}

impl wtt::RecvStream for RecvStream {
	type Error = Error;

	async fn read(&mut self, dst: &mut [u8]) -> Result<Option<usize>, Self::Error> {
		match self.0.read(dst.len()).await.map_err(Error)? {
			Some(chunk) => {
				let n = chunk.len();
				dst[..n].copy_from_slice(&chunk);
				Ok(Some(n))
			}
			None => Ok(None),
		}
	}

	fn stop(&mut self, code: u32) {
		self.0.stop(&code.to_string());
	}

	async fn closed(&mut self) -> Result<(), Self::Error> {
		self.0.closed().await.map(|_| ()).map_err(Error)
	}
}
