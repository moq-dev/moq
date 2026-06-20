//! RTMP server: accept connections, and hand each pending publish to the caller
//! as a [`Request`] to authorize.
//!
//! [`Server::accept`] runs the RTMP handshake and the connect/publish command
//! exchange for each TCP connection (many concurrently, so a slow client doesn't
//! block others), then yields a [`Request`] once the client issues its `publish`
//! command. The caller inspects the app and stream key, makes an authorization
//! decision, and calls [`Request::accept`] (publish into an origin at a path) or
//! [`Request::reject`]. This mirrors `moq-native`'s `Server` / `Request`, so the
//! gateway stays unopinionated about auth: the embedder (e.g. a relay verifying
//! the stream key as a JWT) owns that policy.
//!
//! RTMPS (RTMP over TLS): [`Server::with_tls`] makes the listener terminate TLS
//! before the RTMP handshake, so `rtmps://` clients work with no other change.
//! If you'd rather own the transport (custom TLS, a non-TCP socket, a test
//! pipe), accept the connection and complete any handshake yourself, then hand
//! the established stream to [`accept_stream`]; everything here is generic over
//! the [`Stream`] trait.

use std::collections::VecDeque;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use futures::StreamExt;
use futures::future::BoxFuture;
use futures::stream::FuturesUnordered;
use moq_mux::container::flv::Import as FlvImport;
use moq_net::{BroadcastInfo, OriginProducer, OriginPublish};
use rml_rtmp::handshake::{Handshake, HandshakeProcessResult, PeerType};
use rml_rtmp::sessions::{ServerSession, ServerSessionConfig, ServerSessionEvent, ServerSessionResult};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::{TcpListener, TcpStream};

use crate::Result;
use crate::flv;

/// Read buffer size for pulling RTMP chunk-stream bytes off the socket.
const READ_BUFFER: usize = 16 * 1024;

/// How long a connection has to finish the handshake and issue its `publish`
/// before it is dropped. Bounds the lifetime (and socket / `pending` slot) of a
/// client that connects but never publishes, so idle or half-open connections
/// can't accumulate without limit. With TLS this also covers the TLS handshake.
const PUBLISH_TIMEOUT: Duration = Duration::from_secs(15);

/// A bidirectional byte stream carrying an RTMP session.
///
/// A plaintext [`tokio::net::TcpStream`] for `rtmp://`, or a TLS stream you've
/// accepted for `rtmps://`. Implemented for every
/// `AsyncRead + AsyncWrite + Unpin + Send`, so [`accept_stream`] and
/// [`Request`] work over whatever transport you bring.
pub trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}

/// A connection accepted by [`Server`]: plaintext RTMP, or RTMPS over TLS.
///
/// This is the stream type behind a [`Server`]-produced [`Request`] (hence
/// `Request<Conn>`). Bring-your-own-transport callers using [`accept_stream`]
/// keep their own stream type instead.
pub enum Conn {
	/// A plaintext TCP connection (`rtmp://`).
	Plain(TcpStream),

	/// A TLS connection (`rtmps://`), established by [`Server::with_tls`]. Boxed
	/// because a `TlsStream` is large relative to a bare `TcpStream`.
	#[cfg(feature = "server")]
	Tls(Box<tokio_rustls::server::TlsStream<TcpStream>>),
}

impl AsyncRead for Conn {
	fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
		match self.get_mut() {
			Conn::Plain(s) => Pin::new(s).poll_read(cx, buf),
			#[cfg(feature = "server")]
			Conn::Tls(s) => Pin::new(s).poll_read(cx, buf),
		}
	}
}

