// TODO: Uncomment when observability feature is merged
// use std::sync::Arc;

use crate::{
	Error, OriginConsumer, OriginProducer, Session, VERSIONS,
	coding::{Decode, Encode, Stream},
	ietf, lite, setup,
};

/// A MoQ server session builder.
#[derive(Default, Clone)]
pub struct Server {
	publish: Option<OriginConsumer>,
	consume: Option<OriginProducer>,
	// TODO: Uncomment when observability feature is merged
	// stats: Option<Arc<dyn crate::Stats>>,
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

	// TODO: Uncomment when observability feature is merged
	// pub fn with_stats(mut self, stats: impl Into<Option<Arc<dyn crate::Stats>>>) -> Self {
	// 	self.stats = stats.into();
	// 	self
	// }

	/// Perform the MoQ handshake as a server for the given session.
	pub async fn accept<S: web_transport_trait::Session>(&self, session: S) -> Result<Session, Error> {
		if self.publish.is_none() && self.consume.is_none() {
			tracing::warn!("not publishing or consuming anything");
		}

		// Accept with an initial version; we'll switch to the negotiated version later
		let mut stream = Stream::accept(&session, ()).await?;
		let mut client: setup::Client = stream.reader.decode().await?;
		tracing::trace!(?client, "received client setup");

		// Choose the version to use
		let version = client
			.versions
			.iter()
			.find(|v| VERSIONS.contains(v))
			.copied()
			.ok_or(Error::Version)?;

		// Only encode parameters if we're using the IETF draft because it has max_request_id
		let parameters = if ietf::Version::try_from(version).is_ok() && client.kind == setup::ClientKind::Ietf14 {
			let mut parameters = ietf::Parameters::default();
			parameters.set_varint(ietf::ParameterVarInt::MaxRequestId, u32::MAX as u64);
			parameters.set_bytes(ietf::ParameterBytes::Implementation, b"moq-lite-rs".to_vec());
			parameters.encode_bytes(())
		} else {
			lite::Parameters::default().encode_bytes(())
		};

		let server = setup::Server { version, parameters };
		tracing::trace!(?server, "sending server setup");

		let mut stream = stream.with_version(client.kind.reply());
		stream.writer.encode(&server).await?;

		if let Ok(version) = lite::Version::try_from(version) {
			let stream = stream.with_version(version);
			lite::start(
				session.clone(),
				stream,
				self.publish.clone(),
				self.consume.clone(),
				version,
			)
			.await?;
		} else if let Ok(version) = ietf::Version::try_from(version) {
			// Decode the client's parameters to get their max request ID.
			let parameters = ietf::Parameters::decode(&mut client.parameters, version)?;
			let request_id_max =
				ietf::RequestId(parameters.get_varint(ietf::ParameterVarInt::MaxRequestId).unwrap_or(0));

			let stream = stream.with_version(version);
			ietf::start(
				session.clone(),
				stream,
				request_id_max,
				false,
				self.publish.clone(),
				self.consume.clone(),
				version,
			)
			.await?;
		} else {
			// unreachable, but just in case
			return Err(Error::Version);
		}

		tracing::debug!(?version, "connected");

		Ok(Session::new(session))
	}
}
