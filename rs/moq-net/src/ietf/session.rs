use crate::origin;
use crate::{
	Error, Origin,
	coding::{Decode, Encode, Reader, Stream, Writer},
	ietf::{self, FetchHeader, RequestId},
	setup,
	util::{MaybeBoxedExt, MaybeSendBox, TaskSet, err_only},
};

use super::{Control, Message, Publisher, Subscriber, Version, adapter::ControlStreamAdapter, cluster};

/// Everything one moq-transport session needs to start.
pub struct Config<S: web_transport_trait::Session> {
	pub session: S,

	/// The bidi SETUP stream (draft-14 through draft-16 only). Draft-17+ passes `None`
	/// and exchanges SETUP on uni streams instead.
	pub setup: Option<Stream<S, Version>>,

	pub request_id_max: Option<RequestId>,

	/// Whether we dialed. Only the dialing side prices the link (see [`Self::cost`]).
	pub client: bool,

	/// Traffic stats are attributed through these origin handles: tag them with
	/// `origin::{Consumer, Producer}::with_stats` before calling [`start`].
	pub publish: Option<origin::Consumer>,
	pub subscribe: Option<origin::Producer>,

	/// The origin (hop) id to assign the peer when it declares none itself. See
	/// `Client::with_peer_origin`; a peer that negotiates the MoQ Cluster extension
	/// declares its own, which wins.
	pub peer_origin: Option<Origin>,

	/// What crossing this link costs, declared in our SETUP (see
	/// [`cluster::RELAY_COST`]). Client-only: `None` on the accepting side, and on a
	/// dialer that priced nothing.
	pub cost: Option<u64>,

	pub version: Version,

	/// The request path we advertise in our SETUP (draft-17+ clients on URL-less
	/// transports). A server passes `None`.
	pub path: Option<String>,

	/// The peer's SETUP stream, when it was already read before [`start`] (a draft-17+
	/// server that gated on the client's path via [`accept_setup`]). It becomes the
	/// GOAWAY channel; `None` lets the uni loop read the SETUP itself.
	pub peer_setup_stream: Option<Reader<S::RecvStream, crate::Version>>,

	/// What that pre-read SETUP declared, so the session does not have to parse it
	/// twice. `None` when [`Self::peer_setup_stream`] is.
	pub peer_cluster: Option<cluster::Peer>,
}

