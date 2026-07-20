use crate::origin;
use crate::{
	ALPN_14, ALPN_15, ALPN_16, ALPN_17, ALPN_18, ALPN_19, ALPN_LITE, ALPN_LITE_03, ALPN_LITE_04, ALPN_LITE_05,
	ALPN_LITE_06_WIP, Consume, Driver, Error, NEGOTIATED, Role, Session, Version, Versions,
	coding::{Decode, Encode, Reader, Stream},
	ietf, lite, setup, stats,
};

/// A MoQ server session builder.
#[derive(Default, Clone)]
pub struct Server {
	publish: Option<origin::Consumer>,
	subscribe: Option<origin::Producer>,
	stats: stats::Handle,
	versions: Versions,
}

impl Server {
	/// A server that neither publishes nor subscribes until configured.
	pub fn new() -> Self {
		Default::default()
	}

	/// Publish to the connected client: the session reads from the given origin
	/// (pass an [`origin::Producer`] or [`origin::Consumer`] by reference) and forwards
	/// its announcements. Omit to publish nothing. Pre-scoped via
	/// [`origin::Producer::scope`] for token-gated relays.
	pub fn with_publisher(mut self, publish: impl Consume<origin::Consumer>) -> Self {
		self.publish = Some(publish.consume());
		self
	}

	/// Subscribe to the connected client: the session writes the broadcasts the
	/// client announces into this [`origin::Producer`]. Omit to subscribe to nothing.
	pub fn with_subscriber(mut self, subscribe: origin::Producer) -> Self {
		self.subscribe = Some(subscribe);
		self
	}

	/// Attach a tier-scoped [`stats::Handle`]. Per-broadcast and per-subscription
	/// counters will be bumped through this handle for the lifetime of the session.
	/// Pass [`stats::Handle::default`] (a no-op handle) to opt out.
	pub fn with_stats(mut self, stats: stats::Handle) -> Self {
		self.stats = stats;
		self
	}

	/// Set both publish and subscribe from one shared [`origin::Producer`].
	pub fn with_origin(self, origin: origin::Producer) -> Self {
		self.with_publisher(&origin).with_subscriber(origin)
	}

	/// Restrict which protocol versions to accept, in preference order.
	/// Defaults to every version this crate supports.
	pub fn with_versions(mut self, versions: Versions) -> Self {
		self.versions = versions;
		self
	}

	/// Perform the MoQ handshake as a server, returning the [`Session`] and the
	/// [`Driver`] that runs its protocol work.
	///
	/// Convenience wrapper over [`accept_request`](Self::accept_request) that
	/// completes the handshake immediately. Use `accept_request` when you need to
	/// inspect the client's advertised path before deciding what to serve.
	pub async fn accept<S: web_transport_trait::Session>(&self, session: S) -> Result<(Session, Driver), Error> {
		self.accept_request(session).await?.ok().await
	}

