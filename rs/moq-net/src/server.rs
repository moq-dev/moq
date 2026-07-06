use crate::{
	ALPN_14, ALPN_15, ALPN_16, ALPN_17, ALPN_18, ALPN_19, ALPN_LITE, ALPN_LITE_03, ALPN_LITE_04, ALPN_LITE_05_WIP,
	Error, NEGOTIATED, OriginConsumer, OriginProducer, Session, StatsHandle, Version, Versions,
	coding::{Decode, Encode, Stream},
	ietf, lite, setup,
};

/// A MoQ server session builder.
#[derive(Default, Clone)]
pub struct Server {
	publish: Option<OriginConsumer>,
	consume: Option<OriginProducer>,
	stats: StatsHandle,
	versions: Versions,
}

impl Server {
	pub fn new() -> Self {
		Default::default()
	}

	pub fn with_publish(mut self, publish: impl Into<Option<OriginConsumer>>) -> Self {
		self.publish = publish.into();
		self
	}

	pub fn with_consume(mut self, consume: impl Into<Option<OriginProducer>>) -> Self {
		self.consume = consume.into();
		self
	}

	/// Attach a tier-scoped [`StatsHandle`]. Per-broadcast and per-subscription
	/// counters will be bumped through this handle for the lifetime of the session.
	/// Pass [`StatsHandle::default`] (a no-op handle) to opt out.
	pub fn with_stats(mut self, stats: StatsHandle) -> Self {
		self.stats = stats;
		self
	}

	/// Set both publish and consume from an `OriginProducer`.
	///
	/// This is equivalent to calling `with_publish(origin.consume())` and `with_consume(origin)`.
	pub fn with_origin(self, origin: OriginProducer) -> Self {
		let consumer = origin.consume();
		self.with_publish(consumer).with_consume(origin)
	}

	pub fn with_versions(mut self, versions: Versions) -> Self {
		self.versions = versions;
		self
	}

	/// Perform the MoQ handshake as a server, returning the established [`Session`].
	///
	/// Convenience wrapper over [`accept_request`](Self::accept_request) that completes
	/// the handshake immediately. Use `accept_request` when you need to inspect the
	/// client's advertised path before deciding what to serve.
	pub async fn accept<S: web_transport_trait::Session>(&self, session: S) -> Result<Session, Error> {
		self.accept_request(session).await?.ok().await
	}