pub fn start<S: web_transport_trait::Session>(
	config: Config<S>,
) -> Result<MaybeSendBox<'static, Result<(), Error>>, Error> {
	let Config {
		session,
		setup,
		request_id_max,
		client,
		publish,
		subscribe,
		peer_origin,
		cost,
		version,
		path,
		peer_setup_stream,
		peer_cluster,
	} = config;

	let driver = async move {
		// moq-transport threads concrete origins through the publisher/subscriber.
		// An unset half gets an empty origin: an empty publish origin announces
		// nothing, and an empty subscribe origin issues no SUBSCRIBE_NAMESPACE.
		let publish = publish.unwrap_or_else(|| origin::Producer::empty(Origin::random()).consume());
		let subscribe = subscribe.unwrap_or_else(|| origin::Producer::empty(Origin::random()));

		// Our own Hop ID: the identity of the origin we publish from, so every session
		// out of this process stamps the same one and cross-session loop detection works.
		let self_origin = *publish;

		// The peer's cluster options. Seeded now when its SETUP was already read
		// (a gated server accept), and filled by the uni loop otherwise. A version that
		// cannot negotiate the extension is settled immediately so nothing blocks on it.
		let peer_setup = cluster::PeerSetup::default();
		let setup_read = peer_cluster.is_some();
		match peer_cluster {
			Some(peer) => peer_setup.set(peer),
			None if !cluster::supported(version) => peer_setup.set(cluster::Peer::default()),
			None => {}
		}

		let res = match version {
			Version::Draft14 | Version::Draft15 | Version::Draft16 => {
				let Some(setup) = setup else {
					let err = Error::ProtocolViolation;
					session.close(err.to_code(), "setup stream required");
					return Err(err);
				};
				let control = Control::new(request_id_max, client);
				let adapter = ControlStreamAdapter::new(session.clone(), control.clone(), version);

				let publisher = Publisher::new(
					adapter.clone(),
					publish,
					control.clone(),
					peer_origin,
					peer_setup.clone(),
					version,
				);
				let (tasks, mut task_set) = TaskSet::new();
				let subscriber = Subscriber::new(
					adapter.clone(),
					subscribe,
					control,
					peer_origin,
					peer_setup.clone(),
					self_origin,
					cost,
					version,
					tasks,
				);

				let dispatch_session = adapter.clone();
				let sub_ns = subscriber.clone();
				let sub_ns_adapter = adapter.clone();

				// Every half only ends the session on error (err_only parks on clean
				// completion); the task set draining is the one clean exit.
				let mut adapter_run = std::pin::pin!(err_only(adapter.run(setup.reader, setup.writer)));
				let mut unis = std::pin::pin!(err_only(run_unis(
					adapter.clone(),
					subscriber.clone(),
					None,
					false,
					version
				)));
				let mut dispatch = std::pin::pin!(err_only(run_dispatch(
					dispatch_session,
					publisher.clone(),
					subscriber.clone(),
					version
				)));
				// One SUBSCRIBE_NAMESPACE per permitted prefix, like `lite::Subscriber`:
				// the scope is what we may ask for, and it is not the origin's root.
				let mut sub_ns_run = std::pin::pin!(err_only(async {
					let mut prefixes = futures::stream::FuturesUnordered::new();
					for prefix in sub_ns.subscribe_prefixes() {
						let mut sub_ns = sub_ns.clone();
						let sub_ns_adapter = sub_ns_adapter.clone();
						prefixes.push(async move {
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
							if let Err(err) = sub_ns.run_subscribe_namespace(stream, prefix).await {
								tracing::warn!(%err, "subscribe_namespace failed, continuing without");
							}
							Ok::<(), Error>(())
						});
					}
					while let Some(result) = futures::StreamExt::next(&mut prefixes).await {
						result?;
					}
					Ok(())
				}));

				kio::wait(|waiter| {
					use std::task::Poll;
					if let Poll::Ready(err) = waiter.poll_future(adapter_run.as_mut()) {
						return Poll::Ready(Err::<(), Error>(err));
					}
					if let Poll::Ready(err) = waiter.poll_future(unis.as_mut()) {
						return Poll::Ready(Err(err));
					}
					if let Poll::Ready(err) = waiter.poll_future(dispatch.as_mut()) {
						return Poll::Ready(Err(err));
					}
					if task_set.poll(waiter).is_ready() {
						return Poll::Ready(Ok(()));
					}
					if let Poll::Ready(err) = waiter.poll_future(sub_ns_run.as_mut()) {
						return Poll::Ready(Err(err));
					}
					Poll::Pending
				})
				.await
			}
			_ => {
				// Send SETUP and keep the stream alive for GOAWAY.
				let setup = {
					let session = session.clone();
					async move {
						if let Err(err) = run_setup(session, version, path, self_origin, cost).await {
							tracing::warn!(%err, "setup send error");
						}
						std::future::pending::<()>().await;
					}
				};

				let control = Control::new(None, client);
				let publisher = Publisher::new(
					session.clone(),
					publish,
					control.clone(),
					peer_origin,
					peer_setup.clone(),
					version,
				);
				let (tasks, mut task_set) = TaskSet::new();
				let subscriber = Subscriber::new(
					session.clone(),
					subscribe,
					control,
					peer_origin,
					peer_setup.clone(),
					self_origin,
					cost,
					version,
					tasks,
				);

				let sub_ns_session = session.clone();
				let sub_ns = subscriber.clone();

				// When the peer's SETUP was pre-read (a gated server accept), monitor
				// GOAWAY on that stream here; otherwise `run_unis` does it when the SETUP
				// arrives on the wire.
				let goaway = async move {
					match peer_setup_stream {
						Some(reader) => run_goaway(reader.with_version(version), version).await,
						None => std::future::pending().await,
					}
				};

				// Every half only ends the session on error (err_only parks on clean
				// completion); `setup` never resolves (it holds the stream open) and the
				// task set draining is the one clean exit.
				let mut unis = std::pin::pin!(err_only(run_unis(
					session.clone(),
					subscriber.clone(),
					Some(peer_setup.clone()),
					setup_read,
					version
				)));
				let mut dispatch = std::pin::pin!(err_only(run_dispatch(
					session.clone(),
					publisher.clone(),
					subscriber.clone(),
					version
				)));
				let mut goaway = std::pin::pin!(err_only(goaway));
				let mut setup = std::pin::pin!(setup);
				// One SUBSCRIBE_NAMESPACE per permitted prefix; see the draft-16 arm.
				let mut sub_ns_run = std::pin::pin!(err_only(async {
					let mut prefixes = futures::stream::FuturesUnordered::new();
					for prefix in sub_ns.subscribe_prefixes() {
						let mut sub_ns = sub_ns.clone();
						let sub_ns_session = sub_ns_session.clone();
						prefixes.push(async move {
							let stream = Stream::open(&sub_ns_session, version).await?;
							if let Err(err) = sub_ns.run_subscribe_namespace(stream, prefix).await {
								tracing::warn!(%err, "subscribe_namespace failed, continuing without");
							}
							Ok::<(), Error>(())
						});
					}
					while let Some(result) = futures::StreamExt::next(&mut prefixes).await {
						result?;
					}
					Ok(())
				}));

				kio::wait(|waiter| {
					use std::task::Poll;
					if let Poll::Ready(err) = waiter.poll_future(unis.as_mut()) {
						return Poll::Ready(Err::<(), Error>(err));
					}
					if let Poll::Ready(err) = waiter.poll_future(dispatch.as_mut()) {
						return Poll::Ready(Err(err));
					}
					if let Poll::Ready(err) = waiter.poll_future(goaway.as_mut()) {
						return Poll::Ready(Err(err));
					}
					if waiter.poll_future(setup.as_mut()).is_ready() {
						return Poll::Ready(Ok(()));
					}
					if task_set.poll(waiter).is_ready() {
						return Poll::Ready(Ok(()));
					}
					if let Poll::Ready(err) = waiter.poll_future(sub_ns_run.as_mut()) {
						return Poll::Ready(Err(err));
					}
					Poll::Pending
				})
				.await
			}
		};

		match &res {
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

		res
	}
	.maybe_boxed();

	Ok(driver)
}

