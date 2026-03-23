use crate::{
	ALPN_14, ALPN_15, ALPN_16, ALPN_17, ALPN_LITE, ALPN_LITE_03, Error, NEGOTIATED, OriginConsumer, OriginProducer,
	Session, Version, Versions,
	coding::{Decode, Encode, Stream},
	ietf, lite, setup,
};

/// A MoQ server session builder.
#[derive(Default, Clone)]
pub struct Server {
	publish: Option<OriginConsumer>,
	consume: Option<OriginProducer>,
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

	pub fn with_versions(mut self, versions: Versions) -> Self {
		self.versions = versions;
		self
	}

	/// Perform the MoQ handshake as a server for the given session.
	pub async fn accept<S: web_transport_trait::Session>(&self, session: S) -> Result<Session, Error> {
		if self.publish.is_none() && self.consume.is_none() {
			tracing::warn!("not publishing or consuming anything");
		}

		let (encoding, supported) = match session.protocol() {
			Some(ALPN_17) => {
				let v = self
					.versions
					.select(Version::Ietf(ietf::Version::Draft17))
					.ok_or(Error::Version)?;

				// Draft-17: SETUP is exchanged in the background by the session.
				ietf::start(
					session.clone(),
					None,
					None,
					false,
					self.publish.clone(),
					self.consume.clone(),
					ietf::Version::Draft17,
				)?;

				tracing::debug!(version = ?v, "connected");
				return Ok(Session::new(session, v));
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
			Some(ALPN_LITE_03) => {
				self.versions
					.select(Version::Lite(lite::Version::Lite03))
					.ok_or(Error::Version)?;

				// Starting with draft-03, there's no more SETUP control stream.
				lite::start(
					session.clone(),
					None,
					self.publish.clone(),
					self.consume.clone(),
					lite::Version::Lite03,
				)?;

				return Ok(Session::new(session, lite::Version::Lite03.into()));
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

		match version {
			Version::Lite(v) => {
				let stream = stream.with_version(v);
				lite::start(
					session.clone(),
					Some(stream),
					self.publish.clone(),
					self.consume.clone(),
					v,
				)?;
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
					self.consume.clone(),
					v,
				)?;
			}
		};

		Ok(Session::new(session, version))
	}
}