	/// Begin the MoQ handshake, pausing once the client's request path is known so
	/// the caller can authorize/scope before serving.
	///
	/// Reads the client's SETUP (the in-band path lives there on URL-less transports),
	/// then returns a [`Request`]: inspect [`path`](Request::path), set the origins to
	/// serve, and call [`ok`](Request::ok) or [`close`](Request::close). Session start
	/// is deferred to `ok()`, so origins set on the `Request` always take effect.
	///
	/// The path is surfaced for moq-lite-05 and every moq-transport draft we speak;
	/// it's empty on versions with no in-band request path (e.g. lite 01-04).
	pub async fn accept_request<S: web_transport_trait::Session>(&self, session: S) -> Result<Request<S>, Error> {
		// Regimes without a path to read defer to `ok()` without surfacing one, and
		// carry no role hint, so authorization is unchanged for them.
		let deferred = |handshake| Request {
			path: None,
			role: None,
			inner: Some(RequestInner {
				server: self.clone(),
				handshake,
			}),
		};

		let (encoding, supported) = match session.protocol() {
			Some(ALPN_19) => {
				self.versions
					.select(Version::Ietf(ietf::Version::Draft19))
					.ok_or(Error::Version)?;
				return self.accept_ietf_modern(session, ietf::Version::Draft19).await;
			}
			Some(ALPN_18) => {
				self.versions
					.select(Version::Ietf(ietf::Version::Draft18))
					.ok_or(Error::Version)?;
				return self.accept_ietf_modern(session, ietf::Version::Draft18).await;
			}
			Some(ALPN_17) => {
				self.versions
					.select(Version::Ietf(ietf::Version::Draft17))
					.ok_or(Error::Version)?;
				return self.accept_ietf_modern(session, ietf::Version::Draft17).await;
			}
			Some(ALPN_16) => {
				let v = self
					.versions
					.select(Version::Ietf(ietf::Version::Draft16))
					.ok_or(Error::Version)?;
				(v, v.into())
			}
			Some(ALPN_15) => {
				let v = self
					.versions
					.select(Version::Ietf(ietf::Version::Draft15))
					.ok_or(Error::Version)?;
				(v, v.into())
			}
			Some(ALPN_14) => {
				let v = self
					.versions
					.select(Version::Ietf(ietf::Version::Draft14))
					.ok_or(Error::Version)?;
				(v, v.into())
			}
			Some(alpn @ (ALPN_LITE_05 | ALPN_LITE_06_WIP)) => {
				let version = match alpn {
					ALPN_LITE_06_WIP => lite::Version::Lite06Wip,
					_ => lite::Version::Lite05,
				};
				self.versions.select(Version::Lite(version)).ok_or(Error::Version)?;

				// Gate on the client's SETUP: read it before serving so the caller can
				// scope by the advertised path. Seeded back into `start` on `ok()` so
				// PROBE gating resolves without re-reading the (consumed) Setup Stream.
				let client_setup = lite::accept_setup(&session, version).await?;
				return Ok(Request {
					path: client_setup.path.clone(),
					role: client_setup.role,
					inner: Some(RequestInner {
						server: self.clone(),
						handshake: Handshake::LiteSetup {
							session,
							version,
							client_setup,
						},
					}),
				});
			}
			Some(ALPN_LITE_04) => {
				self.versions
					.select(Version::Lite(lite::Version::Lite04))
					.ok_or(Error::Version)?;
				return Ok(deferred(Handshake::LiteBare {
					session,
					version: lite::Version::Lite04,
				}));
			}
			Some(ALPN_LITE_03) => {
				self.versions
					.select(Version::Lite(lite::Version::Lite03))
					.ok_or(Error::Version)?;
				return Ok(deferred(Handshake::LiteBare {
					session,
					version: lite::Version::Lite03,
				}));
			}
			Some(ALPN_LITE) | None => {
				let supported = self.versions.filter(&NEGOTIATED.into()).ok_or(Error::Version)?;
				(Version::Ietf(ietf::Version::Draft14), supported)
			}
			Some(p) => return Err(Error::UnknownAlpn(p.to_string())),
		};

		// Legacy bidi SETUP exchange (IETF 14-16, lite 01/02). Read the client's
		// SETUP to choose the version; `ok()` sends the server SETUP and starts.
		let mut stream = Stream::accept(&session, encoding).await?;
		let mut client: setup::Client = stream.reader.decode().await?;

		let version = client
			.versions
			.iter()
			.flat_map(|v| Version::try_from(*v).ok())
			.find(|v| supported.contains(v))
			.ok_or(Error::Version)?;

		// Pull the request path and max request ID out now (IETF only) so `ok()`
		// doesn't re-decode the consumed parameters. moq-transport carries the path
		// in its SETUP just like lite-05.
		let (path, request_id_max) = match version {
			Version::Ietf(v) => {
				let params = ietf::Parameters::decode(&mut client.parameters, v)?;
				let path = match params.get_bytes(ietf::ParameterBytes::Path) {
					Some(bytes) => Some(
						std::str::from_utf8(bytes)
							.map_err(|_| Error::Decode(crate::DecodeError::InvalidValue))?
							.to_owned(),
					),
					None => None,
				};
				let request_id_max = params
					.get_varint(ietf::ParameterVarInt::MaxRequestId)
					.map(ietf::RequestId);
				(path, request_id_max)
			}
			Version::Lite(_) => (None, None),
		};

		Ok(Request {
			path,
			role: None,
			inner: Some(RequestInner {
				server: self.clone(),
				handshake: Handshake::Legacy {
					session,
					stream,
					version,
					request_id_max,
				},
			}),
		})
	}

