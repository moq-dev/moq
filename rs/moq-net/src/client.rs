use crate::{
	ALPN_14, ALPN_15, ALPN_16, ALPN_17, ALPN_18, ALPN_LITE, ALPN_LITE_03, ALPN_LITE_04, ALPN_LITE_05_WIP, Error,
	NEGOTIATED, Origin, OriginProducer, Session, StatsHandle, Version, Versions,
	coding::{self, Decode, Encode, Stream},
	ietf, lite, setup,
};

/// A MoQ client session builder.
#[derive(Clone)]
pub struct Client {
	publish: OriginProducer,
	consume: OriginProducer,
	stats: StatsHandle,
	versions: Versions,
}

impl Default for Client {
	fn default() -> Self {
		// Default to one shared fresh origin so the typical duplex
		// client just works without any setup.
		let shared = Origin::random().produce();
		Self {
			publish: shared.clone(),
			consume: shared,
			stats: StatsHandle::default(),
			versions: Versions::default(),
		}
	}
}

impl Client {
	pub fn new() -> Self {
		Default::default()
	}

	/// Override the publish-side origin: the [`OriginProducer`] this
	/// client reads from when forwarding local broadcasts to the remote.
	/// Surfaced as [`Session::publisher`] so callers can keep
	/// `.publish_broadcast(path, broadcast)`-ing after connect.
	///
	/// Pre-scoped via [`OriginProducer::scope`] for token-gated relays.
	pub fn with_publisher(mut self, publish: OriginProducer) -> Self {
		self.publish = publish;
		self
	}

	/// Override the consume-side origin: the [`OriginProducer`] this
	/// client writes into as the remote announces broadcasts. A consumer
	/// view is surfaced as [`Session::consumer`].
	pub fn with_consumer(mut self, consume: OriginProducer) -> Self {
		self.consume = consume;
		self
	}

	/// Attach a tier-scoped [`StatsHandle`]. Per-broadcast and per-subscription
	/// counters will be bumped through this handle for the lifetime of the session.
	/// Pass [`StatsHandle::default`] (a no-op handle) to opt out.
	pub fn with_stats(mut self, stats: StatsHandle) -> Self {
		self.stats = stats;
		self
	}

	/// Set both publish and consume from one shared [`OriginProducer`].
	///
	/// Equivalent to calling [`with_publisher`](Self::with_publisher) and
	/// [`with_consumer`](Self::with_consumer) with the same origin.
	pub fn with_origin(self, origin: OriginProducer) -> Self {
		self.with_publisher(origin.clone()).with_consumer(origin)
	}

	pub fn with_versions(mut self, versions: Versions) -> Self {
		self.versions = versions;
		self
	}