impl AsyncWrite for Conn {
	fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
		match self.get_mut() {
			Conn::Plain(s) => Pin::new(s).poll_write(cx, buf),
			#[cfg(feature = "server")]
			Conn::Tls(s) => Pin::new(s).poll_write(cx, buf),
		}
	}

	fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
		match self.get_mut() {
			Conn::Plain(s) => Pin::new(s).poll_flush(cx),
			#[cfg(feature = "server")]
			Conn::Tls(s) => Pin::new(s).poll_flush(cx),
		}
	}

	fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
		match self.get_mut() {
			Conn::Plain(s) => Pin::new(s).poll_shutdown(cx),
			#[cfg(feature = "server")]
			Conn::Tls(s) => Pin::new(s).poll_shutdown(cx),
		}
	}
}

/// An RTMP server that yields each connection's pending publish as a [`Request`].
///
/// Build it with [`bind`](Self::bind), optionally enable RTMPS with
/// [`with_tls`](Self::with_tls), then loop on [`accept`](Self::accept). The
/// handshake and the connect/publish exchange happen inside `accept`, so a
/// [`Request`] is only produced once a client actually wants to publish.
pub struct Server {
	listener: TcpListener,

	/// When set, each accepted connection is TLS-terminated (RTMPS) before the
	/// RTMP handshake.
	#[cfg(feature = "server")]
	tls: Option<tokio_rustls::TlsAcceptor>,

	/// In-flight handshakes; each resolves to a ready [`Request`], or `None` if
	/// the connection closed or errored before issuing a publish.
	pending: FuturesUnordered<BoxFuture<'static, Option<Request<Conn>>>>,
}

impl Server {
	/// Bind an RTMP listener on `addr` (RTMP's well-known port is 1935).
	pub async fn bind(addr: SocketAddr) -> Result<Self> {
		let listener = TcpListener::bind(addr).await?;
		Ok(Self {
			listener,
			#[cfg(feature = "server")]
			tls: None,
			pending: FuturesUnordered::new(),
		})
	}

	/// Terminate TLS on every accepted connection, turning this into an RTMPS
	/// listener (`rtmps://`). Pass a `rustls::ServerConfig` (e.g. from
	/// [`moq_native::tls::Server::server_config`] with an empty ALPN list), or
	/// `None` to leave it plaintext.
	#[cfg(feature = "server")]
	pub fn with_tls(mut self, tls: impl Into<Option<std::sync::Arc<rustls::ServerConfig>>>) -> Self {
		self.tls = tls.into().map(tokio_rustls::TlsAcceptor::from);
		self
	}

	/// The local address the listener is bound to.
	pub fn local_addr(&self) -> Result<SocketAddr> {
		Ok(self.listener.local_addr()?)
	}

	/// Wait for the next connection that wants to publish.
	///
	/// New connections are accepted and handshaked concurrently; this returns the
	/// next one to reach its `publish` command. Connections that close or error
	/// before publishing are dropped without surfacing here. Returns `None` only
	/// if the listener itself stops (it currently never does).
	pub async fn accept(&mut self) -> Option<Request<Conn>> {
		loop {
			tokio::select! {
				// A handshake finished: yield its request, or skip a dead connection.
				Some(maybe) = self.pending.next(), if !self.pending.is_empty() => {
					if let Some(request) = maybe {
						return Some(request);
					}
				}
				// A new TCP connection: start its (TLS +) handshake concurrently.
				res = self.listener.accept() => match res {
					Ok((stream, peer)) => {
						// Nagle off: RTMP is latency-sensitive and we write whole packets.
						if let Err(err) = stream.set_nodelay(true) {
							tracing::debug!(%peer, %err, "failed to set TCP_NODELAY");
						}
						#[cfg(feature = "server")]
						let tls = self.tls.clone();
						self.pending.push(Box::pin(async move {
							// The TLS handshake (if any) and the RTMP handshake share one
							// budget, so a client that stalls either is dropped.
							let outcome = tokio::time::timeout(PUBLISH_TIMEOUT, async move {
								#[cfg(feature = "server")]
								let conn = match tls {
									Some(acceptor) => Conn::Tls(Box::new(
										acceptor
											.accept(stream)
											.await
											.map_err(|e| anyhow::anyhow!("rtmps tls handshake: {e}"))?,
									)),
									None => Conn::Plain(stream),
								};
								#[cfg(not(feature = "server"))]
								let conn = Conn::Plain(stream);
								accept_until_publish(conn, peer).await
							})
							.await;
							match outcome {
								Ok(Ok(request)) => request,
								Ok(Err(err)) => {
									tracing::warn!(%peer, %err, "RTMP connection closed before publish");
									None
								}
								Err(_) => {
									tracing::warn!(%peer, "RTMP connection did not publish before timeout");
									None
								}
							}
						}));
					}
					Err(err) => {
						// A failed accept must not take the listener down; back off so a
						// persistent error doesn't busy-spin.
						tracing::warn!(%err, "failed to accept RTMP connection; continuing");
						tokio::time::sleep(Duration::from_millis(100)).await;
					}
				},
			}
		}
	}
}