	/// Read a draft-17/18 client's SETUP (with its request path) off its uni stream,
	/// then pause. `ok()` starts the session and hands the stream back for GOAWAY.
	async fn accept_ietf_modern<S: web_transport_trait::Session>(
		&self,
		session: S,
		version: ietf::Version,
	) -> Result<Request<S>, Error> {
		let (peer_setup, path) = ietf::accept_setup(&session, version).await?;
		Ok(Request {
			path,
			role: None,
			inner: Some(RequestInner {
				server: self.clone(),
				handshake: Handshake::IetfModern {
					session,
					version,
					peer_setup,
				},
			}),
		})
	}
}

/// A paused server-side handshake.
///
/// Returned by [`Server::accept_request`] once the peer's advertised
/// [`path`](Self::path) is known but before the session is granted anything. Set
/// the origins to serve, then call [`ok`](Self::ok) to complete the handshake, or
/// [`close`](Self::close) to reject it. Modeled on the WebTransport `Request` in
/// moq-native.
pub struct Request<S: web_transport_trait::Session> {
	path: Option<String>,
	role: Option<Role>,
	// Taken by `ok`/`close`; `Drop` rejects the handshake if neither ran.
	inner: Option<RequestInner<S>>,
}

/// The parts of a [`Request`] consumed by [`Request::ok`] / [`Request::close`].
struct RequestInner<S: web_transport_trait::Session> {
	server: Server,
	handshake: Handshake<S>,
}

/// The handshake state captured at the pause point. Every variant defers its
/// session start to [`Request::ok`] so origins set on the Request still apply.
enum Handshake<S: web_transport_trait::Session> {
	/// Modern IETF (17/18): the client's SETUP (with its request path) has been read
	/// off its uni stream; `ok()` starts the session, handing that stream back for
	/// GOAWAY monitoring.
	IetfModern {
		session: S,
		version: ietf::Version,
		peer_setup: Reader<S::RecvStream, Version>,
	},
	/// moq-lite 03/04: no Setup Stream.
	LiteBare { session: S, version: lite::Version },
	/// Legacy IETF (draft 14-16) and lite 01/02: the client SETUP has been read off
	/// the bidi stream (including its request path) but the server SETUP hasn't been
	/// sent. `ok()` finishes it.
	Legacy {
		session: S,
		stream: Stream<S, Version>,
		version: Version,
		request_id_max: Option<ietf::RequestId>,
	},
	/// moq-lite 05+: the client's Setup Stream has been read. `ok()` starts the
	/// session, seeding the SETUP back so PROBE gating resolves.
	LiteSetup {
		session: S,
		version: lite::Version,
		client_setup: lite::Setup,
	},
}

impl<S: web_transport_trait::Session> Request<S> {
	/// The request path the client advertised in its SETUP.
	///
	/// Empty when the client advertised none: either it sent an empty path, or the
	/// version carries none in-band (lite 01-04). Those mean the same thing, so the
	/// wire distinction isn't surfaced. Populated for moq-lite-05 and every
	/// moq-transport draft we speak. See the note on [`Server::accept_request`].
	pub fn path(&self) -> &str {
		self.path.as_deref().unwrap_or("")
	}

	/// The single [`Role`] the client advertised in its SETUP, or `None` for a
	/// bidirectional session.
	///
	/// Only moq-lite-05 carries a role, so `None` covers three cases that the wire
	/// doesn't distinguish: an older version, a client that omitted the parameter, and a
	/// client that explicitly advertised both directions. All three mean the same thing
	/// (the client may publish and subscribe), so authorize on what the token grants.
	/// See the note on [`Server::accept_request`].
	pub fn role(&self) -> Option<Role> {
		self.role
	}

	/// Publish to the connected client. Overrides any value from the [`Server`]
	/// builder; typically set after inspecting [`path`](Self::path).
	pub fn with_publisher(mut self, publish: impl Consume<origin::Consumer>) -> Self {
		self.inner_mut().server.publish = Some(publish.consume());
		self
	}

	/// Subscribe to the connected client. Overrides any value from the [`Server`] builder.
	pub fn with_subscriber(mut self, subscribe: origin::Producer) -> Self {
		self.inner_mut().server.subscribe = Some(subscribe);
		self
	}

	/// Set the tier-scoped stats handle. Overrides any value from the [`Server`] builder.
	pub fn with_stats(mut self, stats: stats::Handle) -> Self {
		self.inner_mut().server.stats = stats;
		self
	}

