use crate::{
	ALPN_14, ALPN_15, ALPN_16, ALPN_LITE, ALPN_LITE_03, Error, NEGOTIATED, OriginConsumer, OriginProducer, Session,
	Version, Versions,
	coding::{self, Decode, Encode, Stream},
	ietf, lite, setup,
};

/// A MoQ client session builder.
#[derive(Default, Clone)]
pub struct Client {
	publish: Option<OriginConsumer>,
	consume: Option<OriginProducer>,
	versions: Versions,
}

impl Client {
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

	/// Perform the MoQ handshake as a client negotiating the version.
	pub async fn connect<S: web_transport_trait::Session>(&self, session: S) -> Result<Session, Error> {
		if self.publish.is_none() && self.consume.is_none() {
			tracing::warn!("not publishing or consuming anything");
		}

		// If ALPN was used to negotiate the version, use the appropriate encoding.
		// Default to IETF 14 if no ALPN was used and we'll negotiate the version later.
		let (encoding, supported) = match session.protocol() {
			Some(ALPN_16) => {
				let v = self.versions.select(Version::Draft16).ok_or(Error::Version)?;
				(v, v.into())
			}
			Some(ALPN_15) => {
				let v = self.versions.select(Version::Draft15).ok_or(Error::Version)?;
				(v, v.into())
			}
			Some(ALPN_14) => {
				let v = self.versions.select(Version::Draft14).ok_or(Error::Version)?;
				(v, v.into())
			}
			Some(ALPN_LITE_03) => {
				self.versions.select(Version::Lite03).ok_or(Error::Version)?;

				// Starting with draft-03, there's no more SETUP control stream.
				lite::start(
					session.clone(),
					None,
					self.publish.clone(),
					self.consume.clone(),
					Version::Lite03,
				)?;

				tracing::debug!(version = ?Version::Lite03, "connected");

				return Ok(Session::new(session));
			}
			Some(ALPN_LITE) | None => {
				let supported = self.versions.filter(&NEGOTIATED.into()).ok_or(Error::Version)?;
				(Version::Draft14, supported)
			}
			Some(p) => return Err(Error::UnknownAlpn(p.to_string())),
		};

		let mut stream = Stream::open(&session, encoding).await?;

		let mut parameters = ietf::Parameters::default();
		parameters.set_varint(ietf::ParameterVarInt::MaxRequestId, u32::MAX as u64);
		parameters.set_bytes(ietf::ParameterBytes::Implementation, b"moq-lite-rs".to_vec());
		let parameters = parameters.encode_bytes(encoding)?;

		let client = setup::Client {
			versions: supported.clone().into(),
			parameters,
		};

		// TODO pretty print the parameters.
		tracing::trace!(?client, "sending client setup");
		stream.writer.encode(&client).await?;

		let mut server: setup::Server = stream.reader.decode().await?;
		tracing::trace!(?server, "received server setup");

		let version = supported
			.iter()
			.find(|v| coding::Version::from(**v) == server.version)
			.copied()
			.ok_or(Error::Version)?;

		// Switch the stream to the negotiated version.
		stream.set_version(version);

		if version.is_lite() {
			lite::start(
				session.clone(),
				Some(stream),
				self.publish.clone(),
				self.consume.clone(),
				version,
			)?;
		} else {
			// Decode the parameters to get the initial request ID.
			let parameters = ietf::Parameters::decode(&mut server.parameters, version)?;
			let request_id_max = ietf::RequestId(
				parameters
					.get_varint(ietf::ParameterVarInt::MaxRequestId)
					.unwrap_or_default(),
			);

			ietf::start(
				session.clone(),
				stream,
				request_id_max,
				true,
				self.publish.clone(),
				self.consume.clone(),
				version,
			)?;
		}

		tracing::debug!(version = ?server.version, "connected");

		Ok(Session::new(session))
	}
}
