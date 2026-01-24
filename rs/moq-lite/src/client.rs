// TODO: Uncomment when observability feature is merged
// use std::sync::Arc;

use crate::{
	Error, OriginConsumer, OriginProducer, Session, VERSIONS,
	coding::{Decode, Encode, Stream},
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

		let mut stream = Stream::open(&session, setup::ServerKind::Ietf14).await?;

		let mut parameters = ietf::Parameters::default();
		parameters.set_varint(ietf::ParameterVarInt::MaxRequestId, u32::MAX as u64);
		parameters.set_bytes(ietf::ParameterBytes::Implementation, b"moq-lite-rs".to_vec());
		let parameters = parameters.encode_bytes(());

		let client = setup::Client {
			// Unfortunately, we have to pick a single draft range to support.
			// moq-lite can support this handshake.
			kind: setup::ClientKind::Ietf14,
			versions: VERSIONS.into(),
			parameters,
		};

		// TODO pretty print the parameters.
		tracing::trace!(?client, "sending client setup");
		stream.writer.encode(&client).await?;

		let mut server: setup::Server = stream.reader.decode().await?;
		tracing::trace!(?server, "received server setup");

		if let Ok(version) = lite::Version::try_from(server.version) {
			let stream = stream.with_version(version);
			lite::start(
				session.clone(),
				stream,
				self.publish.clone(),
				self.consume.clone(),
				version,
			)
			.await?;
		} else if let Ok(version) = ietf::Version::try_from(server.version) {
			// Decode the parameters to get the initial request ID.
			let parameters = ietf::Parameters::decode(&mut server.parameters, version)?;
			let request_id_max =
				ietf::RequestId(parameters.get_varint(ietf::ParameterVarInt::MaxRequestId).unwrap_or(0));

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
		} else {
			// unreachable, but just in case
			return Err(Error::Version(client.versions, [server.version].into()));
		}

		tracing::debug!(version = ?server.version, "connected");

		Ok(Session::new(session))
	}
}