	/// Begin the MoQ handshake, pausing once the client's request path is known so the
	/// caller can authorize or scope before serving.
	///
	/// Reads the client's SETUP (the in-band path lives there on URL-less transports),
	/// then returns a [`Request`]: inspect [`path`](Request::path), set the origins to
	/// serve, and call [`ok`](Request::ok) or [`close`](Request::close). Starting the
	/// session is deferred to `ok()`, so origins set on the `Request` take effect.
	///
	/// The path is surfaced for moq-lite-05; it is `None` on versions with no in-band
	/// request path (lite 01-04, and the IETF drafts in this build).
	pub async fn accept_request<S: web_transport_trait::Session>(&self, session: S) -> Result<Request<S>, Error> {
		// Regimes without an in-band path defer to `ok()` without surfacing one.
		let deferred = |handshake| Request {
			server: self.clone(),
			path: None,
			handshake,
		};

		let (encoding, supported) = match session.protocol() {
			Some(ALPN_19) => {
				self.versions
					.select(Version::Ietf(ietf::Version::Draft19))
					.ok_or(Error::Version)?;
				return Ok(deferred(Handshake::IetfModern {
					session,
					version: ietf::Version::Draft19,
				}));
			}
			Some(ALPN_18) => {
				self.versions
					.select(Version::Ietf(ietf::Version::Draft18))
					.ok_or(Error::Version)?;
				return Ok(deferred(Handshake::IetfModern {
					session,
					version: ietf::Version::Draft18,
				}));
			}
			Some(ALPN_17) => {
				self.versions
					.select(Version::Ietf(ietf::Version::Draft17))
					.ok_or(Error::Version)?;
				return Ok(deferred(Handshake::IetfModern {
					session,
					version: ietf::Version::Draft17,
				}));
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
			Some(ALPN_LITE_05_WIP) => {
				self.versions
					.select(Version::Lite(lite::Version::Lite05Wip))
					.ok_or(Error::Version)?;

				// Gate on the client's SETUP: read it before serving so the caller can
				// scope by the advertised path.
				let client_setup = lite::accept_setup(&session, lite::Version::Lite05Wip).await?;
				return Ok(Request {
					server: self.clone(),
					path: client_setup.path,
					handshake: Handshake::Lite05 { session },
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

		// Legacy bidi SETUP exchange (IETF 14-16, lite 01/02). Read the client's SETUP
		// to choose the version; `ok()` sends the server SETUP and starts the session.
		let mut stream = Stream::accept(&session, encoding).await?;
		let mut client: setup::Client = stream.reader.decode().await?;

		let version = client
			.versions
			.iter()
			.flat_map(|v| Version::try_from(*v).ok())
			.find(|v| supported.contains(v))
			.ok_or(Error::Version)?;

		// Pull the max request ID out now (IETF only) so `ok()` doesn't re-decode the
		// consumed parameters.
		let request_id_max = match version {
			Version::Ietf(v) => {
				let params = ietf::Parameters::decode(&mut client.parameters, v)?;
				params
					.get_varint(ietf::ParameterVarInt::MaxRequestId)
					.map(ietf::RequestId)
			}
			Version::Lite(_) => None,
		};

		Ok(deferred(Handshake::Legacy {
			session,
			stream,
			version,
			request_id_max,
		}))
	}
}

/// A paused server-side handshake.
///
/// Returned by [`Server::accept_request`] once the client's advertised
/// [`path`](Self::path) is known but before the session is granted anything. Set the
/// origins to serve, then call [`ok`](Self::ok) to complete the handshake, or
/// [`close`](Self::close) to reject it. Modeled on the WebTransport `Request`.
pub struct Request<S: web_transport_trait::Session> {
	server: Server,
	path: Option<String>,
	handshake: Handshake<S>,
}

/// The handshake state captured at the pause point. Every variant defers its session
/// start to [`Request::ok`] so origins set on the `Request` still apply.
enum Handshake<S: web_transport_trait::Session> {
	/// Modern IETF (17/18): SETUP is exchanged in the background by the session.
	IetfModern { session: S, version: ietf::Version },
	/// moq-lite 03/04: no Setup stream.
	LiteBare { session: S, version: lite::Version },
	/// moq-lite 05+: the client's Setup stream has already been read.
	Lite05 { session: S },
	/// Legacy IETF (14-16) and lite 01/02: the client SETUP has been read off the bidi
	/// stream but the server SETUP hasn't been sent. `ok()` finishes it.
	Legacy {
		session: S,
		stream: Stream<S, Version>,
		version: Version,
		request_id_max: Option<ietf::RequestId>,
	},
}

impl<S: web_transport_trait::Session> Request<S> {
	/// The request path the client advertised in its SETUP, if any.
	///
	/// Populated for moq-lite-05; `None` on versions without an in-band request path.
	/// See the note on [`Server::accept_request`].
	pub fn path(&self) -> Option<&str> {
		self.path.as_deref()
	}

	/// Publish to the connected client. Overrides any value from the [`Server`]
	/// builder; typically set after inspecting [`path`](Self::path).
	pub fn with_publish(mut self, publish: impl Into<Option<OriginConsumer>>) -> Self {
		self.server.publish = publish.into();
		self
	}

	/// Subscribe to the connected client. Overrides any value from the [`Server`] builder.
	pub fn with_consume(mut self, consume: impl Into<Option<OriginProducer>>) -> Self {
		self.server.consume = consume.into();
		self
	}

	/// Set the tier-scoped stats handle. Overrides any value from the [`Server`] builder.
	pub fn with_stats(mut self, stats: StatsHandle) -> Self {
		self.server.stats = stats;
		self
	}

	/// Accept the session, completing the handshake.
	pub async fn ok(self) -> Result<Session, Error> {
		let server = self.server;

		// Warn here, not in `accept_request`: callers attach origins on the Request
		// (after inspecting the path), so checking earlier gives false positives.
		if server.publish.is_none() && server.consume.is_none() {
			tracing::warn!("not publishing or consuming anything");
		}

		let (session, mut stream, version, request_id_max) = match self.handshake {
			Handshake::IetfModern { session, version } => {
				ietf::start(
					session.clone(),
					None,
					None,
					false,
					server.publish,
					server.consume,
					server.stats,
					version,
				)?;
				tracing::debug!(?version, "connected");
				return Ok(Session::new(session, version.into(), None));
			}
			Handshake::LiteBare { session, version } => {
				let recv_bw = lite::start(
					session.clone(),
					None,
					server.publish,
					server.consume,
					server.stats,
					version,
					lite::Setup::default(),
				)?;
				return Ok(Session::new(session, version.into(), recv_bw));
			}
			Handshake::Lite05 { session } => {
				// A server never advertises a request path.
				let recv_bw = lite::start(
					session.clone(),
					None,
					server.publish,
					server.consume,
					server.stats,
					lite::Version::Lite05Wip,
					lite::Setup::default(),
				)?;
				return Ok(Session::new(session, lite::Version::Lite05Wip.into(), recv_bw));
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

		let recv_bw = match version {
			Version::Lite(v) => {
				let stream = stream.with_version(v);
				// Pre-lite-05: no Setup stream, so nothing to advertise.
				lite::start(
					session.clone(),
					Some(stream),
					server.publish,
					server.consume,
					server.stats,
					v,
					lite::Setup::default(),
				)?
			}
			Version::Ietf(v) => {
				let stream = stream.with_version(v);
				ietf::start(
					session.clone(),
					Some(stream),
					request_id_max,
					false,
					server.publish,
					server.consume,
					server.stats,
					v,
				)?;
				None
			}
		};

		Ok(Session::new(session, version, recv_bw))
	}

	/// Reject the session, closing the transport with `err`'s wire code.
	pub fn close(self, err: Error) {
		let session = match self.handshake {
			Handshake::IetfModern { session, .. } => session,
			Handshake::LiteBare { session, .. } => session,
			Handshake::Lite05 { session } => session,
			Handshake::Legacy { session, .. } => session,
		};
		session.close(err.to_code(), &err.to_string());
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::{
		collections::VecDeque,
		sync::{Arc, Mutex},
	};

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

	/// Encode a lite-05 Setup stream: the `DataType::Setup` tag then the SETUP message.
	fn lite05_setup(path: Option<&str>) -> Vec<u8> {
		let v = lite::Version::Lite05Wip;
		let mut buf = Vec::new();
		lite::DataType::Setup.encode(&mut buf, v).unwrap();
		lite::Setup {
			path: path.map(str::to_string),
		}
		.encode(&mut buf, v)
		.unwrap();
		buf
	}

	/// Encode a lite-05 Group uni stream header (just the `DataType::Group` tag).
	fn lite05_group() -> Vec<u8> {
		let mut buf = Vec::new();
		lite::DataType::Group
			.encode(&mut buf, lite::Version::Lite05Wip)
			.unwrap();
		buf
	}

	#[tokio::test(start_paused = true)]
	async fn accept_request_reads_lite05_path() {
		let session = FakeSession::new(ALPN_LITE_05_WIP, [lite05_setup(Some("/team/room"))]);
		let request = Server::new()
			.with_versions(Version::Lite(lite::Version::Lite05Wip).into())
			.accept_request(session)
			.await
			.unwrap();
		assert_eq!(request.path(), Some("/team/room"));
	}

	#[tokio::test(start_paused = true)]
	async fn accept_request_lite05_without_path_is_none() {
		let session = FakeSession::new(ALPN_LITE_05_WIP, [lite05_setup(None)]);
		let request = Server::new()
			.with_versions(Version::Lite(lite::Version::Lite05Wip).into())
			.accept_request(session)
			.await
			.unwrap();
		assert_eq!(request.path(), None);
	}

	#[tokio::test(start_paused = true)]
	async fn accept_request_skips_uni_stream_before_setup() {
		// A Group racing ahead of the SETUP is reset and skipped; the gate keeps
		// reading until it finds the SETUP.
		let session = FakeSession::new(ALPN_LITE_05_WIP, [lite05_group(), lite05_setup(Some("/team/room"))]);
		let request = Server::new()
			.with_versions(Version::Lite(lite::Version::Lite05Wip).into())
			.accept_request(session)
			.await
			.unwrap();
		assert_eq!(request.path(), Some("/team/room"));
	}
}