/// What a peer's SETUP told us, beyond the stream it arrived on.
pub struct PeerSetup<S: web_transport_trait::Session> {
	/// The SETUP stream, which becomes the GOAWAY channel.
	pub stream: Reader<S::RecvStream, crate::Version>,

	/// The request path the peer advertised, for URL-less transports.
	pub path: Option<String>,

	/// The MoQ Cluster options it declared (see [`cluster`]).
	pub cluster: cluster::Peer,
}

/// Server (draft-17+): read the peer's SETUP off its uni stream before starting the
/// session, returning that stream plus what it declared.
///
/// Blocks on the peer's Setup Stream; any other uni stream racing ahead of it is
/// `STOP_SENDING`-ed and skipped (group data needs a prior subscribe, so nothing
/// legitimate precedes the SETUP at connect). Pass the returned reader to [`start`]
/// as its `peer_setup_stream` so GOAWAY monitoring continues without re-reading it.
pub async fn accept_setup<S: web_transport_trait::Session>(
	session: &S,
	version: Version,
) -> Result<PeerSetup<S>, Error> {
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
		let mut bytes = setup.parameters.clone();
		let params = ietf::Parameters::decode(&mut bytes, version)?;
		let path = match params.get_bytes(ietf::ParameterBytes::Path) {
			Some(bytes) => Some(
				std::str::from_utf8(bytes)
					.map_err(|_| Error::Decode(crate::DecodeError::InvalidValue))?
					.to_owned(),
			),
			None => None,
		};
		let cluster = cluster::peer_from_setup(&params, version)?;

		return Ok(PeerSetup {
			stream: reader,
			path,
			cluster,
		});
	}
}

/// Parse the MoQ Cluster options out of a raw SETUP parameter block.
fn decode_peer_cluster(parameters: bytes::Bytes, version: Version) -> Result<cluster::Peer, crate::DecodeError> {
	let mut bytes = parameters;
	let params = ietf::Parameters::decode(&mut bytes, version)?;
	cluster::peer_from_setup(&params, version)
}

