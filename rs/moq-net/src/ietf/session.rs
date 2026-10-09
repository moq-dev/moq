use crate::origin;
use crate::{
	Error, Hop, SessionError, StreamError,
	coding::{Decode, DecodeError, Encode, Reader, Stream, Writer},
	ietf::{self, FetchHeader, RequestId},
	setup,
	util::{MaybeBoxedExt, MaybeSendBox, TaskSet, err_only},
};

use super::{
	Control, Message, Publisher, Subscriber, Version, active_count,
	adapter::ControlStreamAdapter,
	auth, cluster, hidden, peer, request_update, solicit,
	subscriber::{is_protocol_violation, subscribe_prefixes},
};

/// Everything one moq-transport session needs to start.
pub struct Config<S: crate::transport::poll::Session> {
	/// The runtime that arms the session's timers.
	pub runtime: crate::time::Clock,

	pub session: S,

	/// The bidi SETUP stream (draft-14 through draft-16 only). Draft-17+ passes `None`
	/// and exchanges SETUP on uni streams instead.
	pub setup: Option<Stream<S, Version>>,

	pub request_id_max: Option<RequestId>,

	/// What the peer may make this session hold. On drafts 14 to 16 it also sizes the
	/// MAX_REQUEST_ID window advertised in our SETUP (see [`ietf::initial_max_request_id`]).
	pub limits: crate::session::Limits,

	/// Whether we dialed, which sets the request-id parity.
	pub client: bool,

	/// Traffic stats are attributed through these origin handles: tag them with
	/// `origin::{Consumer, Producer}::with_stats` before calling [`start`].
	pub publish: Option<origin::Consumer>,
	pub subscribe: Option<origin::Producer>,

	/// The origin (hop) id to assign the peer when it declares none itself. See
	/// `Client::with_peer_hop`; a peer that negotiates the MoQ Cluster extension
	/// declares its own, which wins.
	pub peer_hop: Option<Hop>,

	/// What crossing this link costs. Declared in our SETUP (see
	/// [`cluster::RELAY_COST`]) for the peer to charge, and charged locally on what the
	/// peer sends us. `None` declares nothing and charges whatever the peer declared,
	/// falling back to 1; that is what a server accepting a connection passes, since it
	/// has no per-peer configuration of its own.
	///
	/// Only `moqt-17`+ negotiates the MoQ Cluster extension. Earlier drafts carry no
	/// cost at all, so nothing is charged and their routes rank on hop count alone.
	pub cost: Option<u64>,

	pub version: Version,

	/// The request path we advertise in our SETUP (draft-17+ clients on URL-less
	/// transports). A server passes `None`.
	pub path: Option<String>,

	/// The URI authority we advertise in our SETUP, under the same rules as `path`.
	pub authority: Option<String>,

	/// The peer's SETUP stream, when it was already read before [`start`] (a draft-17+
	/// server that gated on the client's path via [`accept_setup`]). It becomes the
	/// GOAWAY channel; `None` lets the uni loop read the SETUP itself.
	pub peer_setup_stream: Option<Reader<S::RecvStream, crate::Version>>,

	/// What that pre-read SETUP declared, so the session does not have to parse it
	/// twice. `None` when [`Self::peer_setup_stream`] is.
	pub peer_declared: Option<peer::Peer>,

	/// The session's auth handle, created before [`start`] so a server can take the
	/// peer's token requests during its handshake. Supports AUTH exactly when the
	/// version can negotiate it and this side offers it; the peer's SETUP decides whether
	/// it does.
	pub auth: crate::auth::Handle,

	/// Uni streams that arrived before that pre-read SETUP (see [`PeerSetup::early`]),
	/// classified by the session before it accepts any more.
	pub early_unis: Vec<Reader<S::RecvStream, crate::Version>>,

	/// The extensions our SETUP offers. Drafts 14-16 already sent it, so there this only
	/// records what it said. MoQ Auth is offered only while [`Self::auth`] supports it too.
	pub extensions: crate::setup::Extensions,
}

pub(crate) struct Driver {
	pub(crate) withdrawal: crate::session::Withdrawal,
	pub(crate) local_close: std::sync::Arc<std::sync::atomic::AtomicBool>,
	// Dispatched SUBSCRIBE and FETCH serves still owing the peer data.
	owed: std::sync::Arc<std::sync::atomic::AtomicUsize>,
	future: MaybeSendBox<'static, Result<(), Error>>,
}

impl Driver {
	/// Whether withdrawals and dispatched serves have reached the peer.
	pub(crate) fn drained(&self) -> bool {
		self.withdrawal.drained() && self.owed.load(std::sync::atomic::Ordering::Relaxed) == 0
	}
}

