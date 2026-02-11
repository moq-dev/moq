// TODO: Uncomment when observability feature is merged
// use std::sync::Arc;

use crate::{
	Error, NEGOTIATED, OriginConsumer, OriginProducer, Session, Version,
	coding::{self, Decode, Encode, Stream},
	ietf, lite, setup,
};

/// A MoQ client session builder.
#[derive(Default, Clone)]
pub struct Client {
	publish: Option<OriginConsumer>,
	consume: Option<OriginProducer>,
	// TODO: Uncomment when observability feature is merged
	// stats: Option<Arc<dyn crate::Stats>>,
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

	// TODO: Uncomment when observability feature is merged
	// pub fn with_stats(mut self, stats: impl Into<Option<Arc<dyn crate::Stats>>>) -> Self {
	// 	self.stats = stats.into();
	// 	self
	// }

	/// Perform the MoQ handshake as a client negotiating the version.
	pub async fn connect<S: web_transport_trait::Session>(&self, session: S) -> Result<Session, Error> {
		if self.publish.is_none() && self.consume.is_none() {
			tracing::warn!("not publishing or consuming anything");
		}

		// If ALPN was used to negotiate the version, use the appropriate encoding.
		// Default to IETF 14 if no ALPN was used and we'll negotiate the version later.
		let (encoding, supported) = match session.protocol() {
			Some(p) if p == ietf::ALPN_15 => (
				Version::Ietf(ietf::Version::Draft15),
				vec![ietf::Version::Draft15.into()],
			),
			Some(p) if p == ietf::ALPN_14 => (
				Version::Ietf(ietf::Version::Draft14),
				vec![ietf::Version::Draft14.into()],
			),
			Some(p) if p == lite::ALPN => (Version::Ietf(ietf::Version::Draft14), NEGOTIATED.to_vec()),
			None => (Version::Ietf(ietf::Version::Draft14), NEGOTIATED.to_vec()),
			Some(p) => return Err(Error::UnknownAlpn(p.to_string())),
		};

		let mut stream = Stream::open(&session, encoding).await?;

		let mut parameters = ietf::Parameters::default();
		parameters.set_varint(ietf::ParameterVarInt::MaxRequestId, u32::MAX as u64);
		parameters.set_bytes(ietf::ParameterBytes::Implementation, b"moq-lite-rs".to_vec());
		let parameters = parameters.encode_bytes(());

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
			.ok_or_else(|| Error::Version(client.versions.clone(), supported.clone().into()))?;

		match version {
			Version::Lite(version) => {
				let stream = stream.with_version(version);
				lite::start(
					session.clone(),
					stream,
					self.publish.clone(),
					self.consume.clone(),
					version,
				)
				.await?;
			}
			Version::Ietf(version) => {
				// Decode the parameters to get the initial request ID.
				let parameters = ietf::Parameters::decode(&mut server.parameters, version)?;
				let request_id_max = ietf::RequestId(
					parameters
						.get_varint(ietf::ParameterVarInt::MaxRequestId)
						.unwrap_or_default(),
				);

				let stream = stream.with_version(version);
				ietf::start(
					session.clone(),
					stream,
					request_id_max,
					true,
					self.publish.clone(),
					self.consume.clone(),
					version,
				)
				.await?;
			}
		}

		tracing::debug!(version = ?server.version, "connected");

		Ok(Session::new(session))
	}
}