	fn inner_mut(&mut self) -> &mut RequestInner<S> {
		self.inner.as_mut().expect("request already responded")
	}

	/// Accept the session, returning the [`Session`] and the [`Driver`] that runs
	/// its protocol work.
	pub async fn ok(mut self) -> Result<(Session, Driver), Error> {
		let RequestInner { server, handshake } = self.inner.take().expect("request already responded");

		let (session, mut stream, version, request_id_max) = match handshake {
			Handshake::IetfModern {
				session,
				version,
				peer_setup,
			} => {
				// The client's SETUP was read in `accept_request`; hand the stream back
				// for GOAWAY. A server never advertises a path, hence `None`.
				let protocol = ietf::start(
					session.clone(),
					None,
					None,
					false,
					server.publish,
					server.subscribe,
					server.stats,
					version,
					None,
					Some(peer_setup),
				)?;
				tracing::debug!(?version, "connected");
				return Ok(Session::new(session, version.into(), None, protocol));
			}
			Handshake::LiteBare { session, version } => {
				let start = lite::start(
					session.clone(),
					None,
					server.publish,
					server.subscribe,
					server.stats,
					version,
					lite::Setup::default(),
					None,
				)?;
				return Ok(Session::new(
					session,
					version.into(),
					start.recv_bandwidth,
					start.driver,
				));
			}
			Handshake::LiteSetup {
				session,
				version,
				client_setup,
			} => {
				// We report send bitrate; a server never advertises a request Path or Role.
				let our_setup = lite::Setup {
					probe: lite::ProbeLevel::Report,
					path: None,
					role: None,
				};
				let start = lite::start(
					session.clone(),
					None,
					server.publish,
					server.subscribe,
					server.stats,
					version,
					our_setup,
					Some(client_setup),
				)?;
				return Ok(Session::new(
					session,
					version.into(),
					start.recv_bandwidth,
					start.driver,
				));
			}
			Handshake::Legacy {
				session,
				stream,
				version,
				request_id_max,
			} => (session, stream, version, request_id_max),
		};

		// Encode parameters using the version-appropriate type.
		let parameters = match version {
			Version::Ietf(v) => {
				let mut parameters = ietf::Parameters::default();
				parameters.set_varint(ietf::ParameterVarInt::MaxRequestId, u32::MAX as u64);
				parameters.set_bytes(ietf::ParameterBytes::Implementation, b"moq-lite-rs".to_vec());
				parameters.encode_bytes(v)?
			}
			Version::Lite(v) => lite::Parameters::default().encode_bytes(v)?,
		};

		let server_setup = setup::Server {
			version: version.into(),
			parameters,
		};
		stream.writer.encode(&server_setup).await?;

		let (recv_bw, protocol) = match version {
			Version::Lite(v) => {
				let stream = stream.with_version(v);
				// Pre-lite-05: no Setup Stream, so nothing to advertise or seed.
				let start = lite::start(
					session.clone(),
					Some(stream),
					server.publish,
					server.subscribe,
					server.stats,
					v,
					lite::Setup::default(),
					None,
				)?;
				(start.recv_bandwidth, start.driver)
			}
			Version::Ietf(v) => {
				let stream = stream.with_version(v);
				// Draft 14-16: path came in the bidi SETUP, no uni SETUP to hand back.
				let protocol = ietf::start(
					session.clone(),
					Some(stream),
					request_id_max,
					false,
					server.publish,
					server.subscribe,
					server.stats,
					v,
					None,
					None,
				)?;
				(None, protocol)
			}
		};

		Ok(Session::new(session, version, recv_bw, protocol))
	}

	/// Reject the session, closing the transport with `err`'s wire code.
	pub fn close(mut self, err: Error) {
		let inner = self.inner.take().expect("request already responded");
		inner.close(err);
	}
}

impl<S: web_transport_trait::Session> RequestInner<S> {
	fn close(self, err: Error) {
		let session = match self.handshake {
			Handshake::IetfModern { session, .. } => session,
			Handshake::LiteBare { session, .. } => session,
			Handshake::Legacy { session, .. } => session,
			Handshake::LiteSetup { session, .. } => session,
		};
		session.close(err.to_code(), &err.to_string());
	}
}

