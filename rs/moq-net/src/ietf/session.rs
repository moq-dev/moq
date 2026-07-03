use crate::origin;
use crate::{
	Error, Origin, StatsHandle,
	coding::{Decode, Encode, Reader, Stream, Writer},
	ietf::{self, FetchHeader, RequestId},
	setup,
};

use super::{Control, Message, Publisher, Subscriber, Version, adapter::ControlStreamAdapter};

// Handshake dispatcher: each argument is an independent session parameter, so
// bundling them into a config struct would just add indirection.
#[allow(clippy::too_many_arguments)]
pub fn start<S: web_transport_trait::Session>(
	session: S,
	setup: Option<Stream<S, Version>>,
	request_id_max: Option<RequestId>,
	client: bool,
	publish: Option<origin::Consumer>,
	subscribe: Option<origin::Producer>,
	// Tier-scoped stats handle. Pass [`StatsHandle::default`] to opt out.
	stats: StatsHandle,
	version: Version,
	// The request path we advertise in our SETUP (draft-17+ clients on URL-less
	// transports). A server passes `None`.
	path: Option<String>,
	// The peer's SETUP stream, when it was already read before `start` (a draft-17+
	// server that gated on the client's path via [`accept_setup`]). It becomes the
	// GOAWAY channel; `None` lets the uni loop read the SETUP itself.
	peer_setup: Option<Reader<S::RecvStream, crate::Version>>,
) -> Result<(), Error> {
	web_async::spawn(async move {
		// moq-transport threads concrete origins through the publisher/subscriber.
		// An unset half gets an empty origin: an empty publish origin announces
		// nothing, and an empty subscribe origin issues no SUBSCRIBE_NAMESPACE.
		let publish = publish.unwrap_or_else(|| origin::Producer::empty(Origin::random()).consume());
		let subscribe = subscribe.unwrap_or_else(|| origin::Producer::empty(Origin::random()));
		let res = match version {
			Version::Draft14 | Version::Draft15 | Version::Draft16 => {
				let Some(setup) = setup else {
					return session.close(Error::ProtocolViolation.to_code(), "setup stream required");
				};
				let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
				let control = Control::new(request_id_max, client);
				let adapter = ControlStreamAdapter::new(session.clone(), tx, control.clone(), version);

				let publisher = Publisher::new(adapter.clone(), publish, control.clone(), stats.clone(), version);
				let subscriber = Subscriber::new(adapter.clone(), subscribe, control, stats, version);

				let dispatch_session = adapter.clone();
				let mut sub_ns = subscriber.clone();
				let sub_ns_adapter = adapter.clone();

				tokio::select! {
									Err(err) = adapter.run(setup.reader, setup.writer, rx) => Err::<(), Error>(err),
									Err(err) = run_unis(adapter.clone(), subscriber.clone(), version) => Err(err),
									Err(err) = run_dispatch(dispatch_session, publisher.clone(), subscriber.clone(), version) => Err(err),
									Err(err) = publisher.run() => Err(err),
									Err(err) = async {
				let stream = match version {
											Version::Draft16 => {
												let (send, recv) = sub_ns_adapter.open_native_bi().await?;
												Stream {
													writer: crate::coding::Writer::new(send, version),
													reader: crate::coding::Reader::new(recv, version),
												}
											}
											_ => Stream::open(&sub_ns_adapter, version).await?,
										};
										if let Err(err) = sub_ns.run_subscribe_namespace(stream).await {
										tracing::warn!(%err, "subscribe_namespace failed, continuing without");
									}
									Ok(())
									} => Err(err),
								}
			}
			_ => {
				// Spawn SETUP sender (keeps stream alive for GOAWAY).
				web_async::spawn({
					let session = session.clone();
					async move {
						if let Err(err) = run_setup(session, version, path).await {
							tracing::warn!(%err, "setup send error");
						}
					}
				});

				let control = Control::new(None, client);
				let publisher = Publisher::new(session.clone(), publish, control.clone(), stats.clone(), version);
				let subscriber = Subscriber::new(session.clone(), subscribe, control, stats, version);

				let sub_ns_session = session.clone();
				let mut sub_ns = subscriber.clone();

				// When the peer's SETUP was pre-read (a gated server accept), monitor
				// GOAWAY on that stream here; otherwise `run_unis` does it when the SETUP
				// arrives on the wire.
				let goaway = async move {
					match peer_setup {
						Some(reader) => run_goaway(reader.with_version(version), version).await,
						None => std::future::pending().await,
					}
				};

				tokio::select! {
									Err(err) = run_unis(session.clone(), subscriber.clone(), version) => Err(err),
									Err(err) = run_dispatch(session.clone(), publisher.clone(), subscriber.clone(), version) => Err(err),
									Err(err) = publisher.run() => Err(err),
									Err(err) = goaway => Err(err),
									Err(err) = async {
				let stream = Stream::open(&sub_ns_session, version).await?;
										if let Err(err) = sub_ns.run_subscribe_namespace(stream).await {
											tracing::warn!(%err, "subscribe_namespace failed, continuing without");
										}
										Ok(())
									} => Err(err),
								}
			}
		};

		match res {
			Err(Error::Transport(_)) => {
				tracing::info!("session terminated");
				session.close(1, "");
			}
			Err(err) => {
				tracing::warn!(%err, "session error");
				session.close(err.to_code(), err.to_string().as_ref());
			}
			_ => {
				tracing::info!("session closed");
				session.close(0, "");
			}
		}
	});

	Ok(())
}