/// Send our SETUP on a uni stream and keep it alive for potential GOAWAY.
///
/// `path` is the request path we advertise (clients on URL-less transports); a
/// server passes `None`. `self_origin` and `cost` are the MoQ Cluster options, which
/// declare our identity and (client-only) what this link costs to cross.
async fn run_setup<S: web_transport_trait::Session>(
	session: S,
	version: Version,
	path: Option<String>,
	self_origin: Origin,
	cost: Option<u64>,
) -> Result<(), Error> {
	let outer_version = crate::Version::Ietf(version);

	let send = session.open_uni().await.map_err(Error::from_transport)?;
	let mut writer: Writer<S::SendStream, crate::Version> = Writer::new(send, outer_version);

	let mut parameters = ietf::Parameters::default();
	parameters.set_bytes(ietf::ParameterBytes::Implementation, b"moq-lite-rs".to_vec());
	if let Some(path) = path {
		parameters.set_bytes(ietf::ParameterBytes::Path, path.into_bytes());
	}
	cluster::peer_into_setup(&mut parameters, self_origin, cost, version);
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
	// Where to record the peer's MoQ Cluster options once its SETUP arrives. `None`
	// for draft-14..16, whose SETUP rides the control stream instead.
	peer_setup: Option<cluster::PeerSetup>,
	// Whether the peer's SETUP was already consumed before this loop started.
	setup_read: bool,
	version: Version,
) -> Result<(), Error> {
	let outer_version = crate::Version::Ietf(version);
	let mut tasks = TaskSet::owned();
	// A gated server accept already read the peer's one SETUP off its own uni stream,
	// so anything arriving here is a second one.
	let mut seen_setup = setup_read;

	loop {
		let recv = tasks.drive(session.accept_uni()).await.map_err(Error::from_transport)?;
		let mut reader: Reader<S::RecvStream, crate::Version> = Reader::new(recv, outer_version);
		let kind: u64 = tasks.drive(reader.decode_peek()).await?;

		// v17+: SETUP arrives on a uni stream, then becomes the GOAWAY channel.
		// We accept it in the background without blocking; the one thing that does
		// need it (the MoQ Cluster negotiation) waits on `peer_setup` instead, so a
		// slow SETUP delays announcements rather than the whole session.
		if kind == setup::SETUP_V17 {
			// Exactly one SETUP per endpoint. A second would let a peer restate its
			// declared identity mid-session, silently re-attributing every route
			// already built from the first.
			if std::mem::replace(&mut seen_setup, true) {
				return Err(Error::ProtocolViolation);
			}

			let peer_setup = peer_setup.clone();
			let session = session.clone();
			tasks.push(async move {
				// The negotiation gates the announce and dispatch loops, so a SETUP we
				// cannot read must end the session rather than leave them parked on a
				// slot nothing will ever fill.
				let msg = match reader.decode::<setup::Setup>().await {
					Ok(msg) => msg,
					Err(err) => {
						tracing::warn!(%err, "setup decode error");
						session.close(Error::ProtocolViolation.to_code(), "invalid setup");
						return;
					}
				};

				if let Some(peer_setup) = peer_setup {
					let peer = match decode_peer_cluster(msg.parameters, version) {
						Ok(peer) => peer,
						Err(err) => {
							tracing::warn!(%err, "setup parameter decode error");
							session.close(Error::ProtocolViolation.to_code(), "invalid setup parameters");
							return;
						}
					};
					peer_setup.set(peer);
				}

				// Monitor for GOAWAY after setup completes.
				if let Err(err) = run_goaway(reader.with_version(version), version).await {
					tracing::warn!(%err, "goaway error");
				}
			});

			continue;
		}

		// Poll one child handler for each group stream.
		let mut sub = subscriber.clone();
		tasks.push(async move {
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
	// PUBLISH_NAMESPACE decodes differently once the MoQ Cluster extension is
	// negotiated, so the whole dispatch loop waits for the peer's SETUP first. The peer
	// must send it before anything else, and `run_unis` reads it independently, so this
	// costs a handshake round rather than blocking.
	let peer = subscriber.peer().await;

	let mut tasks = TaskSet::owned();
	loop {
		let mut stream = tasks.drive(Stream::accept(&session, version)).await?;

		let header = tasks
			.drive(async {
				let id: u64 = stream.reader.decode().await?;
				let size: u16 = stream.reader.decode().await?;
				let data = stream.reader.read_exact(size as usize).await?;
				Ok::<_, Error>((id, data))
			})
			.await;
		let (id, data) = header?;

		match id {
			// Publisher handles: Subscribe, Fetch, SubscribeNamespace (0x50 modern /
			// 0x11 legacy), TrackStatus
			ietf::Subscribe::ID
			| ietf::Fetch::ID
			| ietf::SubscribeNamespace::ID
			| ietf::SubscribeNamespaceLegacy::ID
			| ietf::TrackStatus::ID => {
				tasks.push(publisher.handle_stream(id, data, stream)?);
			}
			// Subscriber handles: Publish, PublishNamespace
			ietf::Publish::ID | ietf::PublishNamespace::ID => {
				tasks.push(subscriber.handle_stream(id, data, stream, peer)?);
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

#[cfg(test)]
mod tests {
	use super::*;

	fn occurrences(log: &crate::lite::test_transport::Log, needle: &[u8]) -> usize {
		let writes = log.writes.lock().unwrap();
		writes.windows(needle.len()).filter(|window| *window == needle).count()
	}

	/// A subscriber issues one SUBSCRIBE_NAMESPACE per PERMITTED PREFIX, and asks for
	/// those prefixes rather than the root it mounts replies under. Driven through
	/// `start` so the per-prefix fan-out is exercised, not just one stream in
	/// isolation: a loop that opened a single stream would still satisfy a test that
	/// called `run_subscribe_namespace` itself.
	#[tokio::test]
	async fn every_permitted_prefix_gets_its_own_subscribe_namespace() {
		let origin = crate::origin::Info::new(crate::Origin::new(1).unwrap()).produce();
		let scoped = origin
			.with_root("rootns")
			.and_then(|rooted| rooted.scope(&[crate::Path::new("cam"), crate::Path::new("mic")]))
			.expect("scope the origin to two prefixes");

		let gate = kio::Producer::new(true);
		let session = crate::lite::test_transport::SinkSession::gated_bi(gate.consume());
		let log = session.log.clone();

		let driver = start(Config {
			session,
			setup: None,
			request_id_max: None,
			client: true,
			publish: None,
			subscribe: Some(scoped),
			peer_origin: None,
			cost: None,
			version: Version::Draft18,
			path: None,
			peer_setup_stream: None,
			peer_cluster: None,
		})
		.expect("start the session");
		let _driver = tokio::spawn(driver);

		// Both requests are written before either peer response, which never comes.
		for _ in 0..100 {
			if occurrences(&log, b"cam") > 0 && occurrences(&log, b"mic") > 0 {
				break;
			}
			tokio::time::sleep(std::time::Duration::from_millis(5)).await;
		}

		assert_eq!(occurrences(&log, b"cam"), 1, "one SUBSCRIBE_NAMESPACE for cam");
		assert_eq!(occurrences(&log, b"mic"), 1, "one SUBSCRIBE_NAMESPACE for mic");
		assert_eq!(occurrences(&log, b"rootns"), 0, "asked the peer for our local root");
	}

	/// A namespace is only advertised in response to a SUBSCRIBE_NAMESPACE. A peer
	/// that never subscribes hears nothing, no matter what we announce locally.
	#[tokio::test]
	async fn announces_wait_for_a_subscribe_namespace() {
		let origin = crate::origin::Info::new(crate::Origin::new(1).unwrap()).produce();
		let _cam = origin
			.create_broadcast("solo-cam", crate::broadcast::Route::new().with_announce(true))
			.unwrap();

		// An open gate: an unsolicited PUBLISH_NAMESPACE would reach the wire.
		let gate = kio::Producer::new(true);
		let session = crate::lite::test_transport::SinkSession::gated_bi(gate.consume());
		let log = session.log.clone();

		let driver = start(Config {
			session,
			setup: None,
			request_id_max: None,
			client: true,
			publish: Some(origin.consume()),
			subscribe: None,
			peer_origin: None,
			cost: None,
			version: Version::Draft18,
			path: None,
			peer_setup_stream: None,
			peer_cluster: None,
		})
		.expect("start the session");
		let _driver = tokio::spawn(driver);

		// Give an unsolicited announce every chance to land before asserting silence.
		tokio::time::sleep(std::time::Duration::from_millis(50)).await;
		assert_eq!(
			occurrences(&log, b"solo-cam"),
			0,
			"PUBLISH_NAMESPACE must wait for a SUBSCRIBE_NAMESPACE"
		);
	}
}