impl<S: web_transport_trait::Session> Drop for Request<S> {
	// A dropped request would otherwise leave the client hanging until its idle
	// timeout: it already sent SETUP and is waiting on a response. Reject loudly.
	fn drop(&mut self) {
		if let Some(inner) = self.inner.take() {
			tracing::warn!("Request dropped without ok() or close(); rejecting the session");
			inner.close(Error::Cancel);
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::{
		collections::VecDeque,
		sync::{Arc, Mutex},
	};

	use crate::ALPN_LITE_05;
	use bytes::Bytes;

	#[derive(Debug, Clone, Default)]
	struct FakeError;
	impl std::fmt::Display for FakeError {
		fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
			write!(f, "fake transport error")
		}
	}
	impl std::error::Error for FakeError {}
	impl web_transport_trait::Error for FakeError {
		fn session_error(&self) -> Option<(u32, String)> {
			Some((0, "closed".to_string()))
		}
	}

	/// A session that replays a queue of unidirectional streams (each a `Vec<u8>`) in
	/// order from `accept_uni`; everything else is inert.
	#[derive(Clone)]
	struct FakeSession {
		protocol: Option<&'static str>,
		uni: Arc<Mutex<VecDeque<Vec<u8>>>>,
	}

	impl FakeSession {
		fn new(protocol: &'static str, uni: impl IntoIterator<Item = Vec<u8>>) -> Self {
			Self {
				protocol: Some(protocol),
				uni: Arc::new(Mutex::new(uni.into_iter().collect())),
			}
		}
	}

	impl web_transport_trait::Session for FakeSession {
		type SendStream = FakeSend;
		type RecvStream = FakeRecv;
		type Error = FakeError;

		async fn accept_uni(&self) -> Result<Self::RecvStream, Self::Error> {
			// Drop the guard before any await so the future stays Send.
			let data = self.uni.lock().unwrap().pop_front();
			match data {
				Some(data) => Ok(FakeRecv { data: data.into() }),
				None => std::future::pending().await,
			}
		}
		async fn accept_bi(&self) -> Result<(Self::SendStream, Self::RecvStream), Self::Error> {
			std::future::pending().await
		}
		async fn open_bi(&self) -> Result<(Self::SendStream, Self::RecvStream), Self::Error> {
			std::future::pending().await
		}
		async fn open_uni(&self) -> Result<Self::SendStream, Self::Error> {
			std::future::pending().await
		}
		fn send_datagram(&self, _payload: Bytes) -> Result<(), Self::Error> {
			Ok(())
		}
		async fn recv_datagram(&self) -> Result<Bytes, Self::Error> {
			std::future::pending().await
		}
		fn max_datagram_size(&self) -> usize {
			1200
		}
		fn protocol(&self) -> Option<&str> {
			self.protocol
		}
		fn close(&self, _code: u32, _reason: &str) {}
		async fn closed(&self) -> Self::Error {
			std::future::pending().await
		}
	}

	#[derive(Clone, Default)]
	struct FakeSend;
	impl web_transport_trait::SendStream for FakeSend {
		type Error = FakeError;
		async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
			Ok(buf.len())
		}
		fn set_priority(&mut self, _order: u8) {}
		fn finish(&mut self) -> Result<(), Self::Error> {
			Ok(())
		}
		fn reset(&mut self, _code: u32) {}
		async fn closed(&mut self) -> Result<(), Self::Error> {
			Ok(())
		}
	}

	struct FakeRecv {
		data: VecDeque<u8>,
	}
	impl web_transport_trait::RecvStream for FakeRecv {
		type Error = FakeError;
		async fn read(&mut self, dst: &mut [u8]) -> Result<Option<usize>, Self::Error> {
			if self.data.is_empty() {
				return Ok(None);
			}
			let size = dst.len().min(self.data.len());
			for slot in dst.iter_mut().take(size) {
				*slot = self.data.pop_front().unwrap();
			}
			Ok(Some(size))
		}
		fn stop(&mut self, _code: u32) {}
		async fn closed(&mut self) -> Result<(), Self::Error> {
			Ok(())
		}
	}

	/// Encode a lite-05 Setup Stream: the `DataType::Setup` tag then the SETUP message.
	fn lite05_setup(path: Option<&str>, role: Option<Role>) -> Vec<u8> {
		let v = lite::Version::Lite05;
		let mut buf = Vec::new();
		lite::DataType::Setup.encode(&mut buf, v).unwrap();
		lite::Setup {
			probe: lite::ProbeLevel::None,
			path: path.map(str::to_string),
			role,
		}
		.encode(&mut buf, v)
		.unwrap();
		buf
	}