/// Server (draft-17+): read the peer's SETUP off its uni stream before starting the
/// session, returning that stream (it becomes the GOAWAY channel) and the request
/// path the peer advertised.
///
/// Blocks on the peer's Setup Stream; any other uni stream racing ahead of it is
/// `STOP_SENDING`-ed and skipped (group data needs a prior subscribe, so nothing
/// legitimate precedes the SETUP at connect). Pass the returned reader to [`start`]
/// as its `peer_setup` so GOAWAY monitoring continues without re-reading it.
pub async fn accept_setup<S: web_transport_trait::Session>(
	session: &S,
	version: Version,
) -> Result<(Reader<S::RecvStream, crate::Version>, Option<String>), Error> {
	let outer_version = crate::Version::Ietf(version);

	loop {
		let recv = session.accept_uni().await.map_err(Error::from_transport)?;
		let mut reader: Reader<S::RecvStream, crate::Version> = Reader::new(recv, outer_version);

		if reader.decode_peek::<u64>().await? != setup::SETUP_V17 {
			// Not the SETUP (group data this early is unexpected). Reject and keep waiting.
			reader.abort(&Error::UnexpectedStream);
			continue;
		}

		let setup: setup::Setup = reader.decode().await?;
		let mut bytes = setup.parameters;
		let path = match ietf::Parameters::decode(&mut bytes, version)?.get_bytes(ietf::ParameterBytes::Path) {
			Some(bytes) => Some(
				std::str::from_utf8(bytes)
					.map_err(|_| Error::Decode(crate::DecodeError::InvalidValue))?
					.to_owned(),
			),
			None => None,
		};

		return Ok((reader, path));
	}
}

/// Send our SETUP on a uni stream and keep it alive for potential GOAWAY.
///
/// `path` is the request path we advertise (clients on URL-less transports); a
/// server passes `None`.
async fn run_setup<S: web_transport_trait::Session>(
	session: S,
	version: Version,
	path: Option<String>,
) -> Result<(), Error> {
	let outer_version = crate::Version::Ietf(version);

	let send = session.open_uni().await.map_err(Error::from_transport)?;
	let mut writer: Writer<S::SendStream, crate::Version> = Writer::new(send, outer_version);

	let mut parameters = ietf::Parameters::default();
	parameters.set_bytes(ietf::ParameterBytes::Implementation, b"moq-lite-rs".to_vec());
	if let Some(path) = path {
		parameters.set_bytes(ietf::ParameterBytes::Path, path.into_bytes());
	}
	let parameters = parameters.encode_bytes(version)?;

	writer.encode(&setup::Setup { parameters }).await?;

	// Hold the writer alive until the session closes.
	session.closed().await;
	writer.finish().ok();

	Ok(())
}