/// Run the RTMP handshake and connect/publish exchange on an already-established
/// byte stream, yielding the pending publish as a [`Request`].
///
/// The bring-your-own-transport entry point: accept the connection (and, for
/// `rtmps://`, complete the TLS handshake) yourself, then hand the stream here.
/// `peer` is the remote address, used for logging and [`Request::peer`].
///
/// Returns `Ok(None)` if the client disconnects before issuing `publish`. Unlike
/// [`Server`], this applies no publish timeout: wrap the call in
/// [`tokio::time::timeout`] to bound how long a connected-but-idle client can
/// hold the task.
pub async fn accept_stream<S: Stream>(stream: S, peer: SocketAddr) -> Result<Option<Request<S>>> {
	Ok(accept_until_publish(stream, peer).await?)
}

/// A pending RTMP publish, waiting on the caller to authorize it.
///
/// Inspect [`app`](Self::app) and [`stream_key`](Self::stream_key) (an
/// `rtmp://host/<app>/<key>` URL splits into these), then either
/// [`accept`](Self::accept) the publish into an origin at a chosen broadcast
/// path or [`reject`](Self::reject) it. Dropping a `Request` without either
/// closes the connection.
///
/// `S` is the underlying stream: [`Conn`] for a [`Server`]-produced request, or
/// your own transport when built via [`accept_stream`].
pub struct Request<S = Conn> {
	stream: S,
	session: ServerSession,
	/// The `rml_rtmp` request id for the pending publish, replied to on accept/reject.
	request_id: u32,
	/// Session results produced alongside the publish command, processed once the
	/// publish is accepted.
	work: VecDeque<ServerSessionResult>,
	app: String,
	stream_key: String,
	peer: SocketAddr,
}

impl<S: Stream> Request<S> {
	/// The RTMP app name (the path component of `rtmp://host/<app>/<key>`).
	pub fn app(&self) -> &str {
		&self.app
	}

	/// The RTMP stream key (the final component of `rtmp://host/<app>/<key>`).
	///
	/// Conventionally a publish secret; an embedder can treat it as a token (e.g.
	/// a moq-token JWT) to authenticate the publish.
	pub fn stream_key(&self) -> &str {
		&self.stream_key
	}

	/// The remote peer address.
	pub fn peer(&self) -> SocketAddr {
		self.peer
	}

	/// Accept the publish: announce a broadcast at `path` in `origin` and pump the
	/// RTMP media into it until the client disconnects.
	///
	/// `origin` is whatever the caller wants the media published into (e.g. a
	/// relay's shared origin, optionally re-rooted/scoped per the authenticated
	/// token). This future resolves when the connection ends, so callers usually
	/// run it on its own task.
	pub async fn accept(mut self, origin: &OriginProducer, path: &str) -> Result<()> {
		let results = self
			.session
			.accept_request(self.request_id)
			.map_err(|e| anyhow::anyhow!("rtmp accept publish: {e:?}"))?;
		self.work.extend(results);

		let mut publisher = Publisher::new(origin, path)?;
		tracing::info!(peer = %self.peer, %path, "rtmp publish accepted");

		let result = pump(
			&mut self.stream,
			&mut self.session,
			&mut self.work,
			&mut publisher,
			self.peer,
		)
		.await;

		// Flush the importer so the final groups close cleanly before unannouncing.
		if let Err(err) = publisher.finish() {
			tracing::debug!(peer = %self.peer, %err, "error finishing RTMP publish");
		}

		Ok(result?)
	}