	/// Encode a draft-17+ Setup Stream: the unified SETUP message, whose parameters
	/// carry the request path the same way lite-05's does.
	fn ietf_setup(version: ietf::Version, path: Option<&str>) -> Vec<u8> {
		let mut params = ietf::Parameters::default();
		if let Some(path) = path {
			params.set_bytes(ietf::ParameterBytes::Path, path.as_bytes().to_vec());
		}
		let parameters = params.encode_bytes(version).unwrap();

		let mut buf = Vec::new();
		setup::Setup { parameters }
			.encode(&mut buf, crate::Version::Ietf(version))
			.unwrap();
		buf
	}

	#[tokio::test(start_paused = true)]
	async fn accept_request_reads_ietf_path() {
		// Every draft-17+ version gates on the SETUP stream before starting, so the
		// path is known at authorization time just like lite-05.
		for (alpn, version) in [
			(ALPN_17, ietf::Version::Draft17),
			(ALPN_18, ietf::Version::Draft18),
			(ALPN_19, ietf::Version::Draft19),
		] {
			let session = FakeSession::new(alpn, [ietf_setup(version, Some("/team/room"))]);
			let request = Server::new().accept_request(session).await.unwrap();
			assert_eq!(request.path(), "/team/room", "{alpn}");
		}
	}

	#[tokio::test(start_paused = true)]
	async fn accept_request_ietf_without_path_is_empty() {
		let session = FakeSession::new(ALPN_19, [ietf_setup(ietf::Version::Draft19, None)]);
		let request = Server::new().accept_request(session).await.unwrap();
		assert_eq!(request.path(), "");
	}

	#[tokio::test(start_paused = true)]
	async fn accept_request_ietf_empty_path_is_accepted() {
		let session = FakeSession::new(ALPN_19, [ietf_setup(ietf::Version::Draft19, Some(""))]);
		let request = Server::new().accept_request(session).await.unwrap();
		assert_eq!(request.path(), "");
	}

	/// Encode a lite-05 GROUP uni stream header (just the `DataType::Group` tag).
	fn lite05_group() -> Vec<u8> {
		let mut buf = Vec::new();
		lite::DataType::Group.encode(&mut buf, lite::Version::Lite05).unwrap();
		buf
	}

	#[tokio::test(start_paused = true)]
	async fn accept_request_reads_lite05_path() {
		let session = FakeSession::new(ALPN_LITE_05, [lite05_setup(Some("/team/room"), None)]);
		let request = Server::new().accept_request(session).await.unwrap();
		assert_eq!(request.path(), "/team/room");
		assert_eq!(request.role(), None, "a client that omits the role is bidirectional");
	}

	#[tokio::test(start_paused = true)]
	async fn accept_request_lite05_without_path_is_empty() {
		let session = FakeSession::new(ALPN_LITE_05, [lite05_setup(None, None)]);
		let request = Server::new().accept_request(session).await.unwrap();
		assert_eq!(request.path(), "");
	}

	#[tokio::test(start_paused = true)]
	async fn accept_request_lite05_empty_path_is_accepted() {
		// An empty path is valid on the wire and means the same as omitting it, so a
		// client that wants the root doesn't have to special-case the parameter.
		let session = FakeSession::new(ALPN_LITE_05, [lite05_setup(Some(""), None)]);
		let request = Server::new().accept_request(session).await.unwrap();
		assert_eq!(request.path(), "");
	}

	#[tokio::test(start_paused = true)]
	async fn accept_request_reads_lite05_role() {
		let session = FakeSession::new(ALPN_LITE_05, [lite05_setup(Some("/team/room"), Some(Role::Publisher))]);
		let request = Server::new().accept_request(session).await.unwrap();
		assert_eq!(request.role(), Some(Role::Publisher));
	}

	#[tokio::test(start_paused = true)]
	async fn accept_request_skips_uni_stream_before_setup() {
		// A GROUP racing ahead of the SETUP is STOP_SENDING-ed and skipped; the gate
		// keeps reading until it finds the SETUP.
		let session = FakeSession::new(ALPN_LITE_05, [lite05_group(), lite05_setup(Some("/team/room"), None)]);
		let request = Server::new().accept_request(session).await.unwrap();
		assert_eq!(request.path(), "/team/room");
	}
}