	/// Perform the MoQ handshake as a client negotiating the version.
	pub async fn connect<S: web_transport_trait::Session>(&self, session: S) -> Result<Session, Error> {
		let publisher = self.publish.clone();
		let consumer = self.consume.clone();
		let publish = publisher.consume();
		let consume = consumer.clone();
		let consumer_view = consumer.consume();

		// If ALPN was used to negotiate the version, use the appropriate encoding.
		// Default to IETF 14 if no ALPN was used and we'll negotiate the version later.
		let (encoding, supported) = match session.protocol() {
			Some(ALPN_18) => {
				let v = self
					.versions
					.select(Version::Ietf(ietf::Version::Draft18))
					.ok_or(Error::Version)?;

				// Draft-17+: SETUP is exchanged in the background by the session.
				ietf::start(
					session.clone(),
					None,
					None,
					true,
					publish,
					consume,
					ietf::Version::Draft18,
				)?;

				tracing::debug!(version = ?v, "connected");
				return Ok(Session::new(session, v, None, publisher.clone(), consumer_view.clone()));
			}
			Some(ALPN_17) => {
				let v = self
					.versions
					.select(Version::Ietf(ietf::Version::Draft17))
					.ok_or(Error::Version)?;

				// Draft-17+: SETUP is exchanged in the background by the session.
				ietf::start(
					session.clone(),
					None,
					None,
					true,
					publish,
					consume,
					ietf::Version::Draft17,
				)?;

				tracing::debug!(version = ?v, "connected");
				return Ok(Session::new(session, v, None, publisher.clone(), consumer_view.clone()));
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

				let (recv_bw, connecting) = lite::start(
					session.clone(),
					None,
					publish,
					consume,
					self.stats.clone(),
					lite::Version::Lite05Wip,
				)?;

				// Block until the initial announce set has landed (Lite05 reports it
				// via AnnounceOk + N), so a synchronous get_broadcast() won't race it.
				connecting.ready().await;

				return Ok(Session::new(
					session,
					lite::Version::Lite05Wip.into(),
					recv_bw,
					publisher.clone(),
					consumer_view.clone(),
				));
			}
			Some(ALPN_LITE_04) => {
				self.versions
					.select(Version::Lite(lite::Version::Lite04))
					.ok_or(Error::Version)?;

				let (recv_bw, connecting) = lite::start(
					session.clone(),
					None,
					publish,
					consume,
					self.stats.clone(),
					lite::Version::Lite04,
				)?;

				// Lite04 has no initial-set boundary, so this resolves immediately.
				connecting.ready().await;

				return Ok(Session::new(
					session,
					lite::Version::Lite04.into(),
					recv_bw,
					publisher.clone(),
					consumer_view.clone(),
				));
			}
			Some(ALPN_LITE_03) => {
				self.versions
					.select(Version::Lite(lite::Version::Lite03))
					.ok_or(Error::Version)?;

				// Starting with draft-03, there's no more SETUP control stream.
				let (recv_bw, connecting) = lite::start(
					session.clone(),
					None,
					publish,
					consume,
					self.stats.clone(),
					lite::Version::Lite03,
				)?;

				// Lite03 has no initial-set boundary, so this resolves immediately.
				connecting.ready().await;

				return Ok(Session::new(
					session,
					lite::Version::Lite03.into(),
					recv_bw,
					publisher.clone(),
					consumer_view.clone(),
				));
			}
			Some(ALPN_LITE) | None => {
				let supported = self.versions.filter(&NEGOTIATED.into()).ok_or(Error::Version)?;
				(Version::Ietf(ietf::Version::Draft14), supported)
			}
			Some(p) => return Err(Error::UnknownAlpn(p.to_string())),
		};

		let mut stream = Stream::open(&session, encoding).await?;

		// The encoding is always an IETF version for SETUP negotiation.
		let ietf_encoding = ietf::Version::try_from(encoding).map_err(|_| Error::Version)?;

		let mut parameters = ietf::Parameters::default();
		parameters.set_varint(ietf::ParameterVarInt::MaxRequestId, u32::MAX as u64);
		parameters.set_bytes(ietf::ParameterBytes::Implementation, b"moq-lite-rs".to_vec());
		let parameters = parameters.encode_bytes(ietf_encoding)?;

		let client = setup::Client {
			versions: supported.clone().into(),
			parameters,
		};

		stream.writer.encode(&client).await?;

		let mut server: setup::Server = stream.reader.decode().await?;

		let version = supported
			.iter()
			.find(|v| coding::Version::from(**v) == server.version)
			.copied()
			.ok_or(Error::Version)?;

		let recv_bw = match version {
			Version::Lite(v) => {
				let stream = stream.with_version(v);
				let (recv_bw, connecting) =
					lite::start(session.clone(), Some(stream), publish, consume, self.stats.clone(), v)?;

				// Block until the initial announce set has landed (for versions that
				// report one); resolves immediately otherwise.
				connecting.ready().await;

				recv_bw
			}
			Version::Ietf(v) => {
				// Decode the parameters to get the initial request ID.
				let parameters = ietf::Parameters::decode(&mut server.parameters, v)?;
				let request_id_max = parameters
					.get_varint(ietf::ParameterVarInt::MaxRequestId)
					.map(ietf::RequestId);

				let stream = stream.with_version(v);
				ietf::start(session.clone(), Some(stream), request_id_max, true, publish, consume, v)?;
				None
			}
		};

		Ok(Session::new(session, version, recv_bw, publisher, consumer_view))
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::{
		collections::VecDeque,
		sync::{Arc, Mutex},
	};

	use crate::coding::{Decode, Encode};
	use bytes::{BufMut, Bytes};

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

	#[derive(Clone, Default)]
	struct FakeSession {
		state: Arc<FakeSessionState>,
	}

	#[derive(Default)]
	struct FakeSessionState {
		protocol: Option<&'static str>,
		control_stream: Mutex<Option<(FakeSendStream, FakeRecvStream)>>,
		close_events: Mutex<Vec<(u32, String)>>,
		close_notify: tokio::sync::Notify,
		control_writes: Arc<Mutex<Vec<u8>>>,
	}

	impl FakeSession {
		fn new(protocol: Option<&'static str>, server_control_bytes: Vec<u8>) -> Self {
			let writes = Arc::new(Mutex::new(Vec::new()));
			let send = FakeSendStream { writes: writes.clone() };
			let recv = FakeRecvStream {
				data: VecDeque::from(server_control_bytes),
			};
			let state = FakeSessionState {
				protocol,
				control_stream: Mutex::new(Some((send, recv))),
				close_events: Mutex::new(Vec::new()),
				close_notify: tokio::sync::Notify::new(),
				control_writes: writes,
			};
			Self { state: Arc::new(state) }
		}

		fn control_writes(&self) -> Vec<u8> {
			self.state.control_writes.lock().unwrap().clone()
		}

		async fn wait_for_first_close(&self) -> (u32, String) {
			loop {
				let notified = self.state.close_notify.notified();
				if let Some(close) = self.state.close_events.lock().unwrap().first().cloned() {
					return close;
				}
				notified.await;
			}
		}
	}

	impl web_transport_trait::Session for FakeSession {
		type SendStream = FakeSendStream;
		type RecvStream = FakeRecvStream;
		type Error = FakeError;

		async fn accept_uni(&self) -> Result<Self::RecvStream, Self::Error> {
			std::future::pending().await
		}

		async fn accept_bi(&self) -> Result<(Self::SendStream, Self::RecvStream), Self::Error> {
			std::future::pending().await
		}

		async fn open_bi(&self) -> Result<(Self::SendStream, Self::RecvStream), Self::Error> {
			self.state.control_stream.lock().unwrap().take().ok_or(FakeError)
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
			self.state.protocol
		}

		fn close(&self, code: u32, reason: &str) {
			self.state.close_events.lock().unwrap().push((code, reason.to_string()));
			self.state.close_notify.notify_waiters();
		}

		async fn closed(&self) -> Self::Error {
			self.state.close_notify.notified().await;
			FakeError
		}
	}

	#[derive(Clone, Default)]
	struct FakeSendStream {
		writes: Arc<Mutex<Vec<u8>>>,
	}

	impl web_transport_trait::SendStream for FakeSendStream {
		type Error = FakeError;

		async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
			self.writes.lock().unwrap().put_slice(buf);
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

	struct FakeRecvStream {
		data: VecDeque<u8>,
	}

	impl web_transport_trait::RecvStream for FakeRecvStream {
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

	fn mock_server_setup(negotiated: Version) -> Vec<u8> {
		let mut encoded = Vec::new();
		let server = setup::Server {
			version: negotiated.into(),
			parameters: Bytes::new(),
		};
		server
			.encode(&mut encoded, Version::Ietf(ietf::Version::Draft14))
			.unwrap();

		// Add a setup-stream SessionInfo frame using the negotiated Lite version.
		let info = lite::SessionInfo { bitrate: Some(1) };
		let lite_v = lite::Version::try_from(negotiated).unwrap();
		info.encode(&mut encoded, lite_v).unwrap();

		encoded
	}

	async fn run_alpn_lite_fallback_case(protocol: Option<&'static str>) {
		let fake = FakeSession::new(protocol, mock_server_setup(Version::Lite(lite::Version::Lite01)));
		let client = Client::new().with_versions(
			[
				Version::Lite(lite::Version::Lite03),
				Version::Lite(lite::Version::Lite02),
				Version::Lite(lite::Version::Lite01),
				Version::Ietf(ietf::Version::Draft14),
			]
			.into(),
		);

		let _session = client.connect(fake.clone()).await.unwrap();

		// Verify the client setup was encoded using Draft14 framing (ALPN_LITE fallback path).
		let mut setup_bytes = Bytes::from(fake.control_writes());
		let setup = setup::Client::decode(&mut setup_bytes, Version::Ietf(ietf::Version::Draft14)).unwrap();
		let advertised: Vec<Version> = setup.versions.iter().map(|v| Version::try_from(*v).unwrap()).collect();
		assert_eq!(
			advertised,
			vec![
				Version::Lite(lite::Version::Lite02),
				Version::Lite(lite::Version::Lite01),
				Version::Ietf(ietf::Version::Draft14),
			]
		);

		// The first close comes from the background lite session task.
		// Any non-Version error here means SessionInfo decoded successfully
		// after set_version(). This test cares about the SETUP framing
		// fallback, not the specific close code. Cancel is what we'd see
		// with no origin; RequiredExtension (or similar) is what an
		// auto-created origin's first interaction with a Lite01 peer trips.
		let (code, _) = fake.wait_for_first_close().await;
		assert_ne!(code, Error::Version.to_code(), "SessionInfo failed to decode");
	}

	#[tokio::test(start_paused = true)]
	async fn alpn_lite_falls_back_to_draft14_and_switches_version_post_setup() {
		run_alpn_lite_fallback_case(Some(ALPN_LITE)).await;
	}

	#[tokio::test(start_paused = true)]
	async fn no_alpn_falls_back_to_draft14_and_switches_version_post_setup() {
		run_alpn_lite_fallback_case(None).await;
	}
}