	/// Reject the publish, sending `reason` back to the client as the
	/// `NetStream.Publish.Denied` description, then close the connection.
	pub async fn reject(mut self, reason: &str) -> Result<()> {
		let results = self
			.session
			.reject_request(self.request_id, "NetStream.Publish.Denied", reason)
			.map_err(|e| anyhow::anyhow!("rtmp reject publish: {e:?}"))?;

		// Flush any pending writes plus the rejection so it reaches the client.
		for result in self.work.drain(..).chain(results) {
			if let ServerSessionResult::OutboundResponse(packet) = result {
				self.stream.write_all(&packet.bytes).await?;
			}
		}
		tracing::debug!(peer = %self.peer, %reason, "rtmp publish rejected");
		Ok(())
	}
}

/// Run one connection's handshake and connect/publish exchange, returning a
/// [`Request`] once the client issues `publish` (or `None` if it disconnects first).
async fn accept_until_publish<S: Stream>(mut stream: S, peer: SocketAddr) -> anyhow::Result<Option<Request<S>>> {
	let remaining = run_handshake(&mut stream, peer).await?;

	let (mut session, initial) =
		ServerSession::new(ServerSessionConfig::new()).map_err(|e| anyhow::anyhow!("rtmp session init: {e:?}"))?;
	let mut work: VecDeque<ServerSessionResult> = VecDeque::from(initial);

	// Any RTMP bytes bundled with the final handshake packet.
	if !remaining.is_empty() {
		let results = session
			.handle_input(&remaining)
			.map_err(|e| anyhow::anyhow!("rtmp handle_input: {e:?}"))?;
		work.extend(results);
	}

	let mut buffer = [0u8; READ_BUFFER];
	loop {
		while let Some(result) = work.pop_front() {
			match result {
				ServerSessionResult::OutboundResponse(packet) => {
					stream.write_all(&packet.bytes).await?;
				}
				ServerSessionResult::RaisedEvent(event) => match event {
					// Accept every connect; authorization happens at publish time.
					ServerSessionEvent::ConnectionRequested { request_id, app_name } => {
						tracing::debug!(%peer, %app_name, "rtmp connect");
						let results = session
							.accept_request(request_id)
							.map_err(|e| anyhow::anyhow!("rtmp accept connect: {e:?}"))?;
						work.extend(results);
					}
					// The client wants to publish: hand control back to the caller.
					ServerSessionEvent::PublishStreamRequested {
						request_id,
						app_name,
						stream_key,
						..
					} => {
						return Ok(Some(Request {
							stream,
							session,
							request_id,
							work,
							app: app_name,
							stream_key,
							peer,
						}));
					}
					other => tracing::trace!(%peer, ?other, "ignoring RTMP event before publish"),
				},
				ServerSessionResult::UnhandleableMessageReceived(_) => {
					tracing::trace!(%peer, "ignoring unhandleable RTMP message");
				}
			}
		}

		let n = stream.read(&mut buffer).await?;
		if n == 0 {
			return Ok(None);
		}
		let results = session
			.handle_input(&buffer[..n])
			.map_err(|e| anyhow::anyhow!("rtmp handle_input: {e:?}"))?;
		work.extend(results);
	}
}

