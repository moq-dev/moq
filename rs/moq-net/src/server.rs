use crate::{
	ALPN_14, ALPN_15, ALPN_16, ALPN_17, ALPN_18, ALPN_LITE, ALPN_LITE_03, ALPN_LITE_04, ALPN_LITE_05_WIP, Error,
	NEGOTIATED, Origin, OriginConsumer, OriginProducer, Session, StatsHandle, Version, Versions,
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
	/// Pass [`StatsHandle::disabled`] (also the default) to opt out.
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

	/// Perform the MoQ handshake as a server for the given session.
	///
	/// If neither a publish nor a consume origin was set on this builder,
	/// accept auto-creates a fresh [`Origin`] and wires it as both, then
	/// surfaces the producer and consumer sides on the returned
	/// [`Session`] via [`Session::publisher`] and [`Session::consumer`].
	/// Callers who configured their own origin(s) see both as `None` and
	/// continue to drive publishing / consuming through the origins they
	/// already hold.
	pub async fn accept<S: web_transport_trait::Session>(&self, session: S) -> Result<Session, Error> {
		// Effective publish / consume after potential auto-creation. The
		// `auto_*` locals are populated only on the no-config path so the
		// returned Session reflects what we actually own.
		let (publish, consume, auto_publisher, auto_consumer) = match (self.publish.clone(), self.consume.clone()) {
			(None, None) => {
				let producer = Origin::random().produce();
				let consumer = producer.consume();
				(
					Some(producer.consume()),
					Some(producer.clone()),
					Some(producer),
					Some(consumer),
				)
			}
			(publish, consume) => (publish, consume, None, None),
		};

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
					publish.clone(),
					consume.clone(),
					ietf::Version::Draft18,
				)?;

				tracing::debug!(version = ?v, "connected");
				return Ok(Session::new(session, v, None).with_origins(auto_publisher, auto_consumer));
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
					publish.clone(),
					consume.clone(),
					ietf::Version::Draft17,
				)?;

				tracing::debug!(version = ?v, "connected");
				// TODO: ietf code path does not yet record stats.
				return Ok(Session::new(session, v, None).with_origins(auto_publisher, auto_consumer));
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

				let recv_bw = lite::start(
					session.clone(),
					None,
					publish.clone(),
					consume.clone(),
					self.stats.clone(),
					lite::Version::Lite05Wip,
				)?;

				return Ok(Session::new(session, lite::Version::Lite05Wip.into(), recv_bw)
					.with_origins(auto_publisher, auto_consumer));
			}
			Some(ALPN_LITE_04) => {
				self.versions
					.select(Version::Lite(lite::Version::Lite04))
					.ok_or(Error::Version)?;

				let recv_bw = lite::start(
					session.clone(),
					None,
					publish.clone(),
					consume.clone(),
					self.stats.clone(),
					lite::Version::Lite04,
				)?;

				return Ok(Session::new(session, lite::Version::Lite04.into(), recv_bw)
					.with_origins(auto_publisher, auto_consumer));
			}
			Some(ALPN_LITE_03) => {
				self.versions
					.select(Version::Lite(lite::Version::Lite03))
					.ok_or(Error::Version)?;

				// Starting with draft-03, there's no more SETUP control stream.
				let recv_bw = lite::start(
					session.clone(),
					None,
					publish.clone(),
					consume.clone(),
					self.stats.clone(),
					lite::Version::Lite03,
				)?;

				return Ok(Session::new(session, lite::Version::Lite03.into(), recv_bw)
					.with_origins(auto_publisher, auto_consumer));
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
				lite::start(
					session.clone(),
					Some(stream),
					publish.clone(),
					consume.clone(),
					self.stats.clone(),
					v,
				)?
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
					publish.clone(),
					consume.clone(),
					v,
				)?;
				None
			}
		};

		Ok(Session::new(session, version, recv_bw).with_origins(auto_publisher, auto_consumer))
	}
}
