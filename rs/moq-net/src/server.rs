use crate::{
	ALPN_14, ALPN_15, ALPN_16, ALPN_17, ALPN_18, ALPN_LITE, ALPN_LITE_03, ALPN_LITE_04, ALPN_LITE_05_WIP, Consume,
	Error, NEGOTIATED, OriginConsumer, OriginProducer, Session, StatsHandle, Version, Versions,
	coding::{Decode, Encode, Stream},
	ietf, lite, setup,
};

/// A MoQ server session builder.
#[derive(Default, Clone)]
pub struct Server {
	publish: Option<OriginConsumer>,
	subscribe: Option<OriginProducer>,
	stats: StatsHandle,
	versions: Versions,
}

impl Server {
	pub fn new() -> Self {
		Default::default()
	}

	/// Publish to the connected client: the session reads from the given origin
	/// (pass an [`OriginProducer`] or [`OriginConsumer`] by reference) and forwards
	/// its announcements. Omit to publish nothing. Pre-scoped via
	/// [`OriginProducer::scope`] for token-gated relays.
	pub fn with_publisher(mut self, publish: &impl Consume<OriginConsumer>) -> Self {
		self.publish = Some(publish.consume());
		self
	}

	/// Subscribe to the connected client: the session writes the broadcasts the
	/// client announces into this [`OriginProducer`]. Omit to subscribe to nothing.
	pub fn with_subscriber(mut self, subscribe: OriginProducer) -> Self {
		self.subscribe = Some(subscribe);
		self
	}

	/// Deprecated alias for [`with_publisher`](Self::with_publisher).
	#[deprecated(note = "renamed to `with_publisher`")]
	pub fn with_publish(self, publish: OriginConsumer) -> Self {
		self.with_publisher(&publish)
	}

	/// Deprecated alias for [`with_subscriber`](Self::with_subscriber).
	#[deprecated(note = "renamed to `with_subscriber`")]
	pub fn with_consume(self, subscribe: OriginProducer) -> Self {
		self.with_subscriber(subscribe)
	}

	/// Attach a tier-scoped [`StatsHandle`]. Per-broadcast and per-subscription
	/// counters will be bumped through this handle for the lifetime of the session.
	/// Pass [`StatsHandle::default`] (a no-op handle) to opt out.
	pub fn with_stats(mut self, stats: StatsHandle) -> Self {
		self.stats = stats;
		self
	}

	/// Set both publish and subscribe from one shared [`OriginProducer`].
	pub fn with_origin(self, origin: OriginProducer) -> Self {
		self.with_publisher(&origin).with_subscriber(origin)
	}

	pub fn with_versions(mut self, versions: Versions) -> Self {
		self.versions = versions;
		self
	}

	/// Perform the MoQ handshake as a server for the given session.
	pub async fn accept<S: web_transport_trait::Session>(&self, session: S) -> Result<Session, Error> {
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
					false,
					self.publish.clone(),
					self.subscribe.clone(),
					self.stats.clone(),
					ietf::Version::Draft18,
				)?;

				tracing::debug!(version = ?v, "connected");
				return Ok(Session::new(session, v, None));
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
					false,
					self.publish.clone(),
					self.subscribe.clone(),
					self.stats.clone(),
					ietf::Version::Draft17,
				)?;

				tracing::debug!(version = ?v, "connected");
				return Ok(Session::new(session, v, None));
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

				// Server side never blocks on the initial set; discard the synced receiver.
				let (recv_bw, _connecting) = lite::start(
					session.clone(),
					None,
					self.publish.clone(),
					self.subscribe.clone(),
					self.stats.clone(),
					lite::Version::Lite05Wip,
				)?;

				return Ok(Session::new(session, lite::Version::Lite05Wip.into(), recv_bw));
			}
			Some(ALPN_LITE_04) => {
				self.versions
					.select(Version::Lite(lite::Version::Lite04))
					.ok_or(Error::Version)?;

				let (recv_bw, _connecting) = lite::start(
					session.clone(),
					None,
					self.publish.clone(),
					self.subscribe.clone(),
					self.stats.clone(),
					lite::Version::Lite04,
				)?;

				return Ok(Session::new(session, lite::Version::Lite04.into(), recv_bw));
			}
			Some(ALPN_LITE_03) => {
				self.versions
					.select(Version::Lite(lite::Version::Lite03))
					.ok_or(Error::Version)?;

				// Starting with draft-03, there's no more SETUP control stream.
				let (recv_bw, _connecting) = lite::start(
					session.clone(),
					None,
					self.publish.clone(),
					self.subscribe.clone(),
					self.stats.clone(),
					lite::Version::Lite03,
				)?;

				return Ok(Session::new(session, lite::Version::Lite03.into(), recv_bw));
			}
			Some(ALPN_LITE) | None => {
				let supported = self.versions.filter(&NEGOTIATED.into()).ok_or(Error::Version)?;
				(Version::Ietf(ietf::Version::Draft14), supported)
			}
			Some(p) => return Err(Error::UnknownAlpn(p.to_string())),
		};

		let mut stream = Stream::accept(&session, encoding).await?;

		let mut client: setup::Client = stream.reader.decode().await?;

		// Choose the version to use
		let version = client
			.versions
			.iter()
			.flat_map(|v| Version::try_from(*v).ok())
			.find(|v| supported.contains(v))
			.ok_or(Error::Version)?;

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

		let server = setup::Server {
			version: version.into(),
			parameters,
		};
		stream.writer.encode(&server).await?;

		let recv_bw = match version {
			Version::Lite(v) => {
				let stream = stream.with_version(v);
				let (recv_bw, _connecting) = lite::start(
					session.clone(),
					Some(stream),
					self.publish.clone(),
					self.subscribe.clone(),
					self.stats.clone(),
					v,
				)?;
				recv_bw
			}
			Version::Ietf(v) => {
				// Decode the client's parameters to get their max request ID.
				let parameters = ietf::Parameters::decode(&mut client.parameters, v)?;
				let request_id_max = parameters
					.get_varint(ietf::ParameterVarInt::MaxRequestId)
					.map(ietf::RequestId);

				let stream = stream.with_version(v);
				ietf::start(
					session.clone(),
					Some(stream),
					request_id_max,
					false,
					self.publish.clone(),
					self.subscribe.clone(),
					self.stats.clone(),
					v,
				)?;
				None
			}
		};

		Ok(Session::new(session, version, recv_bw))
	}
}