/// Accept incoming uni streams and dispatch each to a handler.
///
/// For v17, this also handles the SETUP stream (0x2F00) and GOAWAY.
/// For v14-16, all uni streams are group data.
async fn run_unis<S: web_transport_trait::Session>(
	session: S,
	subscriber: Subscriber<S>,
	version: Version,
) -> Result<(), Error> {
	let outer_version = crate::Version::Ietf(version);

	loop {
		let recv = session.accept_uni().await.map_err(Error::from_transport)?;
		let mut reader: Reader<S::RecvStream, crate::Version> = Reader::new(recv, outer_version);
		let kind: u64 = reader.decode_peek().await?;

		// v17+: SETUP arrives on a uni stream, then becomes the GOAWAY channel.
		// We accept it in the background without blocking, since there are no
		// extensions that require waiting on the SETUP before proceeding.
		if kind == setup::SETUP_V17 {
			web_async::spawn(async move {
				// Decode and discard the unified SETUP message.
				if let Err(err) = reader.decode::<setup::Setup>().await {
					tracing::warn!(%err, "setup decode error");
					return;
				}

				// Monitor for GOAWAY after setup completes.
				if let Err(err) = run_goaway(reader.with_version(version), version).await {
					tracing::warn!(%err, "goaway error");
				}
			});

			continue;
		}

		// Group data — spawn a handler for each stream.
		let mut sub = subscriber.clone();
		web_async::spawn(async move {
			let mut reader = reader.with_version(version);
			if let Err(err) = run_uni_group(&mut sub, &mut reader).await {
				tracing::debug!(%err, "uni stream error");
				reader.abort(&err);
			}
		});
	}
}

async fn run_uni_group<S: web_transport_trait::Session>(
	subscriber: &mut Subscriber<S>,
	stream: &mut Reader<S::RecvStream, Version>,
) -> Result<(), Error> {
	let kind: u64 = stream.decode_peek().await?;

	// SUBGROUP_HEADER type bytes match the form 0b0XX1XXXX (spec §11.4.2):
	// draft-14-17 use 0x10-0x1D and 0x30-0x3D, draft-18 adds 0x40 (FIRST_OBJECT)
	// extending the form to also cover 0x50-0x5D and 0x70-0x7D. Per-version and
	// per-bit validation (e.g., FIRST_OBJECT must be 0 on draft-17) is done in
	// `GroupFlags::decode`.
	if kind <= 0xff && (kind & 0x90) == 0x10 {
		return subscriber.recv_group(stream).await;
	}

	match kind {
		FetchHeader::TYPE => Err(Error::Unsupported),
		_ => Err(Error::UnexpectedStream),
	}
}

/// Accept incoming bidi streams and dispatch to the correct handler based on message type.
async fn run_dispatch<S: web_transport_trait::Session>(
	session: S,
	publisher: Publisher<S>,
	mut subscriber: Subscriber<S>,
	version: Version,
) -> Result<(), Error> {
	loop {
		let mut stream = Stream::accept(&session, version).await?;

		let id: u64 = stream.reader.decode().await?;
		let size: u16 = stream.reader.decode().await?;
		let data = stream.reader.read_exact(size as usize).await?;

		match id {
			// Publisher handles: Subscribe, Fetch, SubscribeNamespace (0x50 modern /
			// 0x11 legacy), TrackStatus
			ietf::Subscribe::ID
			| ietf::Fetch::ID
			| ietf::SubscribeNamespace::ID
			| ietf::SubscribeNamespaceLegacy::ID
			| ietf::TrackStatus::ID => {
				publisher.handle_stream(id, data, stream)?;
			}
			// Subscriber handles: Publish, PublishNamespace
			ietf::Publish::ID | ietf::PublishNamespace::ID => {
				subscriber.handle_stream(id, data, stream)?;
			}
			_ => {
				tracing::warn!(id, "unexpected bidi stream type");
				return Err(Error::UnexpectedStream);
			}
		}
	}
}

/// Block until GOAWAY or stream close.
async fn run_goaway<R: web_transport_trait::RecvStream>(
	mut reader: Reader<R, Version>,
	version: Version,
) -> Result<(), Error> {
	let id: u64 = match reader.decode_maybe().await? {
		Some(id) => id,
		None => return Ok(()),
	};

	let size: u16 = reader.decode::<u16>().await?;
	let mut data = reader.read_exact(size as usize).await?;

	if id == ietf::GoAway::ID {
		let msg = ietf::GoAway::decode_msg(&mut data, version)?;
		tracing::debug!(message = ?msg, "received GOAWAY");
		Err(Error::Unsupported)
	} else {
		Err(Error::UnexpectedMessage)
	}
}