/// Pump RTMP media into the publisher until the client disconnects or finishes.
async fn pump<S: Stream>(
	stream: &mut S,
	session: &mut ServerSession,
	work: &mut VecDeque<ServerSessionResult>,
	publisher: &mut Publisher,
	peer: SocketAddr,
) -> anyhow::Result<()> {
	let mut buffer = [0u8; READ_BUFFER];
	loop {
		let mut finished = false;
		while let Some(result) = work.pop_front() {
			match result {
				ServerSessionResult::OutboundResponse(packet) => {
					stream.write_all(&packet.bytes).await?;
				}
				ServerSessionResult::RaisedEvent(event) => match event {
					// A frame that fails to demux is dropped, not fatal: the importer
					// consumes whole tags atomically, so one bad frame doesn't desync
					// the stream, and tearing down a live publish over it would be worse.
					ServerSessionEvent::AudioDataReceived { data, timestamp, .. } => {
						if let Err(err) = publisher.push(flv::TAG_AUDIO, timestamp.value, &data) {
							tracing::warn!(%peer, %err, "dropping RTMP audio frame that failed to demux");
						}
					}
					ServerSessionEvent::VideoDataReceived { data, timestamp, .. } => {
						if let Err(err) = publisher.push(flv::TAG_VIDEO, timestamp.value, &data) {
							tracing::warn!(%peer, %err, "dropping RTMP video frame that failed to demux");
						}
					}
					ServerSessionEvent::PublishStreamFinished { .. } => finished = true,
					// onMetaData and other script data: the FLV importer reads codec
					// config from the sequence headers, so metadata isn't forwarded.
					ServerSessionEvent::StreamMetadataChanged { .. } => {}
					other => tracing::trace!(%peer, ?other, "ignoring RTMP event"),
				},
				ServerSessionResult::UnhandleableMessageReceived(_) => {
					tracing::trace!(%peer, "ignoring unhandleable RTMP message");
				}
			}
		}
		if finished {
			break;
		}

		let n = stream.read(&mut buffer).await?;
		if n == 0 {
			break;
		}
		let results = session
			.handle_input(&buffer[..n])
			.map_err(|e| anyhow::anyhow!("rtmp handle_input: {e:?}"))?;
		work.extend(results);
	}

	tracing::debug!(%peer, "rtmp connection closed");
	Ok(())
}

/// Perform the RTMP server handshake, returning any leftover bytes that followed
/// the client's final handshake packet (the start of the chunk stream).
async fn run_handshake<S: Stream>(stream: &mut S, peer: SocketAddr) -> anyhow::Result<Vec<u8>> {
	let mut handshake = Handshake::new(PeerType::Server);
	let p0_p1 = handshake
		.generate_outbound_p0_and_p1()
		.map_err(|e| anyhow::anyhow!("rtmp handshake p0/p1: {e:?}"))?;
	stream.write_all(&p0_p1).await?;

	let mut buffer = [0u8; 4096];
	loop {
		let n = stream.read(&mut buffer).await?;
		if n == 0 {
			anyhow::bail!("peer {peer} closed during handshake");
		}

		match handshake
			.process_bytes(&buffer[..n])
			.map_err(|e| anyhow::anyhow!("rtmp handshake: {e:?}"))?
		{
			HandshakeProcessResult::InProgress { response_bytes } => {
				if !response_bytes.is_empty() {
					stream.write_all(&response_bytes).await?;
				}
			}
			HandshakeProcessResult::Completed {
				response_bytes,
				remaining_bytes,
			} => {
				if !response_bytes.is_empty() {
					stream.write_all(&response_bytes).await?;
				}
				tracing::debug!(%peer, "rtmp handshake complete");
				return Ok(remaining_bytes);
			}
		}
	}
}

/// An active publish: the moq-mux FLV importer (which owns the
/// [`BroadcastProducer`](moq_net::BroadcastProducer) it publishes into) plus the
/// origin announcement. Dropping it closes and unannounces the broadcast.
struct Publisher {
	/// Held to keep the broadcast announced for the publisher's lifetime.
	_publish: OriginPublish,
	importer: FlvImport,
}