impl std::future::Future for Driver {
	type Output = Result<(), Error>;
	fn poll(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<Self::Output> {
		self.future.as_mut().poll(cx)
	}
}

pub fn start<S>(config: Config<S>) -> Result<(Driver, crate::goaway::Handle, crate::session::Setup), Error>
where
	S: crate::transport::poll::Boxable,
{
	let Config {
		runtime,
		mut session,
		setup,
		request_id_max,
		limits,
		client,
		publish,
		subscribe,
		peer_hop,
		cost,
		version,
		path,
		authority,
		peer_setup_stream,
		peer_declared,
		auth,
		early_unis,
		extensions,
	} = config;
	let request_token = auth.request_token();

	// GOAWAY wiring: the public Session holds one half (drain trigger, received
	// signal), the protocol tasks below hold the other.
	// A moq-transport client MUST send an empty New Session URI: it cannot tell a
	// server to open connections (draft-19 sect 10.4).
	let (goaway_handle, goaway) = crate::goaway::Handle::new(!client);

	// What the peer's connection credential earns by default, from the caller's real
	// handles before the empty-half defaulting below.
	let peer_grant = auth::peer_grant(publish.as_ref(), subscribe.as_ref());

	// Present the connection's own credential (the empty token) right away, so both
	// sides learn their grant without waiting on the app. Draft-17+ only; the peer's
	// SETUP then decides whether it is ever sent.
	// A handle that already knows the peer declined (a gated server accept) refuses it.
	let setup_token = match auth::supported(version) {
		true => auth.present(bytes::Bytes::new(), true).ok(),
		false => None,
	};

	// One SUBSCRIBE_NAMESPACE per permitted prefix, like `lite::Subscriber`: the
	// scope is what we may ask for, and it is not the origin's root.
	let namespaces = subscribe.as_ref().map(subscribe_prefixes).unwrap_or_default();

	let withdrawal = crate::session::Withdrawal::default();
	let withdrawing = withdrawal.clone();
	let local_close = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
	let closing = local_close.clone();
	let owed = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
	let serving = owed.clone();

	// What the peer declared in its SETUP. Seeded now when that stream was already
	// read (the legacy handshake, or a gated server accept), and filled by the uni
	// loop otherwise.
	let peer_setup = peer::PeerSetup::default();
	let setup_read = peer_declared.is_some();
	match peer_declared {
		Some(declared) => peer_setup.set(declared),
		// A legacy caller that passed nothing (our own tests, and the lite paths):
		// settle the slot rather than leave the announce loops waiting on a value
		// that is never coming.
		None if !cluster::supported(version) => peer_setup.set(peer::Peer::default()),
		None => {}
	}

	// Settled already on drafts 14-16, which read the peer's SETUP in the handshake.
	let setup_seen = crate::session::Setup::Ietf(peer_setup.clone());

	let driver = async move {
		// Held for the life of the session.
		let _setup_token = setup_token;
		// Released on any exit, so nothing waits on a token the session will never answer.
		let _auth_close = AuthClose(auth.clone());
		// Decided once, before any Auth request can be accepted: the app took the requests
		// before running the driver, or the session answers itself.
		let _ = auth.acceptor();
		// Our own Hop ID, taken from whichever origin the caller actually supplied so
		// every session out of this process stamps the same one and cross-session loop
		// detection works. Read BEFORE the placeholders below: their ids are random and
		// identify nothing, so declaring one would compare incoming paths against an
		// identity no other session shares.
		let self_origin = self_origin(publish.as_ref(), subscribe.as_ref());

		// moq-transport threads concrete origins through the publisher/subscriber.
		// An unset half gets an empty origin: an empty publish origin announces
		// nothing, and an empty subscribe origin issues no SUBSCRIBE_NAMESPACE.
		let publish = publish.unwrap_or_else(|| origin::Producer::empty(Hop::random()).consume());
		let subscribe = subscribe.unwrap_or_else(|| origin::Producer::empty(Hop::random()));
		let subscriptions = crate::session::Slots::new(limits.subscriptions)
			.with_stats(publish.stats(), crate::stats::Cap::Subscriptions);
		let announces =
			crate::session::Slots::new(limits.announces).with_stats(subscribe.stats(), crate::stats::Cap::Announces);

		let res = match version {
			Version::Draft14 | Version::Draft15 | Version::Draft16 => {
				let Some(setup) = setup else {
					let err = Error::ProtocolViolation;
					session.close(SessionError::from(&err).to_code(), "setup stream required");
					return Err(err);
				};
				let control = Control::new(request_id_max, client).with_window(limits.requests(), client);
				let adapter = ControlStreamAdapter::new(session.clone(), control.clone(), version);

				// No AUTH on these drafts, but a limit still reaches both halves.
				let mut publisher = Publisher::new(
					runtime.clone(),
					adapter.clone(),
					publish,
					control.clone(),
					peer_hop,
					peer_setup.clone(),
					version,
				)
				.with_auth(auth.clone())
				.with_request_token(request_token.clone());
				let (tasks, mut task_set) = TaskSet::new();
				publisher.withdrawal = withdrawing.clone();
				publisher.owed = serving.clone();
				publisher.subscriptions = subscriptions;

				let mut subscriber = Subscriber::new(
					runtime.clone(),
					adapter.clone(),
					subscribe,
					control,
					peer_hop,
					peer_setup.clone(),
					self_origin,
					cost,
					version,
					tasks.clone(),
					goaway.going_away.clone(),
				)
				.with_auth(auth.clone())
				.with_request_token(request_token.clone())
				.with_solicit(extensions.solicit);
				subscriber.announces = announces;

				// GOAWAY send task: draft-14-16 carry GOAWAY on the shared control
				// stream. Parked on the drain trigger; races the transport close so
				// a parked trigger never blocks the task set draining.
				{
					let mut session = session.clone();
					let adapter = adapter.clone();
					let goaway = goaway.clone();
					let runtime = runtime.clone();
					tasks.push(async move {
						let payload = kio::wait(|waiter| {
							let mut cx = waiter.context();
							if session.poll_closed(&mut cx).is_ready() {
								return std::task::Poll::Ready(None);
							}
							goaway.poll_triggered(waiter)
						})
						.await;
						let Some(payload) = payload else {
							return;
						};
						let timeout_ms = payload.timeout.map(|d| d.as_millis() as u64).unwrap_or(0);
						adapter.send_goaway(&payload.uri, timeout_ms, version);
						crate::goaway::enforce(&runtime, &mut session, payload.timeout).await;
					});
				}
				drop(tasks);

				let dispatch_session = adapter.clone();
				let sub_ns = subscriber.clone();
				let sub_ns_adapter = adapter.clone();

				// Every half only ends the session on error (err_only parks on clean
				// completion); the task set draining is the one clean exit.
				let mut adapter_run = std::pin::pin!(err_only(adapter.run(setup.reader, setup.writer, goaway.clone())));
				let mut unis = std::pin::pin!(err_only(run_unis(
					adapter.clone(),
					subscriber.clone(),
					UniSetup {
						peer: None,
						read: false,
						early: Vec::new(),
						version,
						goaway,
						local_close: closing.clone()
					},
				)));
				let mut dispatch = std::pin::pin!(err_only(run_dispatch(
					dispatch_session,
					publisher.clone(),
					subscriber.clone(),
					peer_setup.clone(),
					None,
					version
				)));
				let mut datagrams = std::pin::pin!(err_only(run_datagrams(adapter.clone(), subscriber.clone())));
				// Unsolicited PUBLISH_NAMESPACE unless the peer requires solicitation;
				// see `Publisher::run_publish_namespaces`.
				let mut pub_ns_run = std::pin::pin!(err_only(publisher.clone().run_publish_namespaces()));
				let mut sub_ns_run = std::pin::pin!(err_only(async {
					let mut prefixes = futures::stream::FuturesUnordered::new();
					for prefix in namespaces {
						let mut sub_ns = sub_ns.clone();
						let sub_ns_adapter = sub_ns_adapter.clone();
						prefixes.push(async move {
							let stream = match version {
								Version::Draft16 => {
									let mut sub_ns_adapter = sub_ns_adapter;
									let (send, recv) = sub_ns_adapter.open_native_bi().await?;
									Stream {
										writer: crate::coding::Writer::new(send, version),
										reader: crate::coding::Reader::new(recv, version),
									}
								}
								_ => Stream::open(&mut sub_ns_adapter.clone(), version).await?,
							};
							if let Err(err) = sub_ns.run_subscribe_namespace(stream, prefix).await {
								// The peer breaking the protocol is fatal, and the driver
								// below turns this into the session close the draft wants.
								if is_protocol_violation(&err) {
									return Err(err);
								}
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

				let res = kio::wait(|waiter| {
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
					if let Poll::Ready(err) = waiter.poll_future(datagrams.as_mut()) {
						return Poll::Ready(Err(err));
					}
					if task_set.poll(waiter).is_ready() {
						return Poll::Ready(Ok(()));
					}
					if let Poll::Ready(err) = waiter.poll_future(sub_ns_run.as_mut()) {
						return Poll::Ready(Err(err));
					}
					if let Poll::Ready(err) = waiter.poll_future(pub_ns_run.as_mut()) {
						return Poll::Ready(Err(err));
					}
					Poll::Pending
				})
				.await;
				if closing.load(std::sync::atomic::Ordering::Relaxed) {
					subscriber.close();
				} else if let Err(err) = &res {
					// Every track this session was receiving ends with its error.
					subscriber.abort(err);
				}
				res
			}
			_ => {
				// Send SETUP and keep the stream alive: it is also our GOAWAY channel.
				let setup = {
					let runtime = runtime.clone();
					let session = session.clone();
					let goaway = goaway.clone();
					let extensions = crate::setup::Extensions {
						auth: extensions.auth && auth.supported(),
						..extensions
					};
					async move {
						if let Err(err) = run_setup(
							runtime,
							session,
							version,
							path,
							authority,
							self_origin,
							cost,
							extensions,
							goaway,
						)
						.await
						{
							tracing::warn!(%err, "setup send error");
						}
						std::future::pending::<()>().await;
					}
				};

				let control = Control::new(None, client);
				// Only the dialing side fails loud on a publication outside its grant: a
				// server's publish origin is everything the peer may read, not what it
				// intends to push.
				let enforce = {
					let auth = auth.clone();
					let origin = publish.clone();
					let session = session.clone();
					let request_token = request_token.clone();
					let peer_setup = peer_setup.clone();
					async move {
						match client {
							true => enforce_grant(auth, origin, session, request_token, peer_setup, version).await,
							false => std::future::pending().await,
						}
					}
				};
				let mut publisher = Publisher::new(
					runtime.clone(),
					session.clone(),
					publish,
					control.clone(),
					peer_hop,
					peer_setup.clone(),
					version,
				)
				.with_auth(auth.clone())
				.with_request_token(request_token.clone());
				let (tasks, mut task_set) = TaskSet::new();
				publisher.withdrawal = withdrawing.clone();
				publisher.owed = serving.clone();
				publisher.subscriptions = subscriptions;

				let mut subscriber = Subscriber::new(
					runtime.clone(),
					session.clone(),
					subscribe,
					control.clone(),
					peer_hop,
					peer_setup.clone(),
					self_origin,
					cost,
					version,
					tasks,
					goaway.going_away.clone(),
				)
				.with_auth(auth.clone())
				.with_request_token(request_token.clone())
				.with_solicit(extensions.solicit);

				// Our tokens, one Auth request each, once the peer's SETUP negotiates it.
				let present = auth::run_present(
					runtime.clone(),
					session.clone(),
					control.clone(),
					auth.clone(),
					peer_setup.clone(),
					version,
					goaway.going_away.clone(),
				);
				// Only a side that offered MoQ Auth answers the peer's Auth requests: one that
				// arrives otherwise falls through to the protocol violation in `run_dispatch`.
				let serve = auth.supported().then(|| auth::Serve {
					runtime: runtime.clone(),
					handle: auth.clone(),
					peer_grant,
				});
				subscriber.announces = announces;

				let sub_ns_session = session.clone();
				let sub_ns = subscriber.clone();

				// When the peer's SETUP was pre-read (a gated server accept), monitor
				// GOAWAY on that stream here; otherwise `run_unis` does it when the SETUP
				// arrives on the wire.
				let goaway_recv = {
					let goaway = goaway.clone();
					async move {
						match peer_setup_stream {
							Some(reader) => run_goaway(reader.with_version(version), version, goaway).await,
							None => std::future::pending().await,
						}
					}
				};

				// Every half only ends the session on error (err_only parks on clean
				// completion); `setup` never resolves (it holds the stream open) and the
				// task set draining is the one clean exit.
				let mut unis = std::pin::pin!(err_only(run_unis(
					session.clone(),
					subscriber.clone(),
					UniSetup {
						peer: Some(peer_setup.clone()),
						read: setup_read,
						early: early_unis,
						version,
						goaway,
						local_close: closing.clone()
					},
				)));
				let mut dispatch = std::pin::pin!(err_only(run_dispatch(
					session.clone(),
					publisher.clone(),
					subscriber.clone(),
					peer_setup.clone(),
					serve,
					version
				)));
				let mut datagrams = std::pin::pin!(err_only(run_datagrams(session.clone(), subscriber.clone())));
				let mut goaway_recv = std::pin::pin!(err_only(goaway_recv));
				let mut present = std::pin::pin!(present);
				let mut enforce = std::pin::pin!(err_only(enforce));
				let mut setup = std::pin::pin!(setup);
				// Unsolicited PUBLISH_NAMESPACE unless the peer requires solicitation;
				// see `Publisher::run_publish_namespaces`.
				let mut pub_ns_run = std::pin::pin!(err_only(publisher.clone().run_publish_namespaces()));
				let mut sub_ns_run = std::pin::pin!(err_only(async {
					let mut prefixes = futures::stream::FuturesUnordered::new();
					for prefix in namespaces {
						let mut sub_ns = sub_ns.clone();
						let sub_ns_session = sub_ns_session.clone();
						prefixes.push(async move {
							let mut sub_ns_session = sub_ns_session;
							let stream = Stream::open(&mut sub_ns_session, version).await?;
							if let Err(err) = sub_ns.run_subscribe_namespace(stream, prefix).await {
								// The peer breaking the protocol is fatal, and the driver
								// below turns this into the session close the draft wants.
								if is_protocol_violation(&err) {
									return Err(err);
								}
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

				let res = kio::wait(|waiter| {
					use std::task::Poll;
					if let Poll::Ready(err) = waiter.poll_future(unis.as_mut()) {
						return Poll::Ready(Err::<(), Error>(err));
					}
					if let Poll::Ready(err) = waiter.poll_future(dispatch.as_mut()) {
						return Poll::Ready(Err(err));
					}
					if let Poll::Ready(err) = waiter.poll_future(datagrams.as_mut()) {
						return Poll::Ready(Err(err));
					}
					if let Poll::Ready(err) = waiter.poll_future(goaway_recv.as_mut()) {
						return Poll::Ready(Err(err));
					}
					// Presenting tokens never ends the session.
					let _ = waiter.poll_future(present.as_mut());
					if let Poll::Ready(err) = waiter.poll_future(enforce.as_mut()) {
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
					if let Poll::Ready(err) = waiter.poll_future(pub_ns_run.as_mut()) {
						return Poll::Ready(Err(err));
					}
					Poll::Pending
				})
				.await;
				// Before this arm's Auth serve tasks drop, so each settles with the
				// session's error rather than a bare cancel.
				auth.close(match &res {
					Ok(()) => Error::Cancel,
					Err(err) => err.clone(),
				});
				if closing.load(std::sync::atomic::Ordering::Relaxed) {
					subscriber.close();
				} else if let Err(err) = &res {
					// Every track this session was receiving ends with its error.
					subscriber.abort(err);
				}
				res
			}
		};

		auth.close(match &res {
			Ok(()) => Error::Cancel,
			Err(err) => err.clone(),
		});

		match &res {
			Err(err @ Error::Transport(_)) => {
				tracing::info!(%err, "session terminated");
				session.close(SessionError::Internal.to_code(), "");
			}
			Err(err) => {
				tracing::warn!(%err, "session error");
				session.close(SessionError::from(err).to_code(), err.to_string().as_ref());
			}
			_ => {
				tracing::info!("session closed");
				session.close(SessionError::Cancel.to_code(), "");
			}
		}

		res
	}
	.maybe_boxed();

	Ok((
		Driver {
			withdrawal,
			local_close,
			owed,
			future: driver,
		},
		goaway_handle,
		setup_seen,
	))
}

/// What a peer's SETUP told us, beyond the stream it arrived on.
pub struct PeerSetup<S: crate::transport::poll::Session> {
	/// The SETUP stream, which becomes the GOAWAY channel.
	pub stream: Reader<S::RecvStream, crate::Version>,

	/// The request path the peer advertised, for URL-less transports.
	pub path: Option<String>,

	/// The credential the peer presented in its `AUTHORIZATION TOKEN` option.
	pub token: Option<crate::setup::Token>,

	/// The Setup Options it declared (see [`cluster`] and [`solicit`]).
	pub declared: peer::Peer,

	/// Uni streams that arrived before the SETUP, type peeked but unread. The drafts say
	/// to buffer early data until the control streams arrive, so the session classifies
	/// these once it starts. QUIC stream credit bounds how many can pile up.
	pub early: Vec<Reader<S::RecvStream, crate::Version>>,
}

/// The Hop ID this session declares and detects loops against.
///
/// Both halves of a session share the process's origin identity, so either one names
/// it; the publish half is just the usual one to be set. A session with neither half
/// has no content to route, so a throwaway id is all it can offer.
fn self_origin(publish: Option<&origin::Consumer>, subscribe: Option<&origin::Producer>) -> Hop {
	publish
		.map(|origin| origin.hop())
		.or_else(|| subscribe.map(|origin| origin.hop()))
		.unwrap_or_else(Hop::random)
}

/// Server (draft-17+): read the peer's SETUP off its uni stream before starting the
/// session, returning that stream plus what it declared.
///
/// Blocks on the peer's Setup Stream. Any other uni stream racing ahead of it (padding,
/// or group data for a subscription the peer already holds) is held in
/// [`PeerSetup::early`] for the session to classify. Pass the returned reader to
/// [`start`] as its `peer_setup_stream` so GOAWAY monitoring continues without
/// re-reading it.
pub async fn accept_setup<S: crate::transport::poll::Session>(
	session: &mut S,
	version: Version,
) -> Result<PeerSetup<S>, Error> {
	let outer_version = crate::Version::Ietf(version);
	let mut early = Vec::new();

	loop {
		let recv = session.accept_uni().await.map_err(Error::from_transport)?;
		let mut reader: Reader<S::RecvStream, crate::Version> = Reader::new(recv, outer_version);

		let kind = match reader.varint_peek().await {
			Ok(kind) => kind,
			Err(err) if died_before_header(&err) => {
				tracing::debug!(%err, "dropping uni stream that died before its type");
				continue;
			}
			Err(err) => return Err(err),
		};
		if kind != setup::SETUP_V17 {
			early.push(reader);
			continue;
		}

		let setup: setup::Setup = reader.decode().await?;
		let (params, _) = ietf::Parameters::decode_slice(&setup.parameters, version)?;
		let path = match params.get_bytes(ietf::ParameterBytes::Path) {
			Some(bytes) => Some(
				std::str::from_utf8(bytes)
					.map_err(|_| Error::Decode(crate::DecodeError::InvalidValue))?
					.to_owned(),
			),
			None => None,
		};
		let token = super::token::from_setup(&params, version)?;
		let declared = peer_from_params(&params, version)?;

		return Ok(PeerSetup {
			stream: reader,
			path,
			token,
			declared,
			early,
		});
	}
}

/// Parse the Setup Options we act on out of a raw SETUP parameter block.
fn decode_peer_setup(parameters: bytes::Bytes, version: Version) -> Result<peer::Peer, crate::DecodeError> {
	let (params, _) = ietf::Parameters::decode_slice(&parameters, version)?;
	peer_from_params(&params, version)
}

/// The Setup Options we act on, out of an already-decoded parameter block. One place, so
/// a future option reaches both the pre-read accept path and the uni loop.
fn peer_from_params(params: &ietf::Parameters, version: Version) -> Result<peer::Peer, crate::DecodeError> {
	Ok(peer::Peer {
		cluster: cluster::peer_from_setup(params, version)?,
		solicit: solicit::from_setup(params, version)?,
		hidden: hidden::from_setup(params, version),
		auth: auth::from_setup(params, version) == Some(true),
		active_count: active_count::from_setup(params, version),
		max_request_updates: request_update::from_setup(params, version),
	})
}

/// Send our SETUP on a uni stream and keep it alive: on draft-17+ this stream is
/// also our GOAWAY channel, so a fired drain trigger encodes the GOAWAY here.
///
/// `path` is the request path we advertise (clients on URL-less transports); a
/// server passes `None`. `self_origin` and `cost` are the MoQ Cluster options, which
/// declare our identity and (client-only) what this link costs to cross. `extensions`
/// names the other extensions we offer.
#[allow(clippy::too_many_arguments)]
async fn run_setup<S: crate::transport::poll::Session>(
	runtime: crate::time::Clock,
	mut session: S,
	version: Version,
	path: Option<String>,
	authority: Option<String>,
	self_origin: Hop,
	cost: Option<u64>,
	extensions: crate::setup::Extensions,
	goaway: crate::goaway::Protocol,
) -> Result<(), Error> {
	let outer_version = crate::Version::Ietf(version);

	let send = session.open_uni().await.map_err(Error::from_transport)?;
	let mut writer: Writer<S::SendStream, crate::Version> = Writer::new(send, outer_version);

	let mut parameters = ietf::Parameters::default();
	parameters.set_bytes(ietf::ParameterBytes::Implementation, b"moq-lite-rs".to_vec());
	if let Some(path) = path {
		parameters.set_bytes(ietf::ParameterBytes::Path, path.into_bytes());
	}
	if let Some(authority) = authority {
		parameters.set_bytes(ietf::ParameterBytes::Authority, authority.into_bytes());
	}
	cluster::peer_into_setup(&mut parameters, self_origin, cost, version);
	if extensions.solicit {
		solicit::into_setup(&mut parameters, version);
	}
	hidden::into_setup(&mut parameters, version);
	if extensions.auth {
		auth::into_setup(&mut parameters, version);
	}
	active_count::into_setup(&mut parameters, version);
	request_update::into_setup(&mut parameters, version);
	let parameters = parameters.encode_bytes(version)?;

	writer.encode(&setup::Setup { parameters }).await?;

	// Hold the writer alive until the session closes, sending a GOAWAY if the
	// drain trigger fires meanwhile. The trigger resolves `None` when the session
	// drops without draining; keep holding either way (closing this stream
	// mid-session is a protocol violation on strict peers).
	let payload = kio::wait(|waiter| {
		let mut cx = waiter.context();
		if session.poll_closed(&mut cx).is_ready() {
			return std::task::Poll::Ready(None);
		}
		goaway.poll_triggered(waiter)
	})
	.await;

	if let Some(payload) = payload {
		let timeout_ms = payload.timeout.map(|d| d.as_millis() as u64).unwrap_or(0);
		let msg = ietf::GoAway {
			new_session_uri: std::borrow::Cow::Borrowed(payload.uri.as_ref()),
			timeout: timeout_ms,
		};

		// Frame as [type_id varint][size u16][body], the same shape as the
		// control-stream messages this channel otherwise carries.
		let mut writer = writer.with_version(version);
		writer.encode_message(&msg).await?;

		crate::goaway::enforce(&runtime, &mut session, payload.timeout).await;
		session.closed().await;
		writer.finish().ok();
	} else {
		writer.finish().ok();
	}

	Ok(())
}

/// The PADDING stream type (draft-18+): bytes a peer sends to probe for bandwidth.
const PADDING: u64 = 0x132B3E28;

/// What a unidirectional stream's type names on the negotiated draft.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UniType {
	/// The peer's SETUP, which then carries its GOAWAY (draft-17+).
	Setup,
	/// A SUBGROUP_HEADER, carrying one subgroup of a subscription.
	Subgroup,
	/// A FETCH_HEADER, carrying a fetch response.
	Fetch,
	/// Data to discard (draft-18+).
	Padding,
}

impl UniType {
	/// `None` is a type the draft does not define, which MUST close the session
	/// (draft-21 section 6.4.1, and its equivalent in every draft we negotiate).
	fn classify(kind: u64, version: Version) -> Option<Self> {
		// Draft-14-17 use SUBGROUP_HEADER types 0x10-0x1D and 0x30-0x3D; draft-18 adds
		// 0x40 (FIRST_OBJECT), also covering 0x50-0x5D and 0x70-0x7D. A reserved
		// SUBGROUP_ID_MODE (0b11) or a bit the draft lacks is invalid, which MUST close the
		// session too (draft-21 section 11.3.1).
		if ietf::GroupFlags::decode(kind, version).is_ok() {
			return Some(Self::Subgroup);
		}

		match kind {
			FetchHeader::TYPE => Some(Self::Fetch),
			setup::SETUP_V17 => match version {
				// SETUP rides the bidi control stream.
				Version::Draft14 | Version::Draft15 | Version::Draft16 => None,
				_ => Some(Self::Setup),
			},
			PADDING => match version {
				Version::Draft14 | Version::Draft15 | Version::Draft16 | Version::Draft17 => None,
				_ => Some(Self::Padding),
			},
			_ => None,
		}
	}
}

/// Setup and lifecycle state for the incoming uni stream dispatcher.
struct UniSetup<S: crate::transport::poll::Session> {
	peer: Option<peer::PeerSetup>,
	read: bool,
	/// Streams accepted before the session started, handled before accepting more.
	early: Vec<Reader<S::RecvStream, crate::Version>>,
	version: Version,
	goaway: crate::goaway::Protocol,
	local_close: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// Accept incoming uni streams, including SETUP and GOAWAY on draft-17+.
///
/// Each stream reads its own type in its own child. QUIC opens every lower-numbered
/// stream once a higher one arrives, so the next stream accepted can be one whose bytes
/// the peer has not sent yet, held back by connection flow control that the later
/// streams' unread data is using up. Waiting here for its type would deadlock the
/// connection.
async fn run_unis<S>(mut session: S, subscriber: Subscriber<S>, setup: UniSetup<S>) -> Result<(), Error>
where
	S: crate::transport::poll::Boxable,
{
	use std::task::Poll;

	let UniSetup {
		peer: peer_setup,
		read: setup_read,
		early,
		version,
		goaway,
		local_close,
	} = setup;
	let outer_version = crate::Version::Ietf(version);
	let mut tasks = TaskSet::owned();
	let uni = Uni {
		session: session.clone(),
		subscriber: subscriber.clone(),
		peer_setup,
		// A gated server accept already read the peer's one SETUP off its own uni stream,
		// so anything arriving here is a second one.
		seen_setup: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(setup_read)),
		goaway,
		version,
	};
	// The first stream to break the session, recorded by the child that read it.
	let fatal = kio::Shared::<Option<Error>>::default();
	let mut early = early.into_iter();

	loop {
		let reader = match early.next() {
			Some(reader) => reader,
			None => {
				let recv = tasks
					.drive(|waiter| {
						let broken = fatal.poll(waiter, |fatal| match fatal.is_some() {
							true => Poll::Ready(()),
							false => Poll::Pending,
						});
						if let Poll::Ready(mut fatal) = broken
							&& let Some(err) = fatal.take()
						{
							return Poll::Ready(Err(err));
						}
						let mut cx = waiter.context();
						session.poll_accept_uni(&mut cx).map(Ok)
					})
					.await;
				let recv = match recv {
					Ok(Ok(recv)) => recv,
					Ok(Err(err)) => {
						let err = Error::from_transport(err);
						// Settle tracks and open groups before the owned receive tasks drop
						// their cancellation guards, which would otherwise record Cancel.
						if local_close.load(std::sync::atomic::Ordering::Relaxed) {
							subscriber.close();
						} else {
							subscriber.abort(&err);
						}
						return Err(err);
					}
					Err(fatal) => return Err(fatal),
				};
				Reader::new(recv, outer_version)
			}
		};

		let uni = uni.clone();
		let fatal = fatal.clone();
		tasks.push(async move {
			if let Err(err) = uni.serve(reader).await {
				fatal.lock().get_or_insert(err);
			}
		});
	}
}

/// What serving one incoming uni stream needs from its session.
struct Uni<S: crate::transport::poll::Session> {
	session: S,
	subscriber: Subscriber<S>,
	peer_setup: Option<peer::PeerSetup>,
	/// Exactly one SETUP per endpoint.
	seen_setup: std::sync::Arc<std::sync::atomic::AtomicBool>,
	goaway: crate::goaway::Protocol,
	version: Version,
}

impl<S: crate::transport::poll::Session> Clone for Uni<S> {
	fn clone(&self) -> Self {
		Self {
			session: self.session.clone(),
			subscriber: self.subscriber.clone(),
			peer_setup: self.peer_setup.clone(),
			seen_setup: self.seen_setup.clone(),
			goaway: self.goaway.clone(),
			version: self.version,
		}
	}
}

impl<S: crate::transport::poll::Boxable> Uni<S> {
	/// Read the stream's type and serve it, failing only when the stream breaks the session.
	async fn serve(self, mut reader: Reader<S::RecvStream, crate::Version>) -> Result<(), Error> {
		let Self {
			mut session,
			mut subscriber,
			peer_setup,
			seen_setup,
			goaway,
			version,
		} = self;

		// A stream that dies before its type varint is that stream's failure, not the
		// session's. RESET_STREAM is how a peer drops a group, and QUIC does not order
		// the reset behind the data, so one can beat the first byte even of a stream
		// the peer wrote to. Failing here would tear down the whole session over a
		// single stream the peer had already given up on. Only death is tolerated:
		// bytes that arrive and do not parse stay session-fatal.
		//
		// A transport error counts as death too. A reset whose code the transport cannot
		// place in the stream registry surfaces as one: over raw QUIC a moq-transport
		// peer resets with its own codes (moxygen's CANCELLED is 0x1), which the
		// WebTransport code mapping rejects. If the connection itself died, the next
		// accept reports it.
		let kind: u64 = match std::future::poll_fn(|cx| reader.poll_varint_peek(cx)).await {
			Ok(kind) => kind,
			Err(err) if died_before_header(&err) => {
				tracing::debug!(%err, "dropping uni stream that died before its type");
				return Ok(());
			}
			Err(err) => return Err(err),
		};

		let Some(ty) = UniType::classify(kind, version) else {
			tracing::warn!(kind, "unknown uni stream type");
			return Err(Error::UnexpectedStream);
		};

		match ty {
			// SETUP then becomes the GOAWAY channel. It is read in the background like any
			// other stream; the one thing that does need it (the MoQ Cluster negotiation)
			// waits on `peer_setup` instead, so a slow SETUP delays announcements rather
			// than the whole session.
			UniType::Setup => {
				// Exactly one SETUP per endpoint. A second would let a peer restate its
				// declared identity mid-session, silently re-attributing every route
				// already built from the first.
				if seen_setup.swap(true, std::sync::atomic::Ordering::Relaxed) {
					return Err(Error::ProtocolViolation);
				}

				// The negotiation gates the announce and dispatch loops, so a SETUP we
				// cannot read must end the session rather than leave them parked on a
				// slot nothing will ever fill.
				let msg = match reader.decode::<setup::Setup>().await {
					Ok(msg) => msg,
					Err(err) => {
						tracing::warn!(%err, "setup decode error");
						session.close(SessionError::ProtocolViolation.to_code(), "invalid setup");
						return Ok(());
					}
				};

				if let Some(peer_setup) = peer_setup {
					let peer = match decode_peer_setup(msg.parameters, version) {
						Ok(peer) => peer,
						Err(err) => {
							tracing::warn!(%err, "setup parameter decode error");
							session.close(SessionError::ProtocolViolation.to_code(), "invalid setup parameters");
							return Ok(());
						}
					};
					peer_setup.set(peer);
				}

				// Monitor for GOAWAY after setup completes.
				if let Err(err) = run_goaway(reader.with_version(version), version, goaway).await {
					tracing::warn!(%err, "goaway error");
				}
			}
			UniType::Subgroup => {
				let mut reader = reader.with_version(version);
				let res = subscriber.recv_group(&mut reader).await;
				stop_on_error(&mut reader, res);
			}
			// A fill fetch stream carries the head of the group a draft-20 subscription
			// joined part way through. One answering no fill of ours is refused inside.
			UniType::Fetch => {
				let mut reader = reader.with_version(version);
				let res = subscriber.recv_fill(&mut reader).await;
				stop_on_error(&mut reader, res);
			}
			// The receiver MUST discard padding. We read it to the end rather than cancel,
			// so a peer probing for bandwidth gets the throughput it is measuring.
			UniType::Padding => {
				while let Ok(Some(_)) = std::future::poll_fn(|cx| reader.poll_read_chunk(cx, usize::MAX)).await {}
			}
		}
		Ok(())
	}
}

/// Whether reading an incoming stream's header failed because the stream died, which is
/// that stream's failure and not the session's. Bytes that arrive and do not parse are not.
fn died_before_header(err: &Error) -> bool {
	matches!(
		err,
		Error::Cancel | Error::Stream(_) | Error::Remote(_) | Error::Transport(_) | Error::Decode(DecodeError::Short)
	)
}

/// Receive QUIC datagrams, each an OBJECT_DATAGRAM for one of our subscriptions.
///
/// A transport without datagrams never delivers one, so this parks. A transport failure
/// or a malformed datagram ends the session.
async fn run_datagrams<S>(mut session: S, subscriber: Subscriber<S>) -> Result<(), Error>
where
	S: crate::transport::poll::Boxable,
{
	if session.max_datagram_size() == 0 {
		return Ok(());
	}
	loop {
		let payload = session.recv_datagram().await.map_err(Error::from_transport)?;
		subscriber.recv_datagram(payload)?;
	}
}

/// Stop a data stream whose handler failed. The handler owns only the stream, so it
/// cannot claim the session closed.
fn stop_on_error<R: crate::transport::poll::RecvStream>(reader: &mut Reader<R, Version>, res: Result<(), Error>) {
	let Err(err) = res else {
		return;
	};

	tracing::debug!(%err, "uni stream error");
	let reset = match StreamError::from(&err) {
		StreamError::Session(_) => StreamError::Internal,
		reset => reset,
	};
	reader.abort(reset);
}

/// Accept incoming bidi streams and dispatch to the correct handler based on message type.
async fn run_dispatch<S>(
	session: S,
	publisher: Publisher<S>,
	mut subscriber: Subscriber<S>,
	peer_setup: peer::PeerSetup,
	// Answers the peer's Auth requests, on the versions that can negotiate them.
	serve: Option<auth::Serve>,
	version: Version,
) -> Result<(), Error>
where
	S: crate::transport::poll::Boxable,
{
	// PUBLISH_NAMESPACE decodes differently once the MoQ Cluster extension is
	// negotiated, so the whole dispatch loop waits for the peer's SETUP first. The peer
	// must send it before anything else, and `run_unis` reads it independently, so this
	// costs a handshake round rather than blocking.
	let peer = subscriber.peer().await;

	// An AUTH without MoQ Auth negotiated, offered by both sides, is an unknown request,
	// which falls through to the protocol violation below.
	let serve = match peer_setup.get().await.auth {
		true => serve,
		false => None,
	};

	// From the same slot, so this costs nothing extra: it decides whether an unsolicited
	// advertisement is the peer ignoring our own SETUP (MoQ Solicit).
	let declared = subscriber.solicit().await;

	let mut tasks = TaskSet::owned();
	// Each AUTH serve task settles `Issued::closed` from whatever the auth handle
	// holds when it drops, and the first reason wins. So the error that ends the
	// session has to reach the handle while `tasks` is still alive, or every grant
	// reports a bare cancel instead. `None` when the peer never negotiated AUTH,
	// which leaves no serve task to settle.
	let handle = serve.as_ref().map(|serve| serve.handle.clone());

	// Scoped so `tasks` outlives the close below: the loop borrows it, so it only
	// drops once this block's future is done.
	let res: Result<(), Error> = async {
		let mut accept = session.clone();
		loop {
			let mut stream = tasks
				.drive(|waiter| {
					let mut cx = waiter.context();
					Stream::poll_accept(&mut accept, version, &mut cx)
				})
				.await?;

			// The intermediate results live outside the poll closure, so a Pending
			// mid-header resumes where it left off.
			let mut hdr_id: Option<u64> = None;
			let header = tasks
				.drive(|waiter| {
					let mut cx = waiter.context();
					let id = match hdr_id {
						Some(id) => id,
						None => *hdr_id.insert(std::task::ready!(stream.reader.poll_varint(&mut cx))?),
					};
					let body = std::task::ready!(stream.reader.poll_decode::<ietf::Body>(&mut cx))?;
					std::task::Poll::Ready(Ok::<_, Error>((id, body)))
				})
				.await;
			// Same tolerance as `run_unis`: a request stream that dies before its header
			// is the peer abandoning that request, not the session. Anything else, a
			// header that does not parse included, still fails the session.
			let (id, data) = match header {
				Ok(header) => header,
				Err(err) if died_before_header(&err) => {
					tracing::debug!(%err, "dropping bidi stream that died before its header");
					continue;
				}
				Err(err) => return Err(err),
			};

			match id {
				// Draft-16 moved SUBSCRIBE_NAMESPACE to its own stream, past the control stream
				// that admits every other request, but it still takes a request ID from the
				// MAX_REQUEST_ID window. Held until the request ends, like the rest.
				ietf::SubscribeNamespaceLegacy::ID if version == Version::Draft16 => {
					let request_id =
						RequestId::decode(&mut crate::coding::Decoder::new(&data.0, version.into()), version)?;
					let permit = publisher.control.accept(request_id)?;
					let task = publisher.handle_stream(id, data, stream)?;
					tasks.push(
						async move {
							let _permit = permit;
							task.await
						}
						.maybe_boxed(),
					);
				}
				// Publisher handles: Subscribe, Fetch, SubscribeNamespace (0x50 modern /
				// 0x11 legacy), SubscribeTracks, TrackStatus
				ietf::Subscribe::ID
				| ietf::Fetch::ID
				| ietf::SubscribeNamespace::ID
				| ietf::SubscribeNamespaceLegacy::ID
				| ietf::SUBSCRIBE_TRACKS_ID
				| ietf::TrackStatus::ID => {
					tasks.push(publisher.handle_stream(id, data, stream)?);
				}
				// Subscriber handles: Publish, PublishNamespace
				ietf::Publish::ID | ietf::PublishNamespace::ID => {
					tasks.push(subscriber.handle_stream(id, data, stream, peer, declared)?);
				}
				auth::Auth::ID if let Some(serve) = &serve => {
					let mut data = data.decoder(version);
					let msg = auth::Auth::decode_msg(&mut data, version)?;
					if !data.is_empty() {
						return Err(Error::WrongSize);
					}
					tasks.push(serve.clone().run(stream, msg, version));
				}
				_ => {
					tracing::warn!(id, "unexpected bidi stream type");
					return Err(Error::UnexpectedStream);
				}
			}
		}
	}
	.await;

	// Every error exit above drops the AUTH serve tasks in `tasks`, so the session's
	// error has to land on the handle first. The driver closes it again with the same
	// error, which then does nothing.
	if let Some(handle) = &handle
		&& let Err(err) = &res
	{
		handle.close(err.clone());
	}
	res
}

/// Monitor the peer's SETUP stream for a GOAWAY, surfacing it through
/// [`crate::Session::goaway`], then hold the stream until it FINs.
async fn run_goaway<R: crate::transport::poll::RecvStream>(
	mut reader: Reader<R, Version>,
	version: Version,
	goaway: crate::goaway::Protocol,
) -> Result<(), Error> {
	let id = match reader.varint_maybe().await? {
		Some(id) => id,
		None => return Ok(()),
	};

	let body: ietf::Body = reader.decode().await?;
	let mut data = body.decoder(version);

	if id != ietf::GoAway::ID {
		return Err(Error::UnexpectedMessage);
	}

	let msg = ietf::GoAway::decode_msg(&mut data, version)?;
	tracing::info!(message = ?msg, "received GOAWAY");

	let timeout = (msg.timeout > 0).then(|| std::time::Duration::from_millis(msg.timeout));
	// A second GOAWAY is a protocol violation the draft requires we close over.
	goaway.record(crate::goaway::Goaway {
		uri: msg.new_session_uri.into_owned(),
		timeout,
	})?;

	// Keep the reader alive until the peer FINs or the session closes. Dropping
	// it here would STOP_SENDING the peer's SETUP uni stream, which draft-19
	// sect 3.3 forbids closing at the transport layer mid-session, so a strict
	// peer would tear the session down as a PROTOCOL_VIOLATION right in the
	// middle of the drain we are trying to honor.
	//
	// Nothing else is expected on this stream: a peer sends at most one GOAWAY
	// per session. A second one is the same protocol violation the shared
	// control stream enforces, so close over it here too rather than logging;
	// anything else is merely unexpected and discarded.
	loop {
		let id = match reader.varint_maybe().await? {
			Some(id) => id,
			None => return Ok(()),
		};
		let body: ietf::Body = reader.decode().await?;
		let mut data = body.decoder(version);

		if id == ietf::GoAway::ID {
			let msg = ietf::GoAway::decode_msg(&mut data, version)?;
			let timeout = (msg.timeout > 0).then(|| std::time::Duration::from_millis(msg.timeout));
			goaway.record(crate::goaway::Goaway {
				uri: msg.new_session_uri.into_owned(),
				timeout,
			})?;
			continue;
		}

		tracing::warn!(id, "unexpected message after GOAWAY on the SETUP stream; ignoring");
	}
}

/// Closes the auth handle when the session driver ends without finishing, releasing
/// anything still waiting on a token.
struct AuthClose(crate::auth::Handle);

impl Drop for AuthClose {
	fn drop(&mut self) {
		self.0.close(Error::Cancel);
	}
}

/// Close the session when our origin announces a broadcast our grant never covered,
/// instead of leaving it to wait for a subscription that never comes. See
/// [`crate::auth::Enforce`].
async fn enforce_grant<S: crate::transport::poll::Session>(
	auth: crate::auth::Handle,
	origin: origin::Consumer,
	mut session: S,
	request_token: crate::RequestToken,
	peer_setup: peer::PeerSetup,
	version: Version,
) -> Result<(), Error> {
	// A request token authorizes an announce outside the connection grant only when the
	// announce carries it, which is when it rides its own PUBLISH_NAMESPACE request: always on
	// draft-14/15, and on later drafts unless the peer requires solicitation, which turns every
	// advertisement into an inline NAMESPACE entry with no token slot. Then the grant still
	// bounds what we publish.
	if request_token.peek().is_some() && announces_carry_token(&peer_setup, version).await {
		return Ok(());
	}
	let mut announced = origin.announced();
	let mut check = crate::auth::Enforce::default();
	let Some(path) = kio::wait(|waiter| check.poll(&auth, &mut announced, waiter)).await else {
		return Ok(());
	};
	tracing::error!(broadcast = %origin.absolute(&path), "publishing outside our grant; closing the session");
	let err = Error::Unauthorized;
	session.close(
		SessionError::from(&err).to_code(),
		&crate::auth::unauthorized_reason(&path),
	);
	Err(err)
}

/// Whether our announces will ride their own PUBLISH_NAMESPACE requests, the only form that
/// carries a request token, rather than inline NAMESPACE entries. Draft-14/15 predate
/// NAMESPACE; later drafts send requests unless the peer requires solicitation.
async fn announces_carry_token(peer_setup: &peer::PeerSetup, version: Version) -> bool {
	match version {
		Version::Draft14 | Version::Draft15 => true,
		_ => !peer_setup.get().await.solicit.unwrap_or(false),
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::model::ProduceTest;

	fn occurrences(log: &crate::lite::test_transport::Log, needle: &[u8]) -> usize {
		let writes = log.writes.lock().unwrap();
		writes.windows(needle.len()).filter(|window| *window == needle).count()
	}

	/// The peer's REQUEST_OK followed by a NAMESPACE in the base form: no cluster
	/// parameters, so no HOP_PATH. Built with the crate's own writer so the framing
	/// can't drift from the encoder under test.
	async fn namespace_without_hop_path(version: Version) -> Vec<u8> {
		let log = crate::lite::test_transport::Log::default();
		let mut writer = crate::coding::Writer::new(crate::lite::test_transport::SinkSend::new(log.clone()), version);

		writer.varint(ietf::RequestOk::ID).await.unwrap();
		writer
			.encode(&ietf::RequestOk {
				request_id: None,
				active: None,
			})
			.await
			.unwrap();
		writer.varint(ietf::Namespace::ID).await.unwrap();
		writer
			.encode(&ietf::Namespace {
				suffix: crate::Path::new("cam"),
				cluster: None,
			})
			.await
			.unwrap();

		let writes = log.writes.lock().unwrap();
		writes.clone()
	}

	/// A session that negotiated the MoQ Cluster extension requires HOP_PATH on every
	/// NAMESPACE, and the draft answers a missing one by closing the session.
	///
	/// Driven through `start` rather than `run_subscribe_namespace` directly: the
	/// stream surfaces the error either way, so only this loop's handling of it decides
	/// between a close and a warning, and a test below the loop would pass regardless.
	#[moq_net_sim::test]
	async fn a_namespace_without_a_hop_path_closes_the_session() {
		const VERSION: Version = Version::Draft19;

		// A driver that swallows the violation parks forever instead of failing, so
		// bound it: paused time makes the deadline fire the moment nothing else can run.

		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let session = crate::lite::test_transport::ScriptedSession::new(namespace_without_hop_path(VERSION).await);
		let log = session.log.clone();

		let (driver, _goaway, _) = start(Config {
			runtime: crate::time::Clock::sim(),
			session,
			setup: None,
			request_id_max: None,
			limits: Default::default(),
			client: true,
			publish: None,
			subscribe: Some(origin),
			peer_hop: None,
			cost: None,
			version: VERSION,
			path: None,
			authority: None,
			peer_setup_stream: None,
			// A peer that declared its Hop ID negotiated the extension, which is what
			// makes the cluster parameters mandatory in both directions.
			peer_declared: Some(peer::Peer {
				cluster: cluster::Peer {
					hop: Some(crate::Hop::new(2).unwrap()),
					cost: None,
				},
				..Default::default()
			}),
			auth: crate::auth::Handle::new(false),
			early_unis: Vec::new(),
			extensions: Default::default(),
		})
		.expect("start the session");

		let err = moq_net_sim::timeout(std::time::Duration::from_secs(10), driver)
			.await
			.expect("the session ended rather than carrying on")
			.expect_err("a malformed NAMESPACE fails the session");

		assert!(is_protocol_violation(&err), "not treated as the peer's fault: {err}");
		// A session close carries a session code, so the peer is told PROTOCOL_VIOLATION
		// rather than the local table's value for whichever decode error we hit.
		let (code, reason) = log.closes().first().cloned().expect("the session was closed");
		assert_eq!(code, SessionError::ProtocolViolation.to_code());
		assert_eq!(reason, err.to_string());
	}

	/// A subscriber issues one SUBSCRIBE_NAMESPACE per PERMITTED PREFIX, and asks for
	/// those prefixes rather than the root it mounts replies under. Driven through
	/// `start` so the per-prefix fan-out is exercised, not just one stream in
	/// isolation: a loop that opened a single stream would still satisfy a test that
	/// called `run_subscribe_namespace` itself.
	#[moq_net_sim::test]
	async fn every_permitted_prefix_gets_its_own_subscribe_namespace() {
		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let scope: crate::Patterns = ["cam", "mic"]
			.into_iter()
			.map(|prefix| crate::Pattern::subtree(prefix).unwrap())
			.collect();
		let scoped = origin
			.scope("rootns", &scope)
			.expect("scope the origin to two prefixes");

		let gate = kio::Producer::new(true);
		let session = crate::lite::test_transport::SinkSession::gated_bi(gate.consume());
		let log = session.log.clone();

		let (driver, _goaway, _) = start(Config {
			runtime: crate::time::Clock::sim(),
			session,
			setup: None,
			request_id_max: None,
			limits: Default::default(),
			client: true,
			publish: None,
			subscribe: Some(scoped),
			peer_hop: None,
			cost: None,
			version: Version::Draft18,
			path: None,
			authority: None,
			peer_setup_stream: None,
			// The requests wait on the peer's SETUP (MoQ Hidden).
			peer_declared: Some(peer::Peer::default()),
			auth: crate::auth::Handle::new(false),
			early_unis: Vec::new(),
			extensions: Default::default(),
		})
		.expect("start the session");
		let _driver = moq_net_sim::spawn(driver);

		// Both requests are written before either peer response, which never comes.
		for _ in 0..100 {
			if occurrences(&log, b"cam") > 0 && occurrences(&log, b"mic") > 0 {
				break;
			}
			moq_net_sim::sleep(std::time::Duration::from_millis(5)).await;
		}

		assert_eq!(occurrences(&log, b"cam"), 1, "one SUBSCRIBE_NAMESPACE for cam");
		assert_eq!(occurrences(&log, b"mic"), 1, "one SUBSCRIBE_NAMESPACE for mic");
		assert_eq!(occurrences(&log, b"rootns"), 0, "asked the peer for our local root");
	}

	/// The bytes of one legacy SUBSCRIBE_NAMESPACE, as the session writes them.
	async fn subscribe_namespace_legacy(version: Version, namespace: &str) -> Vec<u8> {
		let log = crate::lite::test_transport::Log::default();
		let mut writer = Writer::new(crate::lite::test_transport::SinkSend::new(log.clone()), version);
		let msg = ietf::SubscribeNamespaceLegacy {
			request_id: RequestId(0),
			namespace: crate::Path::new(namespace),
			subscribe_options: ietf::SubscribeOptions::Namespace,
			hidden: false,
		};
		writer.varint(ietf::SubscribeNamespaceLegacy::ID).await.unwrap();
		writer.encode(&msg).await.unwrap();
		log.writes.lock().unwrap().clone()
	}

	/// How many times a session with this scope writes SUBSCRIBE_NAMESPACE for it.
	///
	/// `prefix: None` is an unscoped origin, whose only interest head is empty. `solicit`
	/// is what the peer's SETUP declared (MoQ Solicit).
	async fn asked_namespace(version: Version, prefix: Option<&str>, solicit: Option<bool>) -> usize {
		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let subscribe = match prefix {
			Some(name) => {
				let scope: crate::Patterns = [crate::Pattern::subtree(name).unwrap()].into_iter().collect();
				origin.scope("rootns", &scope).expect("scope the origin")
			}
			None => origin,
		};

		let gate = kio::Producer::new(true);
		let mut session = crate::lite::test_transport::SinkSession::gated_bi(gate.consume());
		let log = session.log.clone();
		let setup = Stream::open(&mut session, version)
			.await
			.expect("open the control stream");

		let (driver, _goaway, _) = start(Config {
			runtime: crate::time::Clock::sim(),
			session,
			setup: Some(setup),
			request_id_max: None,
			limits: Default::default(),
			client: true,
			publish: None,
			subscribe: Some(subscribe),
			peer_hop: None,
			cost: None,
			version,
			path: None,
			authority: None,
			peer_setup_stream: None,
			peer_declared: Some(peer::Peer {
				solicit,
				..Default::default()
			}),
			auth: crate::auth::Handle::new(false),
			early_unis: Vec::new(),
			extensions: Default::default(),
		})
		.expect("start the session");
		let driver = moq_net_sim::spawn(driver);

		let needle = subscribe_namespace_legacy(version, prefix.unwrap_or("")).await;
		for _ in 0..ANNOUNCE_TURNS {
			if occurrences(&log, &needle) > 0 {
				break;
			}
			moq_net_sim::sleep(std::time::Duration::from_millis(1)).await;
		}

		assert!(!driver.is_finished(), "{version:?} ended the session");
		assert!(log.closes().is_empty(), "{version:?} closed: {:?}", log.closes());
		occurrences(&log, &needle)
	}

	/// Draft-14 and draft-15 reject a zero-field track namespace, so a foreign peer (one
	/// that declared no MoQ Solicit) is not asked for it, and a real prefix still goes
	/// out. A peer that declared Solicit is ours: it only tells when asked, so it still
	/// gets the empty prefix. Draft-16 made the empty prefix legal, so it stays.
	#[moq_net_sim::test]
	async fn an_empty_namespace_is_not_asked_of_a_foreign_peer_before_draft_16() {
		for version in [Version::Draft14, Version::Draft15] {
			assert_eq!(
				asked_namespace(version, None, None).await,
				0,
				"{version:?} asked a foreign peer for every namespace"
			);
			assert_eq!(
				asked_namespace(version, Some("cam"), None).await,
				1,
				"{version:?} skipped a real prefix"
			);
			assert_eq!(
				asked_namespace(version, None, Some(true)).await,
				1,
				"{version:?} did not ask a soliciting peer for every namespace"
			);
		}
		assert_eq!(
			asked_namespace(Version::Draft16, None, None).await,
			1,
			"draft-16 dropped the empty prefix"
		);
	}

	/// How many scheduling turns an advertisement gets before the count is taken. Time is
	/// paused in these tests, so each turn costs nothing and only runs the driver until it
	/// parks again; a busy machine cannot turn a slow announce into a passing silence.
	const ANNOUNCE_TURNS: usize = 100;

	/// A client that presents a request token authorizes each announce at the server when the
	/// announce carries the token, so dialing-side grant enforcement stands down for a peer
	/// that takes unsolicited PUBLISH_NAMESPACE requests: enforcing it would close the client
	/// for announcing outside the connection grant the token was meant to extend.
	#[moq_net_sim::test]
	async fn a_client_may_send_a_token_bearing_request_its_connection_grant_does_not_cover() {
		let auth = crate::auth::Handle::new(true);
		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let _cam = origin.announce("room/alice", crate::origin::Route::default()).unwrap();
		let session = crate::lite::test_transport::SinkSession::new(Default::default());
		let log = session.log.clone();
		let peer_setup = peer::PeerSetup::default();
		peer_setup.set(peer::Peer {
			solicit: Some(false),
			..Default::default()
		});

		let result = enforce_grant(
			auth,
			origin.consume(),
			session,
			crate::RequestToken::new(Some(crate::setup::Token {
				kind: 0,
				value: b"jwt".to_vec(),
			})),
			peer_setup,
			Version::Draft18,
		)
		.await;

		assert!(
			result.is_ok(),
			"a token-bearing client must not be closed by grant enforcement"
		);
		assert!(
			log.closes().is_empty(),
			"the session must stay open for a token-bearing client"
		);
	}

	/// When the peer requires solicitation, every advertisement is an inline NAMESPACE entry,
	/// which has no slot for a request token. A token set for SUBSCRIBE must not let the client
	/// advertise outside its connection grant there: the grant is still enforced.
	#[moq_net_sim::test]
	async fn a_request_token_does_not_lift_the_grant_when_announces_are_inline() {
		let auth = crate::auth::Handle::new(true);
		let setup = auth.present(bytes::Bytes::new(), true).unwrap();
		auth.granted(
			0,
			crate::auth::Grant {
				publish: crate::Pattern::subtree("room/alice").unwrap().into(),
				subscribe: crate::Patterns::new(),
				expires: None,
			},
		);
		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let _cam = origin
			.announce("room/bob/cam", crate::origin::Route::default())
			.unwrap();
		let session = crate::lite::test_transport::SinkSession::new(Default::default());
		let log = session.log.clone();
		let peer_setup = peer::PeerSetup::default();
		peer_setup.set(peer::Peer {
			solicit: Some(true),
			..Default::default()
		});

		let result = enforce_grant(
			auth,
			origin.consume(),
			session,
			crate::RequestToken::new(Some(crate::setup::Token {
				kind: 0,
				value: b"jwt".to_vec(),
			})),
			peer_setup,
			Version::Draft18,
		)
		.await;

		assert!(matches!(result, Err(Error::Unauthorized)), "{result:?}");
		assert_eq!(log.closes().len(), 1, "the session closes on the uncovered announce");
		drop(setup);
	}

	/// Run a publish-only session against a peer that declared `peer_declared`, returning
	/// how many times the namespace reached the wire.
	///
	/// The scripted peer answers nothing, so an advertisement parks after writing. That is
	/// enough: the question is only whether the bytes went out unasked.
	async fn announce_occurrences(peer_declared: Option<peer::Peer>) -> usize {
		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let _cam = origin.announce("solo-cam", crate::origin::Route::default()).unwrap();

		// An open gate: an unsolicited PUBLISH_NAMESPACE can reach the wire.
		let gate = kio::Producer::new(true);
		let session = crate::lite::test_transport::SinkSession::gated_bi(gate.consume());
		let log = session.log.clone();

		let (driver, _goaway, _) = start(Config {
			runtime: crate::time::Clock::sim(),
			session,
			setup: None,
			request_id_max: None,
			limits: Default::default(),
			client: true,
			publish: Some(origin.consume()),
			subscribe: None,
			peer_hop: None,
			cost: None,
			version: Version::Draft18,
			path: None,
			authority: None,
			peer_setup_stream: None,
			peer_declared,
			auth: crate::auth::Handle::new(false),
			early_unis: Vec::new(),
			extensions: Default::default(),
		})
		.expect("start the session");
		let _driver = moq_net_sim::spawn(driver);

		// Drive until the announce lands, rather than betting on one fixed window.
		for _ in 0..ANNOUNCE_TURNS {
			if occurrences(&log, b"solo-cam") > 0 {
				break;
			}
			moq_net_sim::sleep(std::time::Duration::from_millis(1)).await;
		}

		occurrences(&log, b"solo-cam")
	}

	/// The peer's SETUP decides whether an advertisement may go out unasked, so nothing
	/// can be sent before it arrives.
	#[moq_net_sim::test]
	async fn no_announce_before_the_peer_setup() {
		assert_eq!(
			announce_occurrences(None).await,
			0,
			"advertised before knowing what the peer wants"
		);
	}

	/// A peer that requires solicitation hears nothing until it asks, which is the
	/// behavior the IETF draft describes for a relay.
	#[moq_net_sim::test]
	async fn a_peer_requiring_solicitation_is_not_told_unasked() {
		let declared = peer::Peer {
			solicit: Some(true),
			..Default::default()
		};

		assert_eq!(
			announce_occurrences(Some(declared)).await,
			0,
			"PUBLISH_NAMESPACE despite the peer asking to be told on request"
		);
	}

	/// A peer that declared nothing is told without being asked. Every relay that never
	/// sends SUBSCRIBE_NAMESPACE depends on this, and the session is what wires the
	/// unsolicited loop up at all.
	#[moq_net_sim::test]
	async fn a_peer_declaring_nothing_is_told_unasked() {
		assert_eq!(
			announce_occurrences(Some(peer::Peer::default())).await,
			1,
			"no unsolicited PUBLISH_NAMESPACE"
		);
	}

	/// The AUTH message type as it leads an Auth request on the draft-18 wire.
	fn auth_type() -> Vec<u8> {
		let mut buf = Vec::new();
		crate::coding::Encoder::new(&mut buf, Version::Draft18.into())
			.varint(auth::Auth::ID)
			.unwrap();
		buf
	}

	/// A publishing draft-18 session with AUTH available locally, against a peer that
	/// declared `auth`. Everything the session needs to keep running is held here.
	struct AuthSession {
		handle: crate::auth::Handle,
		log: crate::lite::test_transport::Log,
		_origin: crate::origin::Producer,
		_cam: crate::AnnounceProducer,
		_gate: kio::Producer<bool>,
		_goaway: crate::goaway::Handle,
		_driver: moq_net_sim::JoinHandle<Result<(), Error>>,
	}

	fn auth_session(auth: bool) -> AuthSession {
		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let cam = origin.announce("solo-cam", crate::origin::Route::default()).unwrap();

		let gate = kio::Producer::new(true);
		let session = crate::lite::test_transport::SinkSession::gated_bi(gate.consume());
		let log = session.log.clone();

		let handle = crate::auth::Handle::new(true);
		let (driver, goaway, _) = start(Config {
			runtime: crate::time::Clock::sim(),
			session,
			setup: None,
			request_id_max: None,
			limits: Default::default(),
			client: true,
			publish: Some(origin.consume()),
			subscribe: None,
			peer_hop: None,
			cost: None,
			version: Version::Draft18,
			path: None,
			authority: None,
			peer_setup_stream: None,
			peer_declared: Some(peer::Peer {
				auth,
				..Default::default()
			}),
			auth: handle.clone(),
			early_unis: Vec::new(),
			extensions: Default::default(),
		})
		.expect("start the session");
		AuthSession {
			handle,
			log,
			_origin: origin,
			_cam: cam,
			_gate: gate,
			_goaway: goaway,
			_driver: moq_net_sim::spawn(driver),
		}
	}

	/// A peer that never offered MoQ Auth sees no Auth request, the session keeps
	/// working, and every token fails as unsupported rather than hanging.
	#[moq_net_sim::test]
	async fn a_peer_without_auth_sees_no_auth_request() {
		let session = auth_session(false);
		let (handle, log) = (&session.handle, &session.log);

		let err = moq_net_sim::timeout(std::time::Duration::from_secs(1), handle.add("token"))
			.await
			.expect("unsupported promptly")
			.err()
			.expect("no AUTH on this session");
		assert!(matches!(err, Error::Unsupported), "{err:?}");
		assert_eq!(handle.grant().peek(), None, "no grant without the extension");

		for _ in 0..ANNOUNCE_TURNS {
			if occurrences(log, b"solo-cam") > 0 {
				break;
			}
			moq_net_sim::sleep(std::time::Duration::from_millis(1)).await;
		}
		assert_eq!(occurrences(log, b"solo-cam"), 1, "the session stopped advertising");
		assert_eq!(
			occurrences(log, &auth_type()),
			0,
			"sent AUTH to a peer that never offered it"
		);
	}

	/// A peer that offered it gets the connection's own credential right away.
	#[moq_net_sim::test]
	async fn a_negotiating_peer_gets_the_setup_token() {
		let session = auth_session(true);
		let log = &session.log;
		for _ in 0..ANNOUNCE_TURNS {
			if occurrences(log, &auth_type()) > 0 {
				break;
			}
			moq_net_sim::sleep(std::time::Duration::from_millis(1)).await;
		}
		assert_eq!(occurrences(log, &auth_type()), 1, "one AUTH for the setup token");
	}

	/// What the draft-18 SETUP that `start` writes says about MoQ Auth and MoQ Solicit.
	async fn our_setup(client: bool, auth: crate::auth::Handle, extensions: crate::setup::Extensions) -> (bool, bool) {
		const VERSION: Version = Version::Draft18;

		let session = crate::lite::test_transport::ScriptedSession::new(Vec::new());
		let log = session.log.clone();
		let (driver, _goaway, _) = start(Config {
			runtime: crate::time::Clock::sim(),
			session,
			setup: None,
			request_id_max: None,
			limits: Default::default(),
			client,
			publish: None,
			subscribe: None,
			peer_hop: None,
			cost: None,
			version: VERSION,
			path: None,
			authority: None,
			peer_setup_stream: None,
			peer_declared: Some(peer::Peer::default()),
			auth,
			early_unis: Vec::new(),
			extensions,
		})
		.expect("start the session");

		// The driver parks once SETUP is out.
		moq_net_sim::timeout(std::time::Duration::from_secs(10), driver)
			.await
			.expect_err("the session ended instead of parking after SETUP");

		let writes = log.writes.lock().unwrap().clone();
		let (setup, _) = setup::Setup::decode_slice(&writes, crate::Version::Ietf(VERSION)).expect("our SETUP");
		let (params, _) = ietf::Parameters::decode_slice(&setup.parameters, VERSION).expect("its options");
		(
			auth::from_setup(&params, VERSION) == Some(true),
			solicit::from_setup(&params, VERSION).unwrap() == Some(true),
		)
	}

	/// Each extension reaches our SETUP only while this side offers it.
	#[moq_net_sim::test]
	async fn a_declined_extension_stays_out_of_setup() {
		let all = crate::setup::Extensions::default();
		let mut no_auth = all;
		no_auth.auth = false;
		let mut no_solicit = all;
		no_solicit.solicit = false;

		// A client's handle supports AUTH exactly when it offers it.
		assert_eq!(our_setup(true, crate::auth::Handle::new(true), all).await, (true, true));
		assert_eq!(
			our_setup(true, crate::auth::Handle::new(false), no_auth).await,
			(false, true)
		);
		assert_eq!(
			our_setup(true, crate::auth::Handle::new(true), no_solicit).await,
			(true, false)
		);
	}

	/// A server's handle supports AUTH only when the client offered it too, so a server
	/// answering a client that did not offer it leaves the option out, whatever it offers.
	#[moq_net_sim::test]
	async fn a_server_offers_auth_only_to_a_client_that_did() {
		let all = crate::setup::Extensions::default();
		assert_eq!(
			our_setup(false, crate::auth::Handle::new(true), all).await,
			(true, true)
		);
		assert_eq!(
			our_setup(false, crate::auth::Handle::new(false), all).await,
			(false, true)
		);
	}

	/// The bytes of a peer's Auth request presenting its connection credential.
	async fn auth_request(version: Version) -> Vec<u8> {
		let log = crate::lite::test_transport::Log::default();
		let mut writer = crate::coding::Writer::new(crate::lite::test_transport::SinkSend::new(log.clone()), version);
		writer
			.encode_message(&auth::Auth {
				request_id: RequestId(0),
				token: bytes::Bytes::new(),
			})
			.await
			.unwrap();
		let writes = log.writes.lock().unwrap();
		writes.clone()
	}

	/// Run a draft-18 server whose client offered MoQ Auth and then sent an Auth request.
	/// `offered` is whether the server offered it too. Returns how the session ended, if it
	/// did, and what closed it.
	async fn serve_auth_request(offered: bool) -> (Option<Result<(), Error>>, Vec<(u32, String)>) {
		const VERSION: Version = Version::Draft18;

		let session = crate::lite::test_transport::ScriptedSession::new(Vec::new())
			.with_incoming_bidis(vec![auth_request(VERSION).await]);
		let log = session.log.clone();
		let (driver, _goaway, _) = start(Config {
			runtime: crate::time::Clock::sim(),
			session,
			setup: None,
			request_id_max: None,
			limits: Default::default(),
			client: false,
			publish: None,
			subscribe: None,
			peer_hop: None,
			cost: None,
			version: VERSION,
			path: None,
			authority: None,
			peer_setup_stream: None,
			peer_declared: Some(peer::Peer {
				auth: true,
				..Default::default()
			}),
			// As `Server` builds it: offered by the client, so supported iff we offer it.
			auth: crate::auth::Handle::new(offered),
			early_unis: Vec::new(),
			extensions: crate::setup::Extensions::default(),
		})
		.expect("start the session");

		let res = moq_net_sim::timeout(std::time::Duration::from_secs(10), driver)
			.await
			.ok();
		(res, log.closes())
	}

	/// The Auth draft negotiates the extension only when both sides offer it, so an Auth
	/// request reaching a side that declined it is a protocol violation, not a token to grant.
	#[moq_net_sim::test]
	async fn an_auth_request_to_a_side_that_declined_it_is_refused() {
		let (res, closes) = serve_auth_request(true).await;
		assert!(res.is_none(), "both offered: the request is served, {res:?}");
		assert_eq!(closes, vec![]);

		let (res, closes) = serve_auth_request(false).await;
		let Some(Err(err)) = res else {
			panic!("declined: the Auth request did not end the session: {res:?}");
		};
		assert_eq!(SessionError::from(&err), SessionError::ProtocolViolation);
		assert_eq!(
			closes,
			vec![(SessionError::ProtocolViolation.to_code(), err.to_string())]
		);
	}

	/// The declared Hop ID must be the caller's own origin, whichever half carries it.
	///
	/// A subscribe-only session (an ingest that publishes nothing) still routes, so
	/// declaring the placeholder's random id would compare incoming paths against an
	/// identity no other session out of this process shares, and a route returning
	/// here would never be recognized as a loop.
	#[test]
	fn the_hop_id_comes_from_whichever_origin_the_caller_set() {
		let ours = crate::Hop::new(42).unwrap();

		let publish = crate::origin::Config::new(ours).produce();
		assert_eq!(self_origin(Some(&publish.consume()), None), ours, "the publish half");

		let subscribe = crate::origin::Config::new(ours).produce();
		assert_eq!(self_origin(None, Some(&subscribe)), ours, "the subscribe half alone");

		// Neither half: nothing to route, so any id will do as long as it is ours.
		let publish = crate::origin::Config::new(ours).produce();
		assert_eq!(self_origin(Some(&publish.consume()), Some(&subscribe)), ours);
	}

	/// Drive `start` against a peer whose incoming streams die before their first
	/// byte, and assert the session shrugs them off: the driver keeps running and
	/// nothing closes the transport.
	///
	/// The dead stream is not exotic. RESET_STREAM is how a peer drops a group, and
	/// QUIC does not order the reset behind the data, so it can beat the first byte
	/// even when the peer wrote one. Erroring an accept loop on it tears down the
	/// whole session over a single stream the peer had already given up on.
	async fn a_dead_incoming_stream_is_not_fatal(session: crate::lite::test_transport::DeadStreamSession) {
		const VERSION: Version = Version::Draft19;

		// A driver that survives parks forever, so bound it: paused time makes the
		// deadline fire the moment nothing else can run.

		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let log = session.log.clone();

		let (driver, _goaway, _) = start(Config {
			runtime: crate::time::Clock::sim(),
			session,
			setup: None,
			request_id_max: None,
			limits: Default::default(),
			client: true,
			publish: None,
			subscribe: Some(origin),
			peer_hop: None,
			cost: None,
			version: VERSION,
			path: None,
			authority: None,
			peer_setup_stream: None,
			// Pre-settled, so nothing waits on a SETUP the dead stream will never
			// carry and the dispatch loop actually runs.
			peer_declared: Some(peer::Peer::default()),
			auth: crate::auth::Handle::new(false),
			early_unis: Vec::new(),
			extensions: Default::default(),
		})
		.expect("start the session");

		moq_net_sim::timeout(std::time::Duration::from_secs(10), driver)
			.await
			.expect_err("the session ended over one dead stream");

		assert_eq!(log.closes(), vec![], "nothing may close the transport");
	}

	/// A draft-17+ client advertises its AUTHORITY in the SETUP it writes on its uni stream.
	#[moq_net_sim::test]
	async fn setup_carries_the_authority() {
		const AUTHORITY: &[u8] = b"relay.example.com:4443";

		// The driver parks forever once SETUP is out, so bound it: paused time makes the
		// deadline fire the moment nothing else can run.

		for version in [Version::Draft18, Version::Draft19] {
			let session = crate::lite::test_transport::ScriptedSession::new(Vec::new());
			let log = session.log.clone();

			let (driver, _goaway, _) = start(Config {
				runtime: crate::time::Clock::sim(),
				session,
				setup: None,
				request_id_max: None,
				limits: Default::default(),
				client: true,
				publish: None,
				subscribe: None,
				peer_hop: None,
				cost: None,
				version,
				path: None,
				authority: Some(String::from_utf8(AUTHORITY.to_vec()).unwrap()),
				peer_setup_stream: None,
				peer_declared: Some(peer::Peer::default()),
				auth: crate::auth::Handle::new(false),
				early_unis: Vec::new(),
				extensions: Default::default(),
			})
			.expect("start the session");

			moq_net_sim::timeout(std::time::Duration::from_secs(10), driver)
				.await
				.expect_err("the session ended instead of parking after SETUP");

			assert_eq!(
				occurrences(&log, AUTHORITY),
				1,
				"{version:?}: SETUP must carry the authority once"
			);
		}
	}

	#[moq_net_sim::test]
	async fn a_uni_stream_dead_before_its_type_does_not_end_the_session() {
		a_dead_incoming_stream_is_not_fatal(crate::lite::test_transport::DeadStreamSession::unis(1)).await;
	}

	#[moq_net_sim::test]
	async fn a_bidi_stream_dead_before_its_header_does_not_end_the_session() {
		a_dead_incoming_stream_is_not_fatal(crate::lite::test_transport::DeadStreamSession::bis(1)).await;
	}

	/// moxygen resets a subgroup stream it opened but never wrote with its own CANCELLED
	/// (0x1). Over raw QUIC our transport reads that code through the WebTransport space
	/// and cannot map it, which used to end the session.
	#[moq_net_sim::test]
	async fn a_uni_stream_reset_with_an_unmapped_code_does_not_end_the_session() {
		a_dead_incoming_stream_is_not_fatal(crate::lite::test_transport::DeadStreamSession::unis(1).unmapped()).await;
	}

	#[moq_net_sim::test]
	async fn a_bidi_stream_reset_with_an_unmapped_code_does_not_end_the_session() {
		a_dead_incoming_stream_is_not_fatal(crate::lite::test_transport::DeadStreamSession::bis(1).unmapped()).await;
	}

	/// The bytes a publisher writes at the head of a group's unidirectional stream. Built
	/// with the crate's own encoder so the framing can't drift from the decoder the
	/// dispatch loop runs.
	async fn subgroup_header(version: Version, track_alias: u64, sub_group_id: u64) -> Vec<u8> {
		let log = crate::lite::test_transport::Log::default();
		let mut writer = crate::coding::Writer::new(crate::lite::test_transport::SinkSend::new(log.clone()), version);

		writer
			.encode(&ietf::GroupHeader {
				track_alias,
				group_id: 0,
				sub_group_id,
				publisher_priority: 128,
				flags: ietf::GroupFlags {
					has_subgroup: sub_group_id != 0,
					..Default::default()
				},
			})
			.await
			.unwrap();

		let writes = log.writes.lock().unwrap();
		writes.clone()
	}

	/// Run the uni dispatch loop over one incoming stream. Returns what reached the wire,
	/// and the loop's result if that stream ended it.
	async fn dispatch_uni(
		version: Version,
		payload: Vec<u8>,
		retired_alias: Option<u64>,
	) -> (crate::lite::test_transport::Log, Option<Result<(), Error>>) {
		// The peer opens one uni stream and then goes quiet, so
		// the loop is still running when the assertion is taken.
		let session = crate::lite::test_transport::ScriptedSession::new(Vec::new()).with_incoming_unis(vec![payload]);
		drive_unis(version, session, None, retired_alias).await
	}

	/// Run the uni dispatch loop until it stops a stream or ends. `accepted` is a SETUP
	/// already read by [`accept_setup`], with the streams that raced ahead of it.
	async fn drive_unis(
		version: Version,
		session: crate::lite::test_transport::ScriptedSession,
		accepted: Option<PeerSetup<crate::lite::test_transport::ScriptedSession>>,
		retired_alias: Option<u64>,
	) -> (crate::lite::test_transport::Log, Option<Result<(), Error>>) {
		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let log = session.log.clone();

		let (tasks, _task_set) = TaskSet::new();
		let peer_setup = peer::PeerSetup::default();
		let subscriber = Subscriber::new(
			crate::time::Clock::sim(),
			session.clone(),
			origin,
			Control::new(None, true),
			None,
			peer_setup.clone(),
			crate::Hop::new(1).unwrap(),
			None,
			version,
			tasks,
			Default::default(),
		);
		if let Some(alias) = retired_alias {
			subscriber.retire_alias(alias);
		}

		// Held so the trigger side stays alive for as long as the loop runs.
		let (_goaway, goaway) = crate::goaway::Handle::new(false);
		// Without `accepted`, the peer's SETUP has not arrived yet, which is the state a
		// group stream racing ahead of it lands in.
		let read = accepted.is_some();
		let early = accepted.map(|accepted| accepted.early).unwrap_or_default();
		let mut unis = std::pin::pin!(run_unis(
			session,
			subscriber,
			UniSetup {
				peer: Some(peer_setup),
				read,
				early,
				version,
				goaway,
				local_close: Default::default()
			}
		));

		for _ in 0..100 {
			if let std::task::Poll::Ready(result) = futures::poll!(unis.as_mut()) {
				return (log, Some(result));
			}
			if !log.stops().is_empty() {
				break;
			}
			moq_net_sim::sleep(std::time::Duration::from_millis(1)).await;
		}

		(log, None)
	}

	/// A late group must reach the dispatch loop and stop with CANCELLED.
	#[moq_net_sim::test]
	async fn a_group_for_a_retired_alias_is_stopped_with_cancelled() {
		let (log, result) =
			dispatch_uni(Version::Draft19, subgroup_header(Version::Draft19, 7, 0).await, Some(7)).await;

		assert_eq!(
			log.stops(),
			vec![crate::ietf::error::CANCELLED],
			"the group stream must be stopped with the cancelled code",
		);
		assert!(result.is_none(), "one dropped group ended the session: {result:?}");
		assert_eq!(log.closes(), vec![], "one dropped group may not close the session");
	}

	/// A stream whose type has not arrived must not hold up the streams accepted after it.
	///
	/// QUIC opens every lower-numbered stream when a higher one arrives, so the next stream
	/// accepted can be one whose bytes the peer has not sent yet, held back by connection
	/// flow control that the later streams' unread data is using up. Waiting on its type
	/// before reading them deadlocks the connection.
	#[moq_net_sim::test]
	async fn a_silent_stream_does_not_hold_up_the_next() {
		let header = subgroup_header(Version::Draft19, 7, 0).await;
		let session =
			crate::lite::test_transport::ScriptedSession::new(Vec::new()).with_incoming_unis(vec![Vec::new(), header]);
		let (log, result) = drive_unis(Version::Draft19, session, None, Some(7)).await;

		assert_eq!(
			log.stops(),
			vec![crate::ietf::error::CANCELLED],
			"the stream behind the silent one was never read",
		);
		assert!(result.is_none(), "the silent stream ended the session: {result:?}");
	}

	/// A non-zero subgroup is refused on its own stream, never by closing the session.
	#[moq_net_sim::test]
	async fn a_non_zero_subgroup_is_stopped_without_closing_the_session() {
		let (log, result) = dispatch_uni(Version::Draft19, subgroup_header(Version::Draft19, 7, 1).await, None).await;

		assert_eq!(log.stops(), vec![crate::ietf::error::INTERNAL_ERROR]);
		assert!(result.is_none(), "one refused subgroup ended the session: {result:?}");
		assert_eq!(log.closes(), vec![], "one refused subgroup may not close the session");
	}

	/// A stream type encoded for `version`, followed by a few bytes of body.
	fn uni_stream(version: Version, kind: u64) -> Vec<u8> {
		let mut buf = Vec::new();
		let mut w = crate::coding::Encoder::new(&mut buf, version.into());
		w.varint(kind).unwrap();
		w.slice(&[0; 4]);
		buf
	}

	/// Padding is read and dropped: no STOP_SENDING, and the session stays up.
	#[moq_net_sim::test]
	async fn a_padding_stream_is_discarded() {
		for version in [
			Version::Draft18,
			Version::Draft19,
			Version::Draft20,
			Version::Draft21,
			Version::Draft22,
		] {
			let (log, result) = dispatch_uni(version, uni_stream(version, PADDING), None).await;

			assert!(
				log.stops().is_empty(),
				"{version:?}: padding was stopped: {:?}",
				log.stops()
			);
			assert!(result.is_none(), "{version:?}: padding ended the session: {result:?}");
			assert_eq!(log.closes(), vec![], "{version:?}");
		}
	}

	/// A server waiting on the client's SETUP holds the padding and group streams that beat
	/// it, then classifies them once the session starts. The drafts say to buffer early
	/// data; rejecting it reached the wire as INTERNAL_ERROR.
	#[moq_net_sim::test]
	async fn uni_streams_before_setup_are_held_until_it_lands() {
		const VERSION: Version = Version::Draft19;

		let mut setup = Vec::new();
		setup::Setup {
			parameters: ietf::Parameters::default().encode_bytes(VERSION).unwrap(),
		}
		.encode(
			&mut crate::coding::Encoder::new(&mut setup, VERSION.into()),
			crate::Version::Ietf(VERSION),
		)
		.unwrap();

		let mut session = crate::lite::test_transport::ScriptedSession::new(Vec::new()).with_incoming_unis(vec![
			uni_stream(VERSION, PADDING),
			subgroup_header(VERSION, 7, 0).await,
			setup,
		]);
		let accepted = accept_setup(&mut session, VERSION).await.expect("accept the SETUP");
		assert_eq!(accepted.early.len(), 2, "both early streams are held");
		assert!(session.log.stops().is_empty(), "stopped {:?}", session.log.stops());

		// The group answers a retired subscription, so the classifier reaching it shows
		// as CANCELLED; the padding is read to its end and stops nothing.
		let (log, result) = drive_unis(VERSION, session, Some(accepted), Some(7)).await;
		assert_eq!(log.stops(), vec![crate::ietf::error::CANCELLED]);
		assert!(result.is_none(), "an early stream ended the session: {result:?}");
		assert_eq!(log.closes(), vec![]);
	}

	/// An unknown or invalid stream type MUST close the session, so it stops nothing on its own:
	/// the session close takes the stream with it.
	#[moq_net_sim::test]
	async fn an_unknown_uni_type_closes_the_session() {
		for (version, kind) in [
			(Version::Draft19, 0),
			// Padding and uni SETUP arrived in later drafts, so earlier ones do not know them.
			(Version::Draft17, PADDING),
			(Version::Draft16, setup::SETUP_V17),
			// SUBGROUP_HEADER types with the reserved SUBGROUP_ID_MODE (0b11).
			(Version::Draft19, 0x56),
			(Version::Draft14, 0x16),
			// FIRST_OBJECT (0x40) arrived in draft-18.
			(Version::Draft17, 0x50),
		] {
			let (log, result) = dispatch_uni(version, uni_stream(version, kind), None).await;

			let Some(Err(err)) = result else {
				panic!("{version:?}: type {kind:#x} did not end the session: {result:?}");
			};
			assert_eq!(SessionError::from(&err), SessionError::ProtocolViolation, "{version:?}");
			assert!(log.stops().is_empty(), "{version:?}: stopped {:?}", log.stops());
		}
	}

	/// The bidi dispatcher closes the session for an unknown full-width message type.
	#[moq_net_sim::test]
	async fn an_unknown_bidi_type_closes_the_session() {
		for version in [
			Version::Draft17,
			Version::Draft18,
			Version::Draft19,
			Version::Draft20,
			Version::Draft21,
			Version::Draft22,
		] {
			for kind in [0u64, 1 << 53] {
				let mut payload = Vec::new();
				let mut w = crate::coding::Encoder::new(&mut payload, version.into());
				w.varint(kind).unwrap();
				w.u16(0);
				let session =
					crate::lite::test_transport::ScriptedSession::new(Vec::new()).with_incoming_bidis(vec![payload]);
				let log = session.log.clone();
				let (driver, _goaway, _) = start(Config {
					runtime: crate::time::Clock::sim(),
					session,
					setup: None,
					request_id_max: None,
					limits: Default::default(),
					client: false,
					publish: None,
					subscribe: None,
					peer_hop: None,
					cost: None,
					version,
					path: None,
					authority: None,
					peer_setup_stream: None,
					peer_declared: Some(peer::Peer::default()),
					auth: crate::auth::Handle::new(false),
					early_unis: Vec::new(),
					extensions: Default::default(),
				})
				.unwrap();

				let err = moq_net_sim::timeout(std::time::Duration::from_secs(10), driver)
					.await
					.expect("unknown bidi type must end the session")
					.expect_err("unknown bidi type must fail the session");
				assert_eq!(
					SessionError::from(&err),
					SessionError::ProtocolViolation,
					"{version:?}: {kind:#x}"
				);
				assert_eq!(
					log.closes(),
					vec![(SessionError::ProtocolViolation.to_code(), err.to_string())]
				);
			}
		}
	}

	#[moq_net_sim::test]
	async fn an_invalid_group_order_closes_the_session() {
		for version in [Version::Draft18, Version::Draft21, Version::Draft22] {
			for (value, nested) in [
				(0u8, false),
				(3, false),
				(255, false),
				(0, true),
				(3, true),
				(255, true),
			] {
				// FILL_PARAMETERS starts in draft-20.
				if nested && version == Version::Draft18 {
					continue;
				}
				let mut body = vec![0, 1, 1, b'a', 1, b'b', 1];
				if nested {
					body.extend([0x23, 3, 1, 0x22, value]);
				} else {
					body.extend([0x22, value]);
				}
				let mut payload = Vec::new();
				let mut w = crate::coding::Encoder::new(&mut payload, version.into());
				w.varint(ietf::Subscribe::ID).unwrap();
				w.u16(body.len() as u16);
				w.slice(&body);
				let session =
					crate::lite::test_transport::ScriptedSession::new(Vec::new()).with_incoming_bidis(vec![payload]);
				let log = session.log.clone();
				let (driver, _goaway, _) = start(Config {
					runtime: crate::time::Clock::sim(),
					session,
					setup: None,
					request_id_max: None,
					client: false,
					publish: None,
					subscribe: None,
					peer_hop: None,
					cost: None,
					version,
					path: None,
					authority: None,
					peer_setup_stream: None,
					peer_declared: Some(peer::Peer::default()),
					auth: crate::auth::Handle::new(false),
					early_unis: Vec::new(),
					extensions: Default::default(),
					limits: Default::default(),
				})
				.unwrap();
				let err = moq_net_sim::timeout(std::time::Duration::from_secs(10), driver)
					.await
					.expect("invalid GROUP_ORDER must close the session")
					.expect_err("invalid GROUP_ORDER must fail");
				assert_eq!(SessionError::from(&err), SessionError::ProtocolViolation);
				assert_eq!(
					log.closes(),
					vec![(SessionError::ProtocolViolation.to_code(), err.to_string())]
				);
			}
		}
	}

	/// A peer's advertisement of `room/host`, then two namespace-keyed withdrawals of it.
	/// The second has no advertisement left to name.
	async fn publish_namespace_then_two_withdrawals(version: Version) -> Vec<u8> {
		let log = crate::lite::test_transport::Log::default();
		let mut writer = crate::coding::Writer::new(crate::lite::test_transport::SinkSend::new(log.clone()), version);

		writer.varint(ietf::PublishNamespace::ID).await.unwrap();
		writer
			.encode(&ietf::PublishNamespace {
				request_id: RequestId(1),
				track_namespace: crate::Path::new("room/host"),
				cluster: None,
				authorization_token: None,
			})
			.await
			.unwrap();

		for _ in 0..2 {
			writer.varint(ietf::PublishNamespaceDone::ID).await.unwrap();
			writer
				.encode(&ietf::PublishNamespaceDone {
					track_namespace: crate::Path::new("room/host"),
					request_id: RequestId(0),
				})
				.await
				.unwrap();
		}

		let writes = log.writes.lock().unwrap();
		writes.clone()
	}

	/// The acknowledgement draft-14 answers a PUBLISH_NAMESPACE with, as bytes to look
	/// for in what we wrote.
	async fn publish_namespace_ok(request_id: RequestId) -> Vec<u8> {
		let log = crate::lite::test_transport::Log::default();
		let mut writer = crate::coding::Writer::new(
			crate::lite::test_transport::SinkSend::new(log.clone()),
			Version::Draft14,
		);

		writer.varint(ietf::PublishNamespaceOk::ID).await.unwrap();
		writer.encode(&ietf::PublishNamespaceOk { request_id }).await.unwrap();

		let writes = log.writes.lock().unwrap();
		writes.clone()
	}

	/// draft-14 names its withdrawals rather than numbering them, so a repeat has no
	/// advertisement left to resolve to. Dropping it is what keeps the session up: failing
	/// the lookup takes the whole connection down over a message with nothing left to do.
	///
	/// Driven through `start` because only the full loop shows the consequence, the read
	/// task propagating the classifier's error.
	#[moq_net_sim::test]
	async fn a_repeated_publish_namespace_done_does_not_end_the_session() {
		const VERSION: Version = Version::Draft14;

		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let consumer = origin.consume();

		let mut session =
			crate::lite::test_transport::ScriptedSession::new(publish_namespace_then_two_withdrawals(VERSION).await);
		let log = session.log.clone();
		let setup = Stream::open(&mut session, VERSION)
			.await
			.expect("open the control stream");

		let (driver, _goaway, _) = start(Config {
			runtime: crate::time::Clock::sim(),
			session,
			setup: Some(setup),
			request_id_max: None,
			limits: Default::default(),
			client: true,
			publish: None,
			subscribe: Some(origin),
			peer_hop: None,
			cost: None,
			version: VERSION,
			path: None,
			authority: None,
			peer_setup_stream: None,
			peer_declared: None,
			auth: crate::auth::Handle::new(false),
			early_unis: Vec::new(),
			extensions: Default::default(),
		})
		.expect("start the session");
		let driver = moq_net_sim::spawn(driver);

		let accepted = publish_namespace_ok(RequestId(1)).await;
		for _ in 0..ANNOUNCE_TURNS {
			if occurrences(&log, &accepted) > 0 && consumer.get_broadcast("room/host").is_none() {
				break;
			}
			moq_net_sim::sleep(std::time::Duration::from_millis(1)).await;
		}

		assert_eq!(occurrences(&log, &accepted), 1, "the advertisement was not accepted");
		assert!(
			consumer.get_broadcast("room/host").is_none(),
			"the withdrawal did not reach the request that advertised it"
		);
		assert!(!driver.is_finished(), "the repeated withdrawal ended the session");
		assert!(log.closes().is_empty(), "closed the session: {:?}", log.closes());
	}

	/// A SUBSCRIBE for `room/video` whose parameters are one AUTHORIZATION TOKEN holding
	/// `token`, framed as a control message.
	fn subscribe_with_token(version: Version, token: &[u8]) -> Vec<u8> {
		let msg = ietf::Subscribe {
			request_id: RequestId(0),
			track_namespace: crate::Path::new("room"),
			track_name: "video".into(),
			subscriber_priority: 128,
			group_order: ietf::GroupOrder::Descending,
			filter: ietf::Filter::NextObject,
			fill: None,
			properties_wanted: true,
			forward: true,
			range_filters: false,
			authorization_token: None,
		};
		let mut body = Vec::new();
		let mut w = crate::coding::Encoder::new(&mut body, version.into());
		match version {
			Version::Draft14 => {
				msg.encode_msg(&mut w, version).unwrap();
				// Replace the empty parameter block with the token.
				assert_eq!(body.pop(), Some(0));
			}
			// The request ID, namespace and name; every parameter but the token defaults.
			_ => w.slice(&[0, 1, 4, b'r', b'o', b'o', b'm', 5, b'v', b'i', b'd', b'e', b'o']),
		}
		let mut w = crate::coding::Encoder::new(&mut body, version.into());
		w.varint(1).unwrap();
		w.varint(u64::from(ietf::ParameterBytes::AuthorizationToken)).unwrap();
		w.bytes(token).unwrap();

		let mut payload = Vec::new();
		let mut w = crate::coding::Encoder::new(&mut payload, version.into());
		w.varint(ietf::Subscribe::ID).unwrap();
		w.u16(body.len() as u16);
		w.slice(&body);
		payload
	}

	/// A request's AUTHORIZATION TOKEN decodes by the SETUP option's rules, so one this
	/// endpoint cannot use closes the session with the draft's code. Draft-14 carries the
	/// request on the control stream, a strict draft on its own stream.
	#[moq_net_sim::test]
	async fn a_request_token_that_cannot_be_used_closes_the_session() {
		for version in [Version::Draft14, Version::Draft22] {
			for (token, expected, why) in [
				(
					&[0x01, 0x07, 0x00][..],
					SessionError::AuthTokenCacheOverflow,
					"REGISTER",
				),
				(&[0x00, 0x07][..], SessionError::UnknownAuthTokenAlias, "DELETE"),
				(&[0x02, 0x07][..], SessionError::UnknownAuthTokenAlias, "USE_ALIAS"),
				(
					&[0x03][..],
					SessionError::KeyValueFormatting,
					"USE_VALUE without a type",
				),
				(
					&[0x04, 0x00][..],
					SessionError::KeyValueFormatting,
					"an unknown alias type",
				),
			] {
				let payload = subscribe_with_token(version, token);
				let mut session = match version {
					Version::Draft14 => crate::lite::test_transport::ScriptedSession::new(payload),
					_ => {
						crate::lite::test_transport::ScriptedSession::new(Vec::new()).with_incoming_bidis(vec![payload])
					}
				};
				let log = session.log.clone();
				let setup = match version {
					Version::Draft14 => Some(Stream::open(&mut session, version).await.unwrap()),
					_ => None,
				};
				let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
				let (driver, _goaway, _) = start(Config {
					runtime: crate::time::Clock::sim(),
					session,
					setup,
					request_id_max: None,
					limits: Default::default(),
					client: false,
					publish: Some(origin.consume()),
					subscribe: None,
					peer_hop: None,
					cost: None,
					version,
					path: None,
					authority: None,
					peer_setup_stream: None,
					peer_declared: Some(peer::Peer::default()),
					auth: crate::auth::Handle::new(false),
					extensions: Default::default(),
					early_unis: Vec::new(),
				})
				.unwrap();

				let err = moq_net_sim::timeout(std::time::Duration::from_secs(10), driver)
					.await
					.unwrap_or_else(|_| panic!("{version:?} {why}: the session stayed up"))
					.expect_err("the token must fail the session");
				assert_eq!(SessionError::from(&err), expected, "{version:?} {why}");
				assert_eq!(
					log.closes(),
					vec![(expected.to_code(), err.to_string())],
					"{version:?} {why}"
				);
			}
		}
	}
}