impl Publisher {
	/// Open a broadcast at `path` and prime the importer with the FLV file
	/// header, so subsequent tags decode against an initialized demuxer.
	fn new(origin: &OriginProducer, path: &str) -> anyhow::Result<Self> {
		let mut broadcast = BroadcastInfo::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast)?;
		let mut importer = FlvImport::new(broadcast.clone(), catalog);

		let publish = origin.publish_broadcast(path, broadcast.consume())?;

		// Feed the FLV file header once up front; media tags follow per message.
		importer.decode(&flv::file_header())?;

		Ok(Self {
			_publish: publish,
			importer,
		})
	}

	/// Re-wrap one RTMP audio/video message body as an FLV tag and demux it.
	fn push(&mut self, tag_type: u8, timestamp: u32, body: &[u8]) -> anyhow::Result<()> {
		// FLV's tag DataSize is 24-bit. A larger body would truncate, declaring a
		// wrong size that desyncs the demuxer on the next tag. Drop it instead.
		anyhow::ensure!(
			body.len() <= 0xFF_FFFF,
			"RTMP message body {} exceeds FLV's 24-bit tag size limit",
			body.len()
		);
		self.importer.decode(&flv::tag(tag_type, timestamp, body))
	}

	/// Flush any buffered media and close out the broadcast's open groups.
	fn finish(&mut self) -> anyhow::Result<()> {
		self.importer.finish()
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use rml_rtmp::sessions::{
		ClientSession, ClientSessionConfig, ClientSessionEvent, ClientSessionResult, PublishRequestType,
	};

	/// Drive a real RTMP client over an already-connected `stream` through
	/// handshake -> connect(`live`) -> publish(`cam0`), pumping until aborted by
	/// the test. Generic over the transport so the same client exercises both
	/// plaintext RTMP and RTMPS.
	async fn run_client<S: Stream>(mut stream: S) {
		// Handshake.
		let mut handshake = Handshake::new(PeerType::Client);
		stream
			.write_all(&handshake.generate_outbound_p0_and_p1().unwrap())
			.await
			.unwrap();
		let mut buffer = [0u8; 4096];
		let remaining = loop {
			let n = stream.read(&mut buffer).await.unwrap();
			match handshake.process_bytes(&buffer[..n]).unwrap() {
				HandshakeProcessResult::InProgress { response_bytes } => {
					if !response_bytes.is_empty() {
						stream.write_all(&response_bytes).await.unwrap();
					}
				}
				HandshakeProcessResult::Completed {
					response_bytes,
					remaining_bytes,
				} => {
					if !response_bytes.is_empty() {
						stream.write_all(&response_bytes).await.unwrap();
					}
					break remaining_bytes;
				}
			}
		};

		let (mut session, initial) = ClientSession::new(ClientSessionConfig::new()).unwrap();
		let mut work: VecDeque<ClientSessionResult> = VecDeque::from(initial);
		if !remaining.is_empty() {
			work.extend(session.handle_input(&remaining).unwrap());
		}
		work.push_back(session.request_connection("live".to_string()).unwrap());

		loop {
			while let Some(result) = work.pop_front() {
				match result {
					ClientSessionResult::OutboundResponse(packet) => {
						stream.write_all(&packet.bytes).await.unwrap();
					}
					// Once connected, ask to publish; the publish command is sent
					// automatically as the createStream round trip completes.
					ClientSessionResult::RaisedEvent(ClientSessionEvent::ConnectionRequestAccepted) => {
						let result = session
							.request_publishing("cam0".to_string(), PublishRequestType::Live)
							.unwrap();
						work.push_back(result);
					}
					_ => {}
				}
			}
			let n = match stream.read(&mut buffer).await {
				Ok(n) => n,
				Err(_) => return,
			};
			if n == 0 {
				return;
			}
			match session.handle_input(&buffer[..n]) {
				Ok(results) => work.extend(results),
				Err(_) => return,
			}
		}
	}

	#[tokio::test]
	async fn accept_yields_publish_request() {
		let mut server = Server::bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
		let addr = server.local_addr().unwrap();

		let client = tokio::spawn(async move {
			let stream = TcpStream::connect(addr).await.unwrap();
			run_client(stream).await;
		});

		let request = tokio::time::timeout(Duration::from_secs(5), server.accept())
			.await
			.expect("server.accept timed out")
			.expect("server yielded a request");

		assert_eq!(request.app(), "live");
		assert_eq!(request.stream_key(), "cam0");

		request.reject("test rejection").await.unwrap();
		client.abort();
	}

	/// The same publish flow, but over TLS: prove [`Server::with_tls`] terminates
	/// RTMPS and yields an identical [`Request`]. Gated on `quinn` because it
	/// borrows moq-native's cert generation (`server_config`), which needs a
	/// moq-native backend feature.
	#[cfg(feature = "quinn")]
	#[tokio::test]
	async fn rtmps_accept_yields_publish_request() {
		use std::sync::Arc;

		use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
		use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
		use rustls::{DigitallySignedStruct, SignatureScheme};

		// Accept any server cert: the test uses a throwaway self-signed cert.
		#[derive(Debug)]
		struct NoVerify(Arc<rustls::crypto::CryptoProvider>);

		impl ServerCertVerifier for NoVerify {
			fn verify_server_cert(
				&self,
				_end_entity: &CertificateDer<'_>,
				_intermediates: &[CertificateDer<'_>],
				_server_name: &ServerName<'_>,
				_ocsp: &[u8],
				_now: UnixTime,
			) -> std::result::Result<ServerCertVerified, rustls::Error> {
				Ok(ServerCertVerified::assertion())
			}

			fn verify_tls12_signature(
				&self,
				message: &[u8],
				cert: &CertificateDer<'_>,
				dss: &DigitallySignedStruct,
			) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
				rustls::crypto::verify_tls12_signature(message, cert, dss, &self.0.signature_verification_algorithms)
			}

			fn verify_tls13_signature(
				&self,
				message: &[u8],
				cert: &CertificateDer<'_>,
				dss: &DigitallySignedStruct,
			) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
				rustls::crypto::verify_tls13_signature(message, cert, dss, &self.0.signature_verification_algorithms)
			}

			fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
				self.0.signature_verification_algorithms.supported_schemes()
			}
		}

		let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());

		// Server: a self-signed cert for `localhost`, fronting the RTMP listener.
		let mut tls = moq_native::tls::Server::default();
		tls.generate = vec!["localhost".to_string()];
		let server_config = tls.server_config(vec![]).expect("build RTMPS server config");

		let mut server = Server::bind("127.0.0.1:0".parse().unwrap())
			.await
			.unwrap()
			.with_tls(server_config);
		let addr = server.local_addr().unwrap();

		// Client: TLS-connect (no verify), then run the ordinary RTMP client.
		let client = tokio::spawn(async move {
			let client_config = rustls::ClientConfig::builder_with_provider(provider.clone())
				.with_safe_default_protocol_versions()
				.unwrap()
				.dangerous()
				.with_custom_certificate_verifier(Arc::new(NoVerify(provider)))
				.with_no_client_auth();
			let connector = tokio_rustls::TlsConnector::from(Arc::new(client_config));
			let tcp = TcpStream::connect(addr).await.unwrap();
			let server_name = ServerName::try_from("localhost").unwrap();
			let stream = connector.connect(server_name, tcp).await.unwrap();
			run_client(stream).await;
		});

		let request = tokio::time::timeout(Duration::from_secs(5), server.accept())
			.await
			.expect("server.accept timed out")
			.expect("server yielded a request");

		assert_eq!(request.app(), "live");
		assert_eq!(request.stream_key(), "cam0");

		request.reject("test rejection").await.unwrap();
		client.abort();
	}
}
