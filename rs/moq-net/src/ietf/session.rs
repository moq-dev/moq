use crate::driver::Transport;
use crate::origin;
use crate::transport::poll::Session as _;
use crate::{
	Error, Hop, SessionError, StreamError,
	coding::{Decode, DecodeError, Encode, Reader, Stream, Writer},
	ietf::{self, FetchHeader, RequestId},
	setup,
	util::{MaybeBoxedExt, MaybeSendBox, TaskSet, err_only},
};

use super::{
	Control, Message, Publisher, Subscriber, Version, active_count, auth, cluster, epoch, hidden, max_age, peer,
	request_update, solicit, subscriber::is_protocol_violation,
};

/// Keep the session's SUBSCRIBE_NAMESPACE requests open as `discovery` decides, each on
/// a stream from `open`; with no subscribe half, ask nothing. Only an error ends it.
async fn run_namespaces<F, O>(
	subscriber: Subscriber,
	discovery: Option<crate::discovery::Discovery>,
	open: F,
) -> Result<(), Error>
where
	F: Fn() -> O,
	O: std::future::Future<Output = Result<Stream<Transport, Version>, Error>>,
{
	match discovery {
		Some(discovery) => subscriber.run_namespaces(discovery, open).await,
		None => std::future::pending().await,
	}
}

/// Everything one moq-transport session needs to start.
pub struct Config {
	/// The runtime that arms the session's timers.
	pub runtime: crate::time::Deadlines,

	/// Where the session's control traffic is counted, even with neither origin set.
	pub stats: crate::stats::Session,

	pub session: Transport,

	/// What the peer may make this session hold.
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
	/// A peer that does not negotiate the MoQ Cluster extension carries no cost at all, so
	/// nothing is charged and its routes rank on hop count alone.
	pub cost: Option<u64>,

	pub version: Version,

	/// The request path we advertise in our SETUP (clients on URL-less
	/// transports). A server passes `None`.
	pub path: Option<String>,

	/// The URI authority we advertise in our SETUP, under the same rules as `path`.
	pub authority: Option<String>,

	/// The connection credential our SETUP's `AUTHORIZATION TOKEN` option presents. A
	/// server passes `None`.
	pub token: Option<crate::setup::Token>,

	/// The peer's SETUP stream, when it was already read before [`start`] (a server
	/// that gated on the client's path via [`accept_setup`]). It becomes the
	/// GOAWAY channel; `None` lets the uni loop read the SETUP itself.
	pub peer_setup_stream: Option<Reader<crate::transport::boxed::RecvStream, crate::Version>>,

	/// What that pre-read SETUP declared, so the session does not have to parse it
	/// twice. `None` when [`Self::peer_setup_stream`] is.
	pub peer_declared: Option<peer::Peer>,

	/// The session's auth handle, created before [`start`] so a server can take the
	/// peer's token requests during its handshake. Supports AUTH exactly when the
	/// version can negotiate it and this side offers it; the peer's SETUP decides whether
	/// it does.
	pub auth: crate::auth::Link,

	/// Uni streams that arrived before that pre-read SETUP (see [`PeerSetup::early`]),
	/// classified by the session before it accepts any more.
	pub early_unis: Vec<Reader<crate::transport::boxed::RecvStream, crate::Version>>,

	/// The extensions our SETUP offers. MoQ Auth is offered only while [`Self::auth`]
	/// supports it too.
	pub extensions: crate::setup::Extensions,

	/// The Implementation our SETUP advertises, or `None` to omit it.
	pub implementation: Option<String>,

	/// Where the implementation the peer's SETUP advertises is published, once the uni loop
	/// reads it. A caller that pre-read the SETUP has already filled it.
	pub peer_implementation: crate::setup::PeerImplementation,
}

pub(crate) struct Driver {
	pub(crate) withdrawal: crate::session::Withdrawal,
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

pub fn start(config: Config) -> Result<(Driver, crate::goaway::Handle, Option<crate::auth::Credential>), Error> {
	let Config {
		runtime,
		stats,
		mut session,
		limits,
		client,
		publish,
		subscribe,
		peer_hop,
		cost,
		version,
		path,
		authority,
		token,
		peer_setup_stream,
		peer_declared,
		auth,
		early_unis,
		extensions,
		implementation,
		peer_implementation,
	} = config;

	// GOAWAY wiring: the public Session holds one half (drain trigger, received
	// signal), the protocol tasks below hold the other.
	// A moq-transport client MUST send an empty New Session URI: it cannot tell a
	// server to open connections (draft-22 section 9.2).
	let (goaway_handle, goaway) = crate::goaway::Handle::new(!client);

	// What the peer's connection credential earns by default, from the caller's real
	// handles before the empty-half defaulting below.
	let peer_grant = auth::peer_grant(publish.as_ref(), subscribe.as_ref());

	// Present the connection's own credential (the empty token) right away, so both
	// sides learn their grant without waiting on the app. The peer's SETUP then decides
	// whether it is ever sent.
	// A handle that already knows the peer declined (a gated server accept) skips it.
	let setup_token = match auth.supported() {
		true => auth.present(bytes::Bytes::new(), true).ok(),
		false => None,
	};

	// Which prefixes to SUBSCRIBE_NAMESPACE, as for moq-lite: a client asks about what
	// its origin's readers watch, a server or relay link about its whole scope. Decided
	// now, so the origin's announce consumers never read as synced before a request goes
	// out. An unset half asks nothing.
	let discovery = subscribe
		.as_ref()
		.map(|origin| crate::discovery::Discovery::new(origin, client, runtime.clone()));

	let withdrawal = crate::session::Withdrawal::default();
	let withdrawing = withdrawal.clone();
	let closing = session.closing().clone();
	let owed = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
	let serving = owed.clone();

	// What the peer declared in its SETUP. Seeded now when that stream was already
	// read (the legacy handshake, or a gated server accept), and filled by the uni
	// loop otherwise.
	let mut peer_setup = peer::PeerSetup::new(extensions).with_implementation(peer_implementation);
	let setup_read = peer_declared.is_some();
	match peer_declared {
		Some(declared) => peer_setup.set(declared),
		// A client is live before the server's SETUP, which follows its check of the
		// credential.
		None if client => peer_setup = peer_setup.accepting(auth.clone()),
		None => {}
	}

	let credential = setup_token.clone();
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
		let publish = publish.unwrap_or_else(|| {
			origin::Producer::empty(Hop::random())
				.consume()
				.with_stats(stats.clone())
		});
		let subscribe = subscribe.unwrap_or_else(|| origin::Producer::empty(Hop::random()).with_stats(stats.clone()));
		// Counted for the peak only: stream credit bounds them.
		let subscriptions =
			crate::session::Slots::default().with_stats(publish.stats(), crate::stats::Cap::Subscriptions);
		let announces =
			crate::session::Slots::new(limits.announces).with_stats(subscribe.stats(), crate::stats::Cap::Announces);

		let res = {
			let control = Control::new(client);
			// Send SETUP and retain its writer for the session's lifetime. Only a failure
			// ends the session; the other branches report how it closed.
			let setup = err_only(run_setup(
				session.clone(),
				version,
				path,
				authority,
				token,
				implementation,
				self_origin,
				cost,
				crate::setup::Extensions {
					auth: extensions.auth && auth.supported(),
					..extensions
				},
				goaway.clone(),
				control.clone(),
				stats.clone(),
			));

			let mut publisher = Publisher::new(
				runtime.clone(),
				session.clone(),
				publish,
				control.clone(),
				peer_hop,
				peer_setup.clone(),
				version,
			)
			.with_auth(auth.clone());
			let (tasks, mut task_set) = TaskSet::new();
			publisher.withdrawal = withdrawing.clone();
			publisher.owed = serving.clone();
			publisher.subscriptions = subscriptions;

			// Only the dialing side fails loud on a publication outside its grant: a
			// server's publish origin is everything the peer may read, not what it
			// intends to push.
			let enforce = {
				let auth = auth.clone();
				let publisher = publisher.clone();
				let session = session.clone();
				async move {
					match client {
						true => enforce_grant(auth, publisher, session).await,
						false => std::future::pending().await,
					}
				}
			};

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
			.with_solicit(extensions.solicit);

			// Our tokens, one Auth request each, once the peer's SETUP negotiates it.
			let present = auth::run_present(
				runtime.clone(),
				session.clone(),
				control.clone(),
				auth.clone(),
				peer_setup.clone(),
				version,
				stats.clone(),
			);
			// Only a side that offered MoQ Auth answers the peer's Auth requests: one that
			// arrives otherwise falls through to the protocol violation in `run_dispatch`.
			let serve = auth.supported().then(|| auth::Serve {
				runtime: runtime.clone(),
				handle: auth.clone(),
				peer_grant,
			});
			subscriber.announces = announces;

			// When the peer's SETUP was pre-read (a gated server accept), monitor
			// GOAWAY on that stream here; otherwise `run_unis` does it when the SETUP
			// arrives on the wire.
			let goaway_recv = {
				let goaway = goaway.clone();
				let stats = stats.clone();
				async move {
					match peer_setup_stream {
						Some(reader) => {
							run_goaway(reader.with_stats(&stats).with_version(version), version, client, goaway).await
						}
						None => std::future::pending().await,
					}
				}
			};

			// Every half only ends the session on error (err_only parks on clean
			// completion); `setup` never resolves (it holds the stream open) and the
			// task set draining is the one clean exit.
			let mut unis = Unis::new(
				session.clone(),
				subscriber.clone(),
				UniSetup {
					peer: Some(peer_setup.clone()),
					read: setup_read,
					early: early_unis,
					client,
					version,
					goaway,
				},
			);
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
			let mut present = std::pin::pin!(err_only(present));
			let mut enforce = std::pin::pin!(err_only(enforce));
			let mut setup = std::pin::pin!(setup);
			// Unsolicited PUBLISH_NAMESPACE unless the peer requires solicitation;
			// see `Publisher::run_publish_namespaces`.
			let mut pub_ns_run = std::pin::pin!(err_only(publisher.clone().run_publish_namespaces()));
			let sub_ns_run = run_namespaces(subscriber.clone(), discovery, {
				let session = session.clone();
				move || {
					let mut session = session.clone();
					async move { Stream::open_request(&mut session, version).await }
				}
			});
			let mut sub_ns_run = std::pin::pin!(err_only(sub_ns_run));

			let res = kio::wait(|waiter| {
				use std::task::Poll;
				if let Poll::Ready(res) = unis.poll(waiter) {
					return Poll::Ready(res);
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
				// Malformed replies are session-fatal, unlike token refusals.
				if let Poll::Ready(err) = waiter.poll_future(present.as_mut()) {
					return Poll::Ready(Err(err));
				}
				if let Poll::Ready(err) = waiter.poll_future(enforce.as_mut()) {
					return Poll::Ready(Err(err));
				}
				if let Poll::Ready(err) = waiter.poll_future(setup.as_mut()) {
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
			close(&mut session, &res);
			// Before this arm's Auth serve tasks drop, so each settles with the
			// session's error rather than a bare cancel.
			auth.close(match &res {
				Ok(()) => Error::Cancel,
				Err(err) => err.clone(),
			});
			if !closing.is_local() {
				kio::wait(|waiter| unis.poll_drain(waiter)).await;
			}
			if closing.is_local() {
				subscriber.close();
			} else if let Err(err) = &res {
				// Every track this session was receiving ends with its error.
				subscriber.abort(err);
			}
			res
		};

		auth.close(match &res {
			Ok(()) => Error::Cancel,
			Err(err) => err.clone(),
		});

		res
	}
	.maybe_boxed();

	Ok((
		Driver {
			withdrawal,
			owed,
			future: driver,
		},
		goaway_handle,
		credential,
	))
}

/// Close the session over the driver's result, while its streams are still open.
///
/// Dropping them first would reset the control stream ahead of the close, and a strict peer
/// closes over that reset with its own PROTOCOL_VIOLATION before it reads our code.
fn close(session: &mut Transport, res: &Result<(), Error>) {
	match res {
		Err(err @ (Error::Transport(_) | Error::IdleTimeout)) => {
			tracing::info!(%err, "session terminated");
			session.abort(err, super::error::to_session_code(&SessionError::Internal), "");
		}
		Err(err) => {
			tracing::warn!(%err, "session error");
			session.abort(
				err,
				super::error::to_session_code(&SessionError::from(err)),
				err.to_string().as_ref(),
			);
		}
		Ok(()) => {
			tracing::info!("session closed");
			session.abort(&Error::Cancel, super::error::to_session_code(&SessionError::Cancel), "");
		}
	}
}

/// What a peer's SETUP told us, beyond the stream it arrived on.
pub struct PeerSetup {
	/// The SETUP stream, which becomes the GOAWAY channel.
	pub stream: Reader<crate::transport::boxed::RecvStream, crate::Version>,

	/// The SETUP's encoded size, counted once the session's stats are known.
	pub size: usize,

	/// The request path the peer advertised, for URL-less transports.
	pub path: Option<String>,

	/// The credential the peer presented in its `AUTHORIZATION TOKEN` option.
	pub token: Option<crate::setup::Token>,

	/// The implementation the peer advertised in its `IMPLEMENTATION` option.
	pub implementation: Option<String>,

	/// The Setup Options it declared (see [`cluster`] and [`solicit`]).
	pub declared: peer::Peer,

	/// Uni streams that arrived before the SETUP, type peeked but unread. The drafts say
	/// to buffer early data until the control streams arrive, so the session classifies
	/// these once it starts. QUIC stream credit bounds how many can pile up.
	pub early: Vec<Reader<crate::transport::boxed::RecvStream, crate::Version>>,
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

/// Server: read the peer's SETUP off its uni stream before starting the
/// session, returning that stream plus what it declared.
///
/// Blocks on the peer's Setup Stream. Any other uni stream racing ahead of it (padding,
/// or group data for a subscription the peer already holds) is held in
/// [`PeerSetup::early`] for the session to classify. Pass the returned reader to
/// [`start`] as its `peer_setup_stream` so GOAWAY monitoring continues without
/// re-reading it.
pub async fn accept_setup(session: &mut Transport, version: Version) -> Result<PeerSetup, Error> {
	let outer_version = crate::Version::Ietf(version);
	let mut early = Vec::new();

	loop {
		let recv = session.accept_uni().await.map_err(Error::from_transport)?;
		let mut reader: Reader<crate::transport::boxed::RecvStream, crate::Version> = Reader::new(recv, outer_version);

		let kind = match reader.varint_peek().await {
			Ok(kind) => kind,
			Err(err) if died_before_header(&err) => {
				tracing::debug!(%err, "dropping uni stream that died before its type");
				continue;
			}
			Err(err) => return Err(err),
		};
		if kind != setup::SETUP {
			early.push(reader);
			continue;
		}

		reader.set_priority(crate::priority::RECV_CONTROL);
		let (setup, size) = reader.decode_sized::<setup::Setup>().await?;
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
		let implementation =
			crate::setup::decode_implementation(params.get_bytes(ietf::ParameterBytes::Implementation))?;
		let declared = peer_from_params(&params, version)?;

		return Ok(PeerSetup {
			stream: reader,
			size,
			path,
			token,
			implementation,
			declared,
			early,
		});
	}
}

/// Parse the Setup Options we act on out of a raw SETUP parameter block, refusing any the
/// peer's role may not send.
pub(crate) fn decode_peer_setup(
	parameters: bytes::Bytes,
	version: Version,
	client: bool,
) -> Result<(peer::Peer, Option<String>), Error> {
	let (params, _) = ietf::Parameters::decode_slice(&parameters, version)?;
	// A server sends neither PATH nor AUTHORITY (draft-22 sections 9.1.1 and 9.1.2).
	if client && params.get_bytes(ietf::ParameterBytes::Path).is_some() {
		return Err(SessionError::InvalidPath.into());
	}
	if client && params.get_bytes(ietf::ParameterBytes::Authority).is_some() {
		return Err(SessionError::InvalidAuthority.into());
	}
	// Nothing here uses the peer's token, but one that cannot be decoded still closes the
	// session (draft-22 section 8.9).
	super::token::from_setup(&params, version)?;
	let implementation = crate::setup::decode_implementation(params.get_bytes(ietf::ParameterBytes::Implementation))?;
	Ok((peer_from_params(&params, version)?, implementation))
}

/// The Setup Options we act on, out of an already-decoded parameter block. One place, so
/// a future option reaches both the pre-read accept path and the uni loop.
fn peer_from_params(params: &ietf::Parameters, version: Version) -> Result<peer::Peer, crate::DecodeError> {
	Ok(peer::Peer {
		cluster: cluster::peer_from_setup(params)?,
		solicit: solicit::from_setup(params, version)?,
		hidden: hidden::from_setup(params, version),
		max_age: max_age::from_setup(params, version),
		auth: auth::from_setup(params) == Some(true),
		active_count: active_count::from_setup(params),
		epoch: epoch::from_setup(params),
		flate: super::flate::from_setup(params)?,
	})
}

/// Send our SETUP on a uni stream and keep it alive: this stream is also our GOAWAY
/// channel, so a fired drain trigger encodes the GOAWAY here.
///
/// `path` is the request path we advertise (clients on URL-less transports); a
/// server passes `None`, as it does for `token`, the client's connection credential.
/// `self_origin` and `cost` are the MoQ Cluster options, which
/// declare our identity and (client-only) what this link costs to cross. `extensions`
/// names the other extensions we offer.
#[expect(
	clippy::too_many_arguments,
	reason = "private plumbing; a single-use argument struct would only rename the parameters"
)]
async fn run_setup(
	session: Transport,
	version: Version,
	path: Option<String>,
	authority: Option<String>,
	token: Option<crate::setup::Token>,
	implementation: Option<String>,
	self_origin: Hop,
	cost: Option<u64>,
	extensions: crate::setup::Extensions,
	goaway: crate::goaway::Protocol,
	control: Control,
	stats: crate::stats::Session,
) -> Result<(), Error> {
	let outer_version = crate::Version::Ietf(version);

	let send = session
		.open_uni(Default::default())
		.await
		.map_err(Error::from_transport)?;
	let mut writer: Writer<crate::transport::boxed::SendStream, crate::Version> =
		Writer::new(send, outer_version).with_stats(&stats);

	let mut parameters = ietf::Parameters::default();
	if let Some(implementation) = implementation {
		parameters.set_bytes(ietf::ParameterBytes::Implementation, implementation.into_bytes());
	}
	if let Some(path) = path {
		parameters.set_bytes(ietf::ParameterBytes::Path, path.into_bytes());
	}
	if let Some(authority) = authority {
		parameters.set_bytes(ietf::ParameterBytes::Authority, authority.into_bytes());
	}
	if let Some(token) = &token {
		super::token::into_setup(&mut parameters, token, version)?;
	}
	cluster::peer_into_setup(&mut parameters, self_origin, cost);
	if extensions.solicit {
		solicit::into_setup(&mut parameters, version);
	}
	hidden::into_setup(&mut parameters, version);
	max_age::into_setup(&mut parameters, version);
	if extensions.auth {
		auth::into_setup(&mut parameters);
	}
	active_count::into_setup(&mut parameters);
	if extensions.epoch {
		epoch::into_setup(&mut parameters);
	}
	request_update::into_setup(&mut parameters, version);
	super::flate::into_setup(&mut parameters);
	let parameters = parameters.encode_bytes(version)?;

	writer
		.encode(&setup::Setup { parameters })
		.await
		.map_err(control_stream_error)?;

	// Hold the writer alive until the session closes, sending a GOAWAY if the
	// drain trigger fires meanwhile. The trigger resolves `None` when the session
	// drops without draining; keep holding either way, since a FIN mid-session is
	// a protocol violation the peer closes over.
	let mut untriggered = false;
	let payload = kio::wait(|waiter| {
		if session.poll_closed(waiter).is_ready() {
			return std::task::Poll::Ready(Ok(None));
		}
		if writer.poll_closed(waiter).is_ready() {
			return std::task::Poll::Ready(Err(Error::ProtocolViolation));
		}
		if !untriggered {
			match goaway.poll_triggered(waiter) {
				std::task::Poll::Ready(Some(payload)) => return std::task::Poll::Ready(Ok(Some(payload))),
				std::task::Poll::Ready(None) => untriggered = true,
				std::task::Poll::Pending => {}
			}
		}
		std::task::Poll::Pending
	})
	.await?;

	if let Some(payload) = payload {
		let timeout_ms = payload.timeout.map(|d| d.as_millis() as u64).unwrap_or(0);
		let msg = ietf::GoAway {
			new_session_uri: std::borrow::Cow::Borrowed(payload.uri.as_ref()),
			timeout: timeout_ms,
			request_id: (version == Version::Draft18).then(|| control.goaway()).transpose()?,
		};

		// Frame as [type_id varint][size u16][body], the same shape as the
		// control-stream messages this channel otherwise carries.
		let mut writer = writer.with_version(version);
		writer.encode_message(&msg).await.map_err(control_stream_error)?;
		let mut closed = std::pin::pin!(session.closed());
		kio::wait(|waiter| {
			if waiter.poll_future(closed.as_mut()).is_ready() {
				return std::task::Poll::Ready(Ok(()));
			}
			if writer.poll_closed(waiter).is_ready() {
				return std::task::Poll::Ready(Err(Error::ProtocolViolation));
			}
			std::task::Poll::Pending
		})
		.await?;
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
	/// The peer's SETUP, which then carries its GOAWAY.
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
	/// (draft-22 section 6.4.1, and its equivalent in every draft we negotiate).
	fn classify(kind: u64) -> Option<Self> {
		// SUBGROUP_HEADER types are 0x10-0x1D and 0x30-0x3D, plus 0x40 (FIRST_OBJECT)
		// covering 0x50-0x5D and 0x70-0x7D. A reserved
		// SUBGROUP_ID_MODE (0b11) or a bit the draft lacks is invalid, which MUST close the
		// session too (draft-22 section 11.3.1).
		if ietf::GroupFlags::decode(kind).is_ok() {
			return Some(Self::Subgroup);
		}

		match kind {
			FetchHeader::TYPE => Some(Self::Fetch),
			setup::SETUP => Some(Self::Setup),
			PADDING => Some(Self::Padding),
			_ => None,
		}
	}
}

/// Setup and lifecycle state for the incoming uni stream dispatcher.
struct UniSetup {
	peer: Option<peer::PeerSetup>,
	read: bool,
	/// Streams accepted before the session started, handled before accepting more.
	early: Vec<Reader<crate::transport::boxed::RecvStream, crate::Version>>,
	/// Our role, which decides what the peer's SETUP and GOAWAY may carry.
	client: bool,
	version: Version,
	goaway: crate::goaway::Protocol,
}

/// Accept incoming uni streams, including SETUP and GOAWAY.
///
/// Each stream reads its own type in its own child. QUIC opens every lower-numbered
/// stream once a higher one arrives, so the next stream accepted can be one whose bytes
/// the peer has not sent yet, held back by connection flow control that the later
/// streams' unread data is using up. Waiting here for its type would deadlock the
/// connection.
struct Unis {
	session: Transport,
	uni: Uni,
	early: std::collections::VecDeque<Reader<crate::transport::boxed::RecvStream, crate::Version>>,
	tasks: TaskSet,
	fatal: kio::Shared<Option<Error>>,
	ended: Option<Error>,
}

impl Unis {
	fn new(session: Transport, subscriber: Subscriber, setup: UniSetup) -> Self {
		Self {
			uni: Uni {
				subscriber,
				peer_setup: setup.peer,
				seen_setup: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(setup.read)),
				goaway: setup.goaway,
				client: setup.client,
				version: setup.version,
			},
			session,
			early: setup.early.into(),
			tasks: TaskSet::owned(),
			fatal: Default::default(),
			ended: None,
		}
	}

	fn accept(&mut self, waiter: &kio::Waiter) {
		while self.ended.is_none() {
			let reader = match self.early.pop_front() {
				Some(reader) => reader,
				None => match self.session.poll_accept_uni(waiter) {
					std::task::Poll::Ready(Ok(recv)) => Reader::new(recv, crate::Version::Ietf(self.uni.version)),
					std::task::Poll::Ready(Err(err)) => {
						self.ended = Some(Error::from_transport(err));
						break;
					}
					std::task::Poll::Pending => break,
				},
			};
			let uni = self.uni.clone();
			let fatal = self.fatal.clone();
			self.tasks.push(async move {
				if let Err(err) = uni.serve(reader).await {
					fatal.lock().get_or_insert(err);
				}
			});
		}
	}

	fn poll(&mut self, waiter: &kio::Waiter) -> std::task::Poll<Result<(), Error>> {
		let _ = self.tasks.poll(waiter);
		if let Some(err) = self.fatal.lock().take() {
			return std::task::Poll::Ready(Err(err));
		}
		self.accept(waiter);
		let _ = self.tasks.poll(waiter);
		if let Some(err) = self.fatal.lock().take() {
			return std::task::Poll::Ready(Err(err));
		}
		match &self.ended {
			Some(err) => std::task::Poll::Ready(Err(err.clone())),
			None => std::task::Poll::Pending,
		}
	}

	fn poll_drain(&mut self, waiter: &kio::Waiter) -> std::task::Poll<()> {
		self.accept(waiter);
		let drained = self.tasks.poll(waiter).is_ready();
		if self.ended.is_none() {
			return std::task::Poll::Pending;
		}
		// Every accepted child has first consumed its buffered header. A fill
		// still waiting to be promised or opened can no longer arrive.
		self.uni.subscriber.settle_control();
		if drained {
			std::task::Poll::Ready(())
		} else {
			self.tasks.poll(waiter)
		}
	}
}

/// What serving one incoming uni stream needs from its session.
#[derive(Clone)]
struct Uni {
	subscriber: Subscriber,
	peer_setup: Option<peer::PeerSetup>,
	/// Exactly one SETUP per endpoint.
	seen_setup: std::sync::Arc<std::sync::atomic::AtomicBool>,
	goaway: crate::goaway::Protocol,
	client: bool,
	version: Version,
}

impl Uni {
	/// Read the stream's type and serve it, failing only when the stream breaks the session.
	async fn serve(self, mut reader: Reader<crate::transport::boxed::RecvStream, crate::Version>) -> Result<(), Error> {
		reader = reader.with_stats(self.subscriber.stats());
		let Self {
			mut subscriber,
			peer_setup,
			seen_setup,
			goaway,
			client,
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
		let kind: u64 = match kio::wait(|waiter| reader.poll_varint_peek(waiter)).await {
			Ok(kind) => kind,
			Err(err) if died_before_header(&err) => {
				tracing::debug!(%err, "dropping uni stream that died before its type");
				return Ok(());
			}
			Err(err) => return Err(err),
		};

		let Some(ty) = UniType::classify(kind) else {
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
				reader.set_priority(crate::priority::RECV_CONTROL);
				let msg = reader.decode::<setup::Setup>().await?;
				if let Some(peer_setup) = peer_setup {
					let (declared, implementation) = decode_peer_setup(msg.parameters, version, client)?;
					peer_setup.set(declared);
					peer_setup.set_implementation(implementation);
				}

				// Monitor for GOAWAY after setup completes.
				run_goaway(reader.with_version(version), version, client, goaway).await?;
			}
			UniType::Subgroup => {
				let mut reader = reader.with_version(version);
				let res = subscriber.recv_group(&mut reader).await;
				stop_on_error(&mut reader, res)?;
			}
			// A fill fetch stream carries the head of the group a draft-22 subscription
			// joined part way through. One answering no fill of ours is refused inside.
			UniType::Fetch => {
				let mut reader = reader.with_version(version);
				let res = subscriber.recv_fill(&mut reader).await;
				stop_on_error(&mut reader, res)?;
			}
			// The receiver MUST discard padding. We read it to the end rather than cancel,
			// so a peer probing for bandwidth gets the throughput it is measuring.
			UniType::Padding => {
				while let Ok(Some(_)) = kio::wait(|waiter| reader.poll_read_chunk(waiter, usize::MAX)).await {}
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
		Error::Cancel
			| Error::Stream(_)
			| Error::Remote(_)
			| Error::Transport(_)
			| Error::IdleTimeout
			| Error::Decode(DecodeError::Short)
	)
}

/// Receive QUIC datagrams, each an OBJECT_DATAGRAM for one of our subscriptions.
///
/// A transport without datagrams never delivers one, so this parks. A transport failure
/// or a malformed datagram ends the session.
async fn run_datagrams(session: Transport, subscriber: Subscriber) -> Result<(), Error> {
	if session.max_datagram_size() == 0 {
		return Ok(());
	}
	loop {
		let payload = session.recv_datagram().await.map_err(Error::from_transport)?;
		subscriber.recv_datagram(payload)?;
	}
}

/// Stop a refused data stream, propagating a session-scoped error to session teardown.
pub(super) fn stop_on_error<R: crate::transport::poll::RecvStream>(
	reader: &mut Reader<R, Version>,
	res: Result<(), Error>,
) -> Result<(), Error> {
	let Err(err) = res else {
		return Ok(());
	};
	// An overflowing object ID is a protocol violation, unlike an unsupported gap or a
	// malformed object that costs only its stream.
	if matches!(err, Error::Session(_)) {
		return Err(err);
	}

	tracing::debug!(%err, "uni stream error");
	let reset = match StreamError::from(&err) {
		StreamError::Session(_) => StreamError::Internal,
		reset => reset,
	};
	reader.abort(reset);
	Ok(())
}

/// Accept incoming bidi streams, leaving each request to read its own header.
async fn run_dispatch(
	session: Transport,
	publisher: Publisher,
	subscriber: Subscriber,
	peer_setup: peer::PeerSetup,
	// Answers the peer's Auth requests, on the versions that can negotiate them.
	serve: Option<auth::Serve>,
	version: Version,
) -> Result<(), Error> {
	use std::task::Poll;

	// PUBLISH_NAMESPACE decodes differently once the MoQ Cluster extension is
	// negotiated, so dispatch waits for SETUP, which `run_unis` reads independently.
	let peer = subscriber.peer().await;

	// An AUTH without MoQ Auth negotiated, offered by both sides, is an unknown request,
	// which falls through to the protocol violation in `Bidi::serve`.
	let declared_setup = peer_setup.get().await;
	let serve = match declared_setup.auth {
		true => serve,
		false => None,
	};
	// Whether requests and PUBLISH_NAMESPACE may carry EPOCH (MoQ Epoch).
	let epoch = declared_setup.epoch;

	// From the same slot, so this costs nothing extra: it decides whether an unsolicited
	// advertisement is the peer ignoring our own SETUP (MoQ Solicit).
	let declared = subscriber.solicit().await;

	// Each AUTH serve task settles `Issued::closed` from whatever the auth handle
	// holds when it drops, and the first reason wins. So the error that ends the
	// session has to reach the handle while `tasks` is still alive, or every grant
	// reports a bare cancel instead. `None` when the peer never negotiated AUTH,
	// which leaves no serve task to settle.
	let handle = serve.as_ref().map(|serve| serve.handle.clone());

	let bidi = Bidi {
		publisher,
		subscriber,
		peer,
		epoch,
		declared,
		serve,
		version,
	};
	let mut tasks = TaskSet::owned();
	let fatal = kio::Shared::<Option<Error>>::default();

	let err = loop {
		let accepted = tasks
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
				Stream::poll_accept(&session, version, waiter)
			})
			.await;
		let stream = match accepted {
			Ok(stream) => stream,
			Err(err) => break err,
		};

		// QUIC may expose an earlier stream before its bytes arrive. Reading the
		// header here could block later streams whose unread data holds flow control.
		let bidi = bidi.clone();
		let fatal = fatal.clone();
		tasks.push(async move {
			if let Err(err) = bidi.serve(stream).await {
				fatal.lock().get_or_insert(err);
			}
		});
	};

	// `tasks` still holds the AUTH serve tasks, so the session's error lands on the
	// handle before they drop. The driver closes it again with the same error, which
	// then does nothing.
	if let Some(handle) = &handle {
		handle.close(err.clone());
	}
	Err(err)
}

/// What serving one incoming bidi request needs from its session.
#[derive(Clone)]
struct Bidi {
	publisher: Publisher,
	subscriber: Subscriber,
	peer: cluster::Peer,
	/// Whether both sides negotiated MoQ Epoch.
	epoch: bool,
	declared: Option<bool>,
	serve: Option<auth::Serve>,
	version: Version,
}

impl Bidi {
	/// Read and serve a request, failing only when it breaks the session.
	async fn serve(self, mut stream: Stream<Transport, Version>) -> Result<(), Error> {
		stream = stream.with_stats(self.publisher.stats());
		let Self {
			publisher,
			mut subscriber,
			peer,
			epoch,
			declared,
			serve,
			version,
		} = self;
		let header = async {
			let id = stream.reader.varint().await?;
			// The type arrived, so a body cut short by FIN is malformed, not the stream dying.
			let body = match stream.reader.decode::<ietf::Body>().await {
				Err(Error::Decode(DecodeError::Short)) => return Err(Error::WrongSize),
				body => body?,
			};
			Ok::<_, Error>((id, body))
		}
		.await;
		// Abandoning a request before its header is not a session failure. Bytes
		// that arrive and do not parse, and dispatch errors, remain session-fatal.
		let (id, data) = match header {
			Ok(header) => header,
			Err(err) if died_before_header(&err) => {
				tracing::debug!(%err, "dropping bidi stream that died before its header");
				return Ok(());
			}
			Err(err) => return Err(err),
		};
		stream.reader = stream.reader.with_stats(publisher.stats());
		publisher
			.stats()
			.control(super::message::control(id), data.0.len() + 2, true);
		if matches!(
			id,
			ietf::Subscribe::ID
				| ietf::Fetch::ID
				| ietf::SubscribeNamespace::ID
				| ietf::SubscribeTracks::ID
				| ietf::TrackStatus::ID
				| ietf::Publish::ID
				| ietf::PublishNamespace::ID
				| auth::Auth::ID
		) {
			let request_id = RequestId::decode(&mut data.decoder(version), version)?;
			match publisher.control.accept(request_id) {
				Err(Error::GoingAway) => return refuse_going_away(stream, id, version).await,
				result => {
					result?;
				}
			}
		}
		match id {
			// Publisher handles: Subscribe, Fetch, SubscribeNamespace, SubscribeTracks, TrackStatus
			ietf::Subscribe::ID
			| ietf::Fetch::ID
			| ietf::SubscribeNamespace::ID
			| ietf::SubscribeTracks::ID
			| ietf::TrackStatus::ID => {
				publisher.handle_stream(id, data, stream, epoch)?.await;
			}
			// Subscriber handles: Publish, PublishNamespace
			ietf::Publish::ID | ietf::PublishNamespace::ID => {
				subscriber.handle_stream(id, data, stream, peer, epoch, declared)?.await;
			}
			auth::Auth::ID if let Some(serve) = serve => {
				let mut data = data.decoder(version);
				let msg = auth::Auth::decode_msg(&mut data, version)?;
				if !data.is_empty() {
					return Err(Error::WrongSize);
				}
				serve.run(stream, msg, version).await?;
			}
			_ => {
				tracing::warn!(id, "unexpected bidi stream type");
				return Err(Error::UnexpectedStream);
			}
		}
		Ok(())
	}
}

/// Refuse a request past our GOAWAY cutoff with the answer its kind reads: an AUTH
/// presenter reads only AUTH_OK and AUTH_ERROR.
async fn refuse_going_away(stream: Stream<Transport, Version>, id: u64, version: Version) -> Result<(), Error> {
	if id == auth::Auth::ID {
		return auth::refuse_going_away(stream).await;
	}
	super::request_stream::reject(
		stream,
		ietf::RequestError {
			error_code: super::error::request::to_code(
				&Error::GoingAway,
				super::error::request::Kind::Subscribe,
				version,
			),
			reason_phrase: "going away".into(),
			retry_interval: 0,
		},
	)
	.await
}

/// Monitor the peer's SETUP stream for GOAWAY until the session closes.
/// A FIN or reset of this control stream is a protocol violation.
async fn run_goaway<R: crate::transport::poll::RecvStream>(
	reader: Reader<R, Version>,
	version: Version,
	client: bool,
	goaway: crate::goaway::Protocol,
) -> Result<(), Error> {
	read_goaways(reader, version, client, goaway)
		.await
		.map_err(control_stream_error)
}

/// A control stream cannot end independently of its session, so its own failure is the
/// peer's violation. The session closing or timing out under it passes through.
pub(super) fn control_stream_error(err: Error) -> Error {
	if is_protocol_violation(&err) || matches!(err, Error::Session(_) | Error::Encode(_) | Error::IdleTimeout) {
		err
	} else {
		Error::ProtocolViolation
	}
}

/// Read every message after SETUP on the peer's SETUP stream, which may only be GOAWAY.
///
/// Reading continues past the first GOAWAY until the peer FINs or the session closes.
/// Dropping the reader would STOP_SENDING the peer's SETUP uni stream, which draft-22
/// section 6.4.1 forbids closing at the transport layer mid-session, so a strict peer would
/// tear the session down as a PROTOCOL_VIOLATION in the middle of the drain we are honoring.
async fn read_goaways<R: crate::transport::poll::RecvStream>(
	mut reader: Reader<R, Version>,
	version: Version,
	client: bool,
	goaway: crate::goaway::Protocol,
) -> Result<(), Error> {
	while let Some(id) = reader.varint_maybe().await? {
		// The type arrived, so a body cut short by FIN is malformed, not the stream dying.
		let body = match reader.decode::<ietf::Body>().await {
			Err(Error::Decode(DecodeError::Short)) => return Err(Error::WrongSize),
			body => body?,
		};
		if id != ietf::GoAway::ID {
			return Err(Error::UnexpectedMessage);
		}

		let msg: ietf::GoAway = body.message(version)?;
		reader
			.stats()
			.control(crate::stats::Message::Goaway, body.0.len() + 2, true);
		tracing::info!(message = ?msg, "received GOAWAY");

		// Draft-18 names the first of our own Request IDs it did not process: even for a
		// client, odd for a server. The field is required here, on the control stream.
		match msg.request_id {
			None if version == Version::Draft18 => return Err(Error::ProtocolViolation),
			Some(request_id) if request_id.0 % 2 != u64::from(!client) => {
				return Err(DecodeError::Session(SessionError::InvalidRequestId).into());
			}
			_ => {}
		}

		// Nothing may follow a GOAWAY on this stream, so bytes already here are a violation.
		// Caught before the GOAWAY surfaces: the application acting on it can close the
		// session first, without our code.
		let waiter = &kio::Waiter::noop();
		if let std::task::Poll::Ready(Ok(true)) = reader.poll_has_more(waiter) {
			return Err(Error::ProtocolViolation);
		}

		let timeout = (msg.timeout > 0).then(|| std::time::Duration::from_millis(msg.timeout));
		// A second GOAWAY is a protocol violation the draft requires we close over.
		goaway.record(crate::goaway::Goaway {
			uri: msg.new_session_uri.into_owned(),
			timeout,
		})?;
	}
	Err(Error::ProtocolViolation)
}

/// Closes the auth handle when the session driver ends without finishing, releasing
/// anything still waiting on a token.
struct AuthClose(crate::auth::Link);

impl Drop for AuthClose {
	fn drop(&mut self) {
		self.0.close(Error::Cancel);
	}
}

/// Close the session when we publish a broadcast our grant never covered, instead of
/// leaving it to wait for a subscription that never comes. See [`crate::auth::Enforce`].
///
/// Judges the publisher's serving origin rather than the raw one: an origin that also
/// consumes holds what the peer sent us, which is not ours to publish back.
async fn enforce_grant(auth: crate::auth::Link, publisher: Publisher, session: Transport) -> Result<(), Error> {
	let origin = publisher.serving_origin().await;
	// Checking what we publish is not a local reader asking to discover anything.
	let mut announced = origin.clone().serving().announced();
	let mut check = crate::auth::Enforce::default();
	let Some(path) = kio::wait(|waiter| check.poll(&auth, &mut announced, waiter)).await else {
		return Ok(());
	};
	tracing::error!(broadcast = %crate::path::Rooted(origin.root(), &path), "publishing outside our grant; closing the session");
	let err = Error::Unauthorized;
	session.close(
		super::error::to_session_code(&SessionError::from(&err)),
		&crate::auth::unauthorized_reason(&path),
	);
	Err(err)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::model::ProduceTest;

	/// The peer's IMPLEMENTATION option is read alongside what it declared, and one that
	/// is not UTF-8 refuses the SETUP.
	#[test]
	fn peer_setup_reads_the_implementation() {
		let version = Version::Draft22;
		let encode = |value: Option<&[u8]>| {
			let mut params = ietf::Parameters::default();
			if let Some(value) = value {
				params.set_bytes(ietf::ParameterBytes::Implementation, value.to_vec());
			}
			params.encode_bytes(version).unwrap()
		};
		let (_, implementation) = decode_peer_setup(encode(Some(b"moq-net-rs/1")), version, true).unwrap();
		assert_eq!(implementation.as_deref(), Some("moq-net-rs/1"));
		let (_, implementation) = decode_peer_setup(encode(None), version, true).unwrap();
		assert_eq!(implementation, None);
		assert!(decode_peer_setup(encode(Some(&[0xff])), version, true).is_err());
	}

	#[moq_clock::test]
	async fn announce_interest_without_credit_keeps_the_session_alive() {
		use std::sync::atomic::Ordering;
		for version in [Version::Draft18] {
			let origin = origin::Config::new(crate::Hop::new(1).unwrap()).produce();
			let interest = origin.consume().announced();
			let session = crate::lite::test_transport::ScriptedSession::new(Vec::new()).without_credit();
			let log = session.log.clone();
			let session = crate::driver::Transport::new(session);
			let (driver, _goaway, _) = start(Config {
				runtime: crate::time::sim(),
				stats: Default::default(),
				session,
				limits: Default::default(),
				client: true,
				publish: None,
				subscribe: Some(origin),
				peer_hop: None,
				cost: None,
				version,
				path: None,
				authority: None,
				token: None,
				peer_setup_stream: None,
				peer_declared: Some(peer::Peer {
					solicit: Some(true),
					..Default::default()
				}),
				auth: crate::auth::Link::new(false),
				early_unis: Vec::new(),
				implementation: None,
				peer_implementation: Default::default(),
				extensions: Default::default(),
			})
			.unwrap();
			let mut driver = std::pin::pin!(driver);
			assert!(futures::poll!(driver.as_mut()).is_pending(), "{version}");
			assert_eq!(
				log.request_attempts.load(Ordering::Relaxed),
				1,
				"{version}: interest was attempted"
			);
			moq_clock::sleep(std::time::Duration::from_secs(20)).await;
			assert!(
				futures::poll!(driver.as_mut()).is_pending(),
				"{version}: local refusal ended the session"
			);
			assert_eq!(
				log.request_attempts.load(Ordering::Relaxed),
				1,
				"{version}: interest was not retried"
			);
			assert!(log.closes().is_empty());
			assert!(interest.is_synced(), "a refused interest releases its snapshot");
		}
	}

	#[moq_clock::test]
	async fn auth_ids_need_the_peers_parity() {
		for version in [Version::Draft18, Version::Draft22] {
			for client in [false, true] {
				let first = u64::from(client);
				for requests in [vec![(true, first ^ 1)], vec![(false, first), (true, (first + 2) ^ 1)]] {
					let incoming = requests
						.into_iter()
						.map(|(is_auth, id)| {
							let mut body = Vec::new();
							let mut encoder = crate::coding::Encoder::new(&mut body, version.into());
							let kind = if is_auth {
								auth::Auth {
									request_id: RequestId(id),
									token: Default::default(),
								}
								.encode_msg(&mut encoder, version)
								.unwrap();
								auth::Auth::ID
							} else {
								ietf::TrackStatus {
									request_id: RequestId(id),
									track_namespace: crate::Path::from_static("test"),
									track_name: "video".into(),
									properties_wanted: true,
									epoch: None,
								}
								.encode_msg(&mut encoder, version)
								.unwrap();
								ietf::TrackStatus::ID
							};
							let mut script = Vec::new();
							let mut encoder = crate::coding::Encoder::new(&mut script, version.into());
							encoder.varint(kind).unwrap();
							encoder.u16(body.len() as u16);
							script.extend(body);
							script
						})
						.collect();
					let session =
						crate::lite::test_transport::ScriptedSession::new(Vec::new()).with_incoming_bidis(incoming);
					let log = session.log.clone();
					let (driver, _, _) = start(Config {
						runtime: crate::time::sim(),
						stats: Default::default(),
						session: crate::driver::Transport::new(session.clone()),
						limits: Default::default(),
						client,
						publish: None,
						subscribe: None,
						peer_hop: None,
						cost: None,
						version,
						path: None,
						authority: None,
						token: None,
						peer_setup_stream: None,
						peer_declared: Some(peer::Peer {
							auth: true,
							..Default::default()
						}),
						auth: crate::auth::Link::new(true),
						early_unis: Vec::new(),
						implementation: None,
						peer_implementation: Default::default(),
						extensions: Default::default(),
					})
					.unwrap();
					let err = moq_clock::timeout(std::time::Duration::from_secs(10), driver)
						.await
						.expect("invalid AUTH ID left the session open")
						.expect_err("invalid request ID must be fatal");
					assert!(
						matches!(err, Error::Session(SessionError::InvalidRequestId)),
						"{version:?} client={client}: {err:?}"
					);
					assert_eq!(log.closes()[0].0, SessionError::InvalidRequestId.to_code());
				}
			}
		}
	}

	#[moq_clock::test]
	async fn malformed_auth_replies_close_the_session() {
		for version in [Version::Draft22] {
			for (id, body) in [
				(99, vec![]),
				(auth::AuthError::ID, vec![0xc0, 0, 0, 1, 0, 0, 0, 0, 0]),
				(auth::AuthError::ID, vec![1]),
				(auth::AuthError::ID, vec![1, 0, 0]),
				(auth::AuthOk::ID, vec![0, 0, 0xc0, 0x20, 0, 0, 0, 0, 0, 0]),
			] {
				let mut script = Vec::new();
				let mut encoder = crate::coding::Encoder::new(&mut script, version.into());
				encoder.varint(id).unwrap();
				encoder.u16(body.len() as u16);
				script.extend(body);
				let session = crate::lite::test_transport::ScriptedSession::per_stream(vec![script]);
				let log = session.log.clone();
				let (driver, _, _) = start(Config {
					runtime: crate::time::sim(),
					stats: Default::default(),
					session: crate::driver::Transport::new(session.clone()),
					limits: Default::default(),
					client: true,
					publish: None,
					subscribe: None,
					peer_hop: None,
					cost: None,
					version,
					path: None,
					authority: None,
					token: None,
					peer_setup_stream: None,
					peer_declared: Some(peer::Peer {
						auth: true,
						..Default::default()
					}),
					auth: crate::auth::Link::new(true),
					early_unis: Vec::new(),
					implementation: None,
					peer_implementation: Default::default(),
					extensions: Default::default(),
				})
				.unwrap();
				let err = moq_clock::timeout(std::time::Duration::from_secs(10), driver)
					.await
					.expect("the session outlived a malformed AUTH reply")
					.expect_err("malformed AUTH is fatal");
				assert!(is_protocol_violation(&err), "{err:?}");
				assert_eq!(log.closes()[0].0, SessionError::ProtocolViolation.to_code());
			}
		}
	}

	/// A granted token whose Auth request the peer resets is presented again on the same
	/// session, backing off while each attempt is lost before a reply: at 0s, 1s, and 3s.
	#[moq_clock::test]
	async fn a_lost_auth_request_is_presented_again() {
		let version = Version::Draft18;
		let mut grant = Vec::new();
		let mut encoder = crate::coding::Encoder::new(&mut grant, version.into());
		encoder.varint(auth::AuthOk::ID).unwrap();
		encoder.u16(3);
		grant.extend([0, 0, 0]);
		let session = crate::lite::test_transport::ScriptedSession::per_stream(vec![grant.clone(), grant]);
		session.close(crate::lite::test_transport::Close::ResetStream(
			crate::ietf::error::to_stream_code(&crate::StreamError::Internal),
		));
		let log = session.log.clone();
		let handle = crate::auth::Handle::new();
		let (driver, _, _) = start(Config {
			runtime: crate::time::sim(),
			stats: Default::default(),
			session: crate::driver::Transport::new(session.clone()),
			limits: Default::default(),
			client: true,
			publish: None,
			subscribe: None,
			peer_hop: None,
			cost: None,
			version,
			path: None,
			authority: None,
			token: None,
			peer_setup_stream: None,
			peer_declared: Some(peer::Peer {
				auth: true,
				..Default::default()
			}),
			auth: crate::auth::Link::attach(&handle, true, true).unwrap(),
			early_unis: Vec::new(),
			implementation: None,
			peer_implementation: Default::default(),
			extensions: Default::default(),
		})
		.unwrap();
		let token = handle.present(bytes::Bytes::from_static(b"token")).unwrap();

		let res = moq_clock::timeout(std::time::Duration::from_secs(5), driver).await;
		assert!(res.is_err(), "the session ended: {res:?}");
		assert_eq!(occurrences(&log, b"token"), 3);
		assert!(
			futures::FutureExt::now_or_never(token.closed()).is_none(),
			"a lost request ended the token"
		);
	}

	/// A peer that negotiated MoQ Auth but resets a token's request before any reply gave it
	/// nothing: the token fails as unsupported rather than being presented again.
	#[moq_clock::test]
	async fn an_auth_request_reset_before_its_reply_is_unsupported() {
		let version = Version::Draft18;
		let mut grant = Vec::new();
		let mut encoder = crate::coding::Encoder::new(&mut grant, version.into());
		encoder.varint(auth::AuthOk::ID).unwrap();
		encoder.u16(3);
		grant.extend([0, 0, 0]);
		// The setup token is granted; ours is reset before any reply.
		let session = crate::lite::test_transport::ScriptedSession::per_stream(vec![grant, Vec::new()]);
		session.close(crate::lite::test_transport::Close::ResetStream(
			crate::ietf::error::to_stream_code(&crate::StreamError::Internal),
		));
		let log = session.log.clone();
		let handle = crate::auth::Handle::new();
		let (driver, _, _) = start(Config {
			runtime: crate::time::sim(),
			stats: Default::default(),
			session: crate::driver::Transport::new(session.clone()),
			limits: Default::default(),
			client: true,
			publish: None,
			subscribe: None,
			peer_hop: None,
			cost: None,
			version,
			path: None,
			authority: None,
			token: None,
			peer_setup_stream: None,
			peer_declared: Some(peer::Peer {
				auth: true,
				..Default::default()
			}),
			auth: crate::auth::Link::attach(&handle, true, true).unwrap(),
			early_unis: Vec::new(),
			implementation: None,
			peer_implementation: Default::default(),
			extensions: Default::default(),
		})
		.unwrap();
		let token = handle.present(bytes::Bytes::from_static(b"token")).unwrap();

		let res = moq_clock::timeout(std::time::Duration::from_secs(5), driver).await;
		assert!(res.is_err(), "the session ended: {res:?}");
		assert_eq!(occurrences(&log, b"token"), 1, "presented again");
		let err = futures::FutureExt::now_or_never(token.closed()).expect("the token is still open");
		assert!(matches!(err, Error::Unsupported), "{err:?}");
	}

	#[moq_clock::test]
	async fn malformed_presenter_auth_closes_the_session() {
		for version in [Version::Draft22] {
			for body in [vec![0, 2], vec![0, 0, 0]] {
				let mut script = Vec::new();
				let mut encoder = crate::coding::Encoder::new(&mut script, version.into());
				encoder.varint(auth::Auth::ID).unwrap();
				encoder.u16(2);
				script.extend(body);
				let session =
					crate::lite::test_transport::ScriptedSession::new(Vec::new()).with_incoming_bidis(vec![script]);
				let log = session.log.clone();
				let (driver, _, _) = start(Config {
					runtime: crate::time::sim(),
					stats: Default::default(),
					session: crate::driver::Transport::new(session.clone()),
					limits: Default::default(),
					client: false,
					publish: None,
					subscribe: None,
					peer_hop: None,
					cost: None,
					version,
					path: None,
					authority: None,
					token: None,
					peer_setup_stream: None,
					peer_declared: Some(peer::Peer {
						auth: true,
						..Default::default()
					}),
					auth: crate::auth::Link::new(true),
					early_unis: Vec::new(),
					implementation: None,
					peer_implementation: Default::default(),
					extensions: Default::default(),
				})
				.unwrap();
				let err = moq_clock::timeout(std::time::Duration::from_secs(10), driver)
					.await
					.expect("malformed presenter AUTH was ignored")
					.expect_err("malformed AUTH is fatal");
				assert!(is_protocol_violation(&err), "{err:?}");
				assert_eq!(log.closes()[0].0, SessionError::ProtocolViolation.to_code());
			}
		}
	}

	#[test]
	fn object_overflow_is_session_fatal_but_a_stream_error_is_not() {
		let mut reader = Reader::new(
			crate::transport::boxed::RecvStream::new(crate::lite::test_transport::PendingRecv),
			Version::Draft22,
		);
		let err = stop_on_error(&mut reader, Err(SessionError::ProtocolViolation.into())).unwrap_err();
		assert_eq!(SessionError::from(&err), SessionError::ProtocolViolation);
		assert!(stop_on_error(&mut reader, Err(Error::Unsupported)).is_ok());
		// A malformed property overflows too, but costs only its stream.
		assert!(stop_on_error(&mut reader, Err(Error::Decode(DecodeError::BoundsExceeded))).is_ok());
	}

	fn occurrences(log: &crate::lite::test_transport::Log, needle: &[u8]) -> usize {
		let writes = log.writes.lock().unwrap();
		writes.windows(needle.len()).filter(|window| *window == needle).count()
	}

	/// The peer's REQUEST_OK followed by a NAMESPACE in the base form: no cluster
	/// parameters, so no HOP_PATH. Built with the crate's own writer so the framing
	/// can't drift from the encoder under test.
	async fn namespace_without_hop_path(version: Version) -> Vec<u8> {
		let log = crate::lite::test_transport::Log::default();
		let mut writer = crate::coding::Writer::new(
			crate::transport::boxed::SendStream::new(crate::lite::test_transport::SinkSend::new(log.clone())),
			version,
		);

		writer.varint(ietf::RequestOk::ID).await.unwrap();
		writer
			.encode(&ietf::RequestOk {
				largest: None,
				active: None,
			})
			.await
			.unwrap();
		writer.varint(ietf::Namespace::ID).await.unwrap();
		writer
			.encode(&ietf::Namespace {
				suffix: crate::Path::from_static("cam"),
				params: None,
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
	#[moq_clock::test]
	async fn a_namespace_without_a_hop_path_closes_the_session() {
		const VERSION: Version = Version::Draft18;

		// A driver that swallows the violation parks forever instead of failing, so
		// bound it: paused time makes the deadline fire the moment nothing else can run.

		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		// A client asks only about what is watched.
		let _watch = origin.consume().announced();
		let session = crate::lite::test_transport::ScriptedSession::new(namespace_without_hop_path(VERSION).await);
		let log = session.log.clone();

		let (driver, _goaway, _) = start(Config {
			runtime: crate::time::sim(),
			stats: Default::default(),
			session: crate::driver::Transport::new(session.clone()),
			limits: Default::default(),
			client: true,
			publish: None,
			subscribe: Some(origin),
			peer_hop: None,
			cost: None,
			version: VERSION,
			path: None,
			authority: None,
			token: None,
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
			auth: crate::auth::Link::new(false),
			early_unis: Vec::new(),
			implementation: None,
			peer_implementation: Default::default(),
			extensions: Default::default(),
		})
		.expect("start the session");

		let err = moq_clock::timeout(std::time::Duration::from_secs(10), driver)
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

	/// A subscriber watching everything issues one SUBSCRIBE_NAMESPACE per PERMITTED
	/// PREFIX, and asks for those prefixes rather than the root it mounts replies under.
	/// Driven through `start` so the per-prefix fan-out is exercised, not just one stream
	/// in isolation: a loop that opened a single stream would still satisfy a test that
	/// called `run_subscribe_namespace` itself.
	#[moq_clock::test]
	async fn every_permitted_prefix_gets_its_own_subscribe_namespace() {
		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let scope: crate::Patterns = ["cam", "mic"]
			.into_iter()
			.map(|prefix| crate::Pattern::subtree(prefix).unwrap())
			.collect();
		let scoped = origin
			.scope("rootns", &scope)
			.expect("scope the origin to two prefixes");
		let _watch = scoped.consume().announced();

		let gate = kio::Producer::new(true);
		let session = crate::lite::test_transport::SinkSession::gated_bi(gate.consume());
		let log = session.log.clone();

		let (driver, _goaway, _) = start(Config {
			runtime: crate::time::sim(),
			stats: Default::default(),
			session: crate::driver::Transport::new(session.clone()),
			limits: Default::default(),
			client: true,
			publish: None,
			subscribe: Some(scoped),
			peer_hop: None,
			cost: None,
			version: Version::Draft18,
			path: None,
			authority: None,
			token: None,
			peer_setup_stream: None,
			// The requests wait on the peer's SETUP (MoQ Hidden).
			peer_declared: Some(peer::Peer::default()),
			auth: crate::auth::Link::new(false),
			early_unis: Vec::new(),
			implementation: None,
			peer_implementation: Default::default(),
			extensions: Default::default(),
		})
		.expect("start the session");
		let _driver = moq_clock::sim::spawn(driver);

		// Both requests are written before either peer response, which never comes.
		for _ in 0..100 {
			if occurrences(&log, b"cam") > 0 && occurrences(&log, b"mic") > 0 {
				break;
			}
			moq_clock::sleep(std::time::Duration::from_millis(5)).await;
		}

		assert_eq!(occurrences(&log, b"cam"), 1, "one SUBSCRIBE_NAMESPACE for cam");
		assert_eq!(occurrences(&log, b"mic"), 1, "one SUBSCRIBE_NAMESPACE for mic");
		assert_eq!(occurrences(&log, b"rootns"), 0, "asked the peer for our local root");
	}

	/// The bytes of one SUBSCRIBE_NAMESPACE, as the session writes them.
	async fn subscribe_namespace(version: Version, namespace: &str, request_id: RequestId) -> Vec<u8> {
		let log = crate::lite::test_transport::Log::default();
		let mut writer = Writer::new(
			crate::transport::boxed::SendStream::new(crate::lite::test_transport::SinkSend::new(log.clone())),
			version,
		);
		let msg = ietf::SubscribeNamespace {
			request_id,
			namespace: crate::Path::new(namespace).unwrap(),
			hidden: false,
		};
		writer.varint(ietf::SubscribeNamespace::ID).await.unwrap();
		writer.encode(&msg).await.unwrap();
		log.writes.lock().unwrap().clone()
	}

	/// Watch everything under `prefix`.
	fn watch(origin: &crate::origin::Producer, prefix: &str) -> crate::announce::Consumer {
		origin
			.consume()
			.scope(prefix, &crate::Patterns::from(crate::Pattern::all()))
			.unwrap()
			.announced()
	}

	/// Start a client feeding `subscribe` against a peer that answers nothing and declared
	/// `solicit` (MoQ Solicit).
	async fn client(
		version: Version,
		subscribe: crate::origin::Producer,
		solicit: Option<bool>,
	) -> (
		crate::lite::test_transport::Log,
		moq_clock::sim::JoinHandle<Result<(), Error>>,
	) {
		let gate = kio::Producer::new(true);
		let session = crate::lite::test_transport::SinkSession::gated_bi(gate.consume());
		client_on(version, session, subscribe, solicit).await
	}

	/// [`client`] over `session`.
	async fn client_on(
		version: Version,
		session: crate::lite::test_transport::SinkSession,
		subscribe: crate::origin::Producer,
		solicit: Option<bool>,
	) -> (
		crate::lite::test_transport::Log,
		moq_clock::sim::JoinHandle<Result<(), Error>>,
	) {
		let log = session.log.clone();
		let session = crate::driver::Transport::new(session);

		let (driver, _goaway, _) = start(Config {
			runtime: crate::time::sim(),
			stats: Default::default(),
			session,
			limits: Default::default(),
			client: true,
			publish: None,
			subscribe: Some(subscribe),
			peer_hop: None,
			cost: None,
			version,
			path: None,
			authority: None,
			token: None,
			peer_setup_stream: None,
			peer_declared: Some(peer::Peer {
				solicit,
				..Default::default()
			}),
			auth: crate::auth::Link::new(false),
			early_unis: Vec::new(),
			implementation: None,
			peer_implementation: Default::default(),
			extensions: Default::default(),
		})
		.expect("start the session");
		(log, moq_clock::sim::spawn(driver))
	}

	/// Let the driver run until `needle` reaches the wire, or give up after a while.
	async fn sent(log: &crate::lite::test_transport::Log, needle: &[u8]) -> bool {
		for _ in 0..ANNOUNCE_TURNS {
			if occurrences(log, needle) > 0 {
				return true;
			}
			moq_clock::sleep(std::time::Duration::from_millis(1)).await;
		}
		false
	}

	/// A client asks nothing until something is watched, then only about what is.
	#[moq_clock::test]
	async fn a_client_asks_only_about_what_is_watched() {
		for version in [Version::Draft18] {
			let subscribe = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
			let (log, _driver) = client(version, subscribe.clone(), Some(true)).await;
			let first = subscribe_namespace(version, "cam", RequestId(0)).await;
			assert!(
				!sent(&log, &first).await,
				"{version:?} asked before anything was watched"
			);

			let _cam = watch(&subscribe, "cam");
			assert!(sent(&log, &first).await, "{version:?} did not ask about the watch");
			let second = subscribe_namespace(version, "", RequestId(2)).await;
			assert!(
				!sent(&log, &second).await,
				"{version:?} asked about more than the watch"
			);
		}
	}

	/// Each request has a stream of its own, so at a foreign peer the broader one waits for
	/// the peer to close the narrower one's, which ends with a FIN on draft-18 and a
	/// reset on draft-22: a peer that never closes it never sees the two overlap.
	#[moq_clock::test]
	async fn a_broader_watch_waits_for_the_narrower_to_leave_the_wire() {
		for version in [Version::Draft18, Version::Draft22] {
			let subscribe = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
			let _front = watch(&subscribe, "cam/front");
			let (log, _driver) = client(version, subscribe.clone(), None).await;
			assert!(sent(&log, b"front").await, "{version:?}");
			let opened = log.bi_opens();

			let _cam = watch(&subscribe, "cam");
			for _ in 0..ANNOUNCE_TURNS {
				moq_clock::sleep(std::time::Duration::from_millis(1)).await;
			}
			assert_eq!(log.bi_opens(), opened, "{version:?} overlapped the narrower request");
			let ended = log.trail().into_iter().any(|(_, sent)| match sent {
				crate::lite::test_transport::Sent::Finish => super::super::request_stream::fin_cancels(version),
				crate::lite::test_transport::Sent::Reset(_) => !super::super::request_stream::fin_cancels(version),
				crate::lite::test_transport::Sent::Write(_) => false,
			});
			assert!(ended, "{version:?} did not end the narrower request: {:?}", log.trail());
		}
	}

	/// A request ended while it waits on the peer for stream credit holds nothing: the
	/// broader request that replaced it goes out alone once the credit arrives.
	#[moq_clock::test]
	async fn a_request_ended_before_it_opened_does_not_block_the_broader_one() {
		for version in [Version::Draft22] {
			let subscribe = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
			let gate = kio::Producer::new(true);
			let credit = kio::Producer::new(true);
			let session =
				crate::lite::test_transport::SinkSession::gated_bi(gate.consume()).with_open_bi_gate(credit.consume());
			let (log, _driver) = client_on(version, session, subscribe.clone(), None).await;
			*credit.write().ok().unwrap() = false;
			let opened = log.bi_opens();

			let _front = watch(&subscribe, "cam/front");
			for _ in 0..ANNOUNCE_TURNS {
				moq_clock::sleep(std::time::Duration::from_millis(1)).await;
			}
			let _cam = watch(&subscribe, "cam");
			for _ in 0..ANNOUNCE_TURNS {
				moq_clock::sleep(std::time::Duration::from_millis(1)).await;
			}
			*credit.write().ok().unwrap() = true;
			assert!(sent(&log, b"cam").await, "{version:?} never asked the broader prefix");
			assert_eq!(log.bi_opens(), opened + 1, "{version:?} opened the replaced request");
			assert!(!sent(&log, b"front").await, "{version:?} sent the replaced request");
		}
	}

	/// A peer that declared MoQ Solicit is ours and holds overlapping requests, so a broader
	/// watch leaves the narrower request open, as on moq-lite: neither takes the other's
	/// routes over before it has synced.
	#[moq_clock::test]
	async fn a_broader_watch_keeps_the_narrower_open_at_our_peer() {
		for version in [Version::Draft18] {
			let subscribe = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
			let _front = watch(&subscribe, "cam/front");
			let (log, _driver) = client(version, subscribe.clone(), Some(true)).await;
			let asked = subscribe_namespace(version, "cam/front", RequestId(0)).await;
			assert!(sent(&log, &asked).await, "{version:?}");

			let _cam = watch(&subscribe, "cam");
			let broader = subscribe_namespace(version, "cam", RequestId(2)).await;
			assert!(sent(&log, &broader).await, "{version:?} never asked the broader prefix");
			let ended = log
				.trail()
				.iter()
				.any(|(_, sent)| !matches!(sent, crate::lite::test_transport::Sent::Write(_)));
			assert!(!ended, "{version:?} ended the narrower request: {:?}", log.trail());
		}
	}

	/// How many scheduling turns an advertisement gets before the count is taken. Time is
	/// paused in these tests, so each turn costs nothing and only runs the driver until it
	/// parks again; a busy machine cannot turn a slow announce into a passing silence.
	const ANNOUNCE_TURNS: usize = 100;

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
			runtime: crate::time::sim(),
			stats: Default::default(),
			session: crate::driver::Transport::new(session.clone()),
			limits: Default::default(),
			client: true,
			publish: Some(origin.consume()),
			subscribe: None,
			peer_hop: None,
			cost: None,
			version: Version::Draft18,
			path: None,
			authority: None,
			token: None,
			peer_setup_stream: None,
			peer_declared,
			auth: crate::auth::Link::new(false),
			early_unis: Vec::new(),
			implementation: None,
			peer_implementation: Default::default(),
			extensions: Default::default(),
		})
		.expect("start the session");
		let _driver = moq_clock::sim::spawn(driver);

		// Drive until the announce lands, rather than betting on one fixed window.
		for _ in 0..ANNOUNCE_TURNS {
			if occurrences(&log, b"solo-cam") > 0 {
				break;
			}
			moq_clock::sleep(std::time::Duration::from_millis(1)).await;
		}

		occurrences(&log, b"solo-cam")
	}

	/// The peer's SETUP decides whether an advertisement may go out unasked, so nothing
	/// can be sent before it arrives.
	#[moq_clock::test]
	async fn no_announce_before_the_peer_setup() {
		assert_eq!(
			announce_occurrences(None).await,
			0,
			"advertised before knowing what the peer wants"
		);
	}

	/// A peer that requires solicitation hears nothing until it asks, which is the
	/// behavior the IETF draft describes for a relay.
	#[moq_clock::test]
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
	#[moq_clock::test]
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
		handle: crate::auth::Link,
		log: crate::lite::test_transport::Log,
		_origin: crate::origin::Producer,
		_cam: crate::AnnounceProducer,
		_gate: kio::Producer<bool>,
		_goaway: crate::goaway::Handle,
		_driver: moq_clock::sim::JoinHandle<Result<(), Error>>,
	}

	fn auth_session(auth: bool) -> AuthSession {
		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let cam = origin.announce("solo-cam", crate::origin::Route::default()).unwrap();

		let gate = kio::Producer::new(true);
		let session = crate::lite::test_transport::SinkSession::gated_bi(gate.consume());
		let log = session.log.clone();

		let handle = crate::auth::Link::ietf(true);
		let (driver, goaway, _) = start(Config {
			runtime: crate::time::sim(),
			stats: Default::default(),
			session: crate::driver::Transport::new(session.clone()),
			limits: Default::default(),
			client: true,
			publish: Some(origin.consume()),
			subscribe: None,
			peer_hop: None,
			cost: None,
			version: Version::Draft18,
			path: None,
			authority: None,
			token: None,
			peer_setup_stream: None,
			peer_declared: Some(peer::Peer {
				auth,
				..Default::default()
			}),
			auth: handle.clone(),
			early_unis: Vec::new(),
			implementation: None,
			peer_implementation: Default::default(),
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
			_driver: moq_clock::sim::spawn(driver),
		}
	}

	/// A peer that never offered MoQ Auth sees no Auth request and the session keeps
	/// working. A token resolves promptly rather than hanging, kept with an unknown grant
	/// for the requests that name it.
	#[moq_clock::test]
	async fn a_peer_without_auth_sees_no_auth_request() {
		let session = auth_session(false);
		let (handle, log) = (&session.handle, &session.log);

		let (auth, answer) = moq_clock::timeout(std::time::Duration::from_secs(1), handle.handle().add("token"))
			.await
			.expect("resolved promptly")
			.expect("kept for requests");
		assert_eq!(answer, crate::auth::Answer::Requests);
		assert_eq!(auth.grant().peek(), None, "no grant without the extension");
		assert_eq!(handle.handle().grant().peek(), None, "no grant without the extension");

		for _ in 0..ANNOUNCE_TURNS {
			if occurrences(log, b"solo-cam") > 0 {
				break;
			}
			moq_clock::sleep(std::time::Duration::from_millis(1)).await;
		}
		assert_eq!(occurrences(log, b"solo-cam"), 1, "the session stopped advertising");
		assert_eq!(
			occurrences(log, &auth_type()),
			0,
			"sent AUTH to a peer that never offered it"
		);
	}

	/// A peer that offered it gets the connection's own credential right away.
	#[moq_clock::test]
	async fn a_negotiating_peer_gets_the_setup_token() {
		let session = auth_session(true);
		let log = &session.log;
		for _ in 0..ANNOUNCE_TURNS {
			if occurrences(log, &auth_type()) > 0 {
				break;
			}
			moq_clock::sleep(std::time::Duration::from_millis(1)).await;
		}
		assert_eq!(occurrences(log, &auth_type()), 1, "one AUTH for the setup token");
	}

	/// What the draft-18 SETUP that `start` writes says about MoQ Auth and MoQ Solicit.
	async fn our_setup(client: bool, auth: crate::auth::Link, extensions: crate::setup::Extensions) -> (bool, bool) {
		const VERSION: Version = Version::Draft18;

		let session = crate::lite::test_transport::ScriptedSession::new(Vec::new());
		let log = session.log.clone();
		let (driver, _goaway, _) = start(Config {
			runtime: crate::time::sim(),
			stats: Default::default(),
			session: crate::driver::Transport::new(session.clone()),
			limits: Default::default(),
			client,
			publish: None,
			subscribe: None,
			peer_hop: None,
			cost: None,
			version: VERSION,
			path: None,
			authority: None,
			token: None,
			peer_setup_stream: None,
			peer_declared: Some(peer::Peer::default()),
			auth,
			early_unis: Vec::new(),
			implementation: None,
			peer_implementation: Default::default(),
			extensions,
		})
		.expect("start the session");

		// The driver parks once SETUP is out.
		moq_clock::timeout(std::time::Duration::from_secs(10), driver)
			.await
			.expect_err("the session ended instead of parking after SETUP");

		let writes = log.writes.lock().unwrap().clone();
		let (setup, _) = setup::Setup::decode_slice(&writes, crate::Version::Ietf(VERSION)).expect("our SETUP");
		let (params, _) = ietf::Parameters::decode_slice(&setup.parameters, VERSION).expect("its options");
		(
			auth::from_setup(&params) == Some(true),
			solicit::from_setup(&params, VERSION).unwrap() == Some(true),
		)
	}

	/// Each extension reaches our SETUP only while this side offers it.
	#[moq_clock::test]
	async fn a_declined_extension_stays_out_of_setup() {
		let all = crate::setup::Extensions::default();
		let mut no_auth = all;
		no_auth.auth = false;
		let mut no_solicit = all;
		no_solicit.solicit = false;

		// A client's handle supports AUTH exactly when it offers it.
		assert_eq!(our_setup(true, crate::auth::Link::new(true), all).await, (true, true));
		assert_eq!(
			our_setup(true, crate::auth::Link::new(false), no_auth).await,
			(false, true)
		);
		assert_eq!(
			our_setup(true, crate::auth::Link::new(true), no_solicit).await,
			(true, false)
		);
	}

	/// A server's handle supports AUTH only when the client offered it too, so a server
	/// answering a client that did not offer it leaves the option out, whatever it offers.
	#[moq_clock::test]
	async fn a_server_offers_auth_only_to_a_client_that_did() {
		let all = crate::setup::Extensions::default();
		assert_eq!(our_setup(false, crate::auth::Link::new(true), all).await, (true, true));
		assert_eq!(
			our_setup(false, crate::auth::Link::new(false), all).await,
			(false, true)
		);
	}

	/// The bytes of a peer's Auth request presenting its connection credential.
	async fn auth_request(version: Version) -> Vec<u8> {
		let log = crate::lite::test_transport::Log::default();
		let mut writer = crate::coding::Writer::new(
			crate::transport::boxed::SendStream::new(crate::lite::test_transport::SinkSend::new(log.clone())),
			version,
		);
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
			runtime: crate::time::sim(),
			stats: Default::default(),
			session: crate::driver::Transport::new(session.clone()),
			limits: Default::default(),
			client: false,
			publish: None,
			subscribe: None,
			peer_hop: None,
			cost: None,
			version: VERSION,
			path: None,
			authority: None,
			token: None,
			peer_setup_stream: None,
			peer_declared: Some(peer::Peer {
				auth: true,
				..Default::default()
			}),
			// As `Server` builds it: offered by the client, so supported iff we offer it.
			auth: crate::auth::Link::new(offered),
			early_unis: Vec::new(),
			implementation: None,
			peer_implementation: Default::default(),
			extensions: crate::setup::Extensions::default(),
		})
		.expect("start the session");

		let res = moq_clock::timeout(std::time::Duration::from_secs(10), driver)
			.await
			.ok();
		(res, log.closes())
	}

	/// The Auth draft negotiates the extension only when both sides offer it, so an Auth
	/// request reaching a side that declined it is a protocol violation, not a token to grant.
	#[moq_clock::test]
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
		const VERSION: Version = Version::Draft18;

		// A driver that survives parks forever, so bound it: paused time makes the
		// deadline fire the moment nothing else can run.

		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let log = session.log.clone();

		let (driver, _goaway, _) = start(Config {
			runtime: crate::time::sim(),
			stats: Default::default(),
			session: crate::driver::Transport::new(session.clone()),
			limits: Default::default(),
			client: true,
			publish: None,
			subscribe: Some(origin),
			peer_hop: None,
			cost: None,
			version: VERSION,
			path: None,
			authority: None,
			token: None,
			peer_setup_stream: None,
			// Pre-settled, so nothing waits on a SETUP the dead stream will never
			// carry and the dispatch loop actually runs.
			peer_declared: Some(peer::Peer::default()),
			auth: crate::auth::Link::new(false),
			early_unis: Vec::new(),
			implementation: None,
			peer_implementation: Default::default(),
			extensions: Default::default(),
		})
		.expect("start the session");

		moq_clock::timeout(std::time::Duration::from_secs(10), driver)
			.await
			.expect_err("the session ended over one dead stream");

		assert_eq!(log.closes(), vec![], "nothing may close the transport");
	}

	/// A client advertises its AUTHORITY and connection credential in the SETUP it writes on
	/// its uni stream.
	#[moq_clock::test]
	async fn setup_carries_the_authority_and_token() {
		const AUTHORITY: &[u8] = b"relay.example.com:4443";
		const TOKEN: &[u8] = b"connection-credential";

		// The driver parks forever once SETUP is out, so bound it: paused time makes the
		// deadline fire the moment nothing else can run.

		let version = Version::Draft18;
		let session = crate::lite::test_transport::ScriptedSession::new(Vec::new());
		let log = session.log.clone();

		let (driver, _goaway, _) = start(Config {
			runtime: crate::time::sim(),
			stats: Default::default(),
			session: crate::driver::Transport::new(session.clone()),
			limits: Default::default(),
			client: true,
			publish: None,
			subscribe: None,
			peer_hop: None,
			cost: None,
			version,
			path: None,
			authority: Some(String::from_utf8(AUTHORITY.to_vec()).unwrap()),
			token: Some(crate::setup::Token {
				kind: crate::setup::Token::OUT_OF_BAND,
				value: TOKEN.to_vec(),
			}),
			peer_setup_stream: None,
			peer_declared: Some(peer::Peer::default()),
			auth: crate::auth::Link::new(false),
			early_unis: Vec::new(),
			implementation: None,
			peer_implementation: Default::default(),
			extensions: Default::default(),
		})
		.expect("start the session");

		moq_clock::timeout(std::time::Duration::from_secs(10), driver)
			.await
			.expect_err("the session ended instead of parking after SETUP");

		assert_eq!(
			occurrences(&log, AUTHORITY),
			1,
			"{version:?}: SETUP must carry the authority once"
		);
		assert_eq!(
			occurrences(&log, TOKEN),
			1,
			"{version:?}: SETUP must carry the token once"
		);
	}

	#[moq_clock::test]
	async fn a_uni_stream_dead_before_its_type_does_not_end_the_session() {
		a_dead_incoming_stream_is_not_fatal(crate::lite::test_transport::DeadStreamSession::unis(1)).await;
	}

	#[moq_clock::test]
	async fn a_bidi_stream_dead_before_its_header_does_not_end_the_session() {
		a_dead_incoming_stream_is_not_fatal(crate::lite::test_transport::DeadStreamSession::bis(1)).await;
	}

	/// moxygen resets a subgroup stream it opened but never wrote with its own CANCELLED
	/// (0x1). Over raw QUIC our transport reads that code through the WebTransport space
	/// and cannot map it, which used to end the session.
	#[moq_clock::test]
	async fn a_uni_stream_reset_with_an_unmapped_code_does_not_end_the_session() {
		a_dead_incoming_stream_is_not_fatal(crate::lite::test_transport::DeadStreamSession::unis(1).unmapped()).await;
	}

	#[moq_clock::test]
	async fn a_bidi_stream_reset_with_an_unmapped_code_does_not_end_the_session() {
		a_dead_incoming_stream_is_not_fatal(crate::lite::test_transport::DeadStreamSession::bis(1).unmapped()).await;
	}

	/// The bytes a publisher writes at the head of a group's unidirectional stream. Built
	/// with the crate's own encoder so the framing can't drift from the decoder the
	/// dispatch loop runs.
	async fn subgroup_header(version: Version, track_alias: u64, sub_group_id: u64) -> Vec<u8> {
		let log = crate::lite::test_transport::Log::default();
		let mut writer = crate::coding::Writer::new(
			crate::transport::boxed::SendStream::new(crate::lite::test_transport::SinkSend::new(log.clone())),
			version,
		);

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
		accepted: Option<PeerSetup>,
		retired_alias: Option<u64>,
	) -> (crate::lite::test_transport::Log, Option<Result<(), Error>>) {
		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let log = session.log.clone();

		let (tasks, _task_set) = TaskSet::new();
		let peer_setup = peer::PeerSetup::default();
		let subscriber = Subscriber::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			origin,
			Control::new(true),
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
		let mut unis = Unis::new(
			crate::driver::Transport::new(session.clone()),
			subscriber,
			UniSetup {
				peer: Some(peer_setup),
				read,
				early,
				client: true,
				version,
				goaway,
			},
		);

		for _ in 0..100 {
			if let std::task::Poll::Ready(result) = unis.poll(&kio::Waiter::noop()) {
				return (log, Some(result));
			}
			if !log.stops().is_empty() {
				break;
			}
			moq_clock::sleep(std::time::Duration::from_millis(1)).await;
		}

		(log, None)
	}

	/// A late group must reach the dispatch loop and stop with CANCELLED.
	#[moq_clock::test]
	async fn a_group_for_a_retired_alias_is_stopped_with_cancelled() {
		let (log, result) =
			dispatch_uni(Version::Draft18, subgroup_header(Version::Draft18, 7, 0).await, Some(7)).await;

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
	#[moq_clock::test]
	async fn a_silent_stream_does_not_hold_up_the_next() {
		let header = subgroup_header(Version::Draft18, 7, 0).await;
		let session =
			crate::lite::test_transport::ScriptedSession::new(Vec::new()).with_incoming_unis(vec![Vec::new(), header]);
		let (log, result) = drive_unis(Version::Draft18, session, None, Some(7)).await;

		assert_eq!(
			log.stops(),
			vec![crate::ietf::error::CANCELLED],
			"the stream behind the silent one was never read",
		);
		assert!(result.is_none(), "the silent stream ended the session: {result:?}");
	}

	/// A stalled bidi header must not block a SUBSCRIBE on the next stream.
	#[moq_clock::test]
	async fn a_silent_bidi_does_not_hold_up_the_next() {
		const VERSION: Version = Version::Draft18;
		let mut subscribe = Vec::new();
		let mut encoder = crate::coding::Encoder::new(&mut subscribe, VERSION.into());
		encoder.varint(ietf::Subscribe::ID).unwrap();
		ietf::Subscribe {
			max_age: None,
			new_group: None,
			request_id: RequestId(1),
			track_namespace: crate::Path::from_static("room"),
			track_name: "video".into(),
			subscriber_priority: 128,
			group_order: ietf::GroupOrder::Descending,
			filter: ietf::Filter::NextObject,
			fill: None,
			properties_wanted: true,
			forward: true,
			rendezvous: false,
			range_filters: false,
			authorization_token: None,
			epoch: None,
		}
		.encode(&mut encoder, VERSION)
		.unwrap();

		// Stall before the type, then after it while the body has not arrived.
		for stalled in [Vec::new(), vec![ietf::Subscribe::ID as u8]] {
			let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
			let broadcast = origin.publish("room", crate::origin::Route::default()).unwrap();
			let mut track = broadcast.create_track("video", None).unwrap();
			let session = crate::lite::test_transport::ScriptedSession::new(Vec::new())
				.with_incoming_bidis(vec![stalled, subscribe.clone()]);
			let driver = bidi_driver(session, Some(origin.consume()));
			let subscribed = Box::pin(async { while track.subscription_changed().await.unwrap().is_none() {} });
			let result = moq_clock::timeout(
				std::time::Duration::from_secs(10),
				futures::future::select(driver, subscribed),
			)
			.await
			.expect("SUBSCRIBE behind the stalled header was never dispatched");
			assert!(
				matches!(result, futures::future::Either::Right(_)),
				"session ended before SUBSCRIBE"
			);
		}
	}

	/// Child tasks must report malformed and unknown requests to the session driver.
	#[moq_clock::test]
	async fn invalid_bidi_requests_still_end_the_session() {
		for id in [ietf::Subscribe::ID, 0x3f] {
			let mut header = Vec::new();
			let mut encoder = crate::coding::Encoder::new(&mut header, Version::Draft18.into());
			encoder.varint(id).unwrap();
			encoder.slice(&[0, 0]); // Complete, empty body: malformed for SUBSCRIBE.
			let session =
				crate::lite::test_transport::ScriptedSession::new(Vec::new()).with_incoming_bidis(vec![header]);
			let err = moq_clock::timeout(std::time::Duration::from_secs(10), bidi_driver(session, None))
				.await
				.expect("invalid request did not end the session")
				.expect_err("invalid request was accepted");
			if id == ietf::Subscribe::ID {
				assert!(matches!(err, Error::Decode(_)), "{err:?}");
			} else {
				assert!(matches!(err, Error::UnexpectedStream), "{err:?}");
			}
		}
	}

	/// Start a draft-22 session with SETUP already settled, so bidi requests run immediately.
	fn bidi_driver(session: crate::lite::test_transport::ScriptedSession, publish: Option<origin::Consumer>) -> Driver {
		start(Config {
			runtime: crate::time::sim(),
			stats: Default::default(),
			session: crate::driver::Transport::new(session.clone()),
			limits: Default::default(),
			client: true,
			publish,
			subscribe: None,
			peer_hop: None,
			cost: None,
			version: Version::Draft18,
			path: None,
			authority: None,
			token: None,
			peer_setup_stream: None,
			peer_declared: Some(peer::Peer::default()),
			auth: crate::auth::Link::new(false),
			early_unis: Vec::new(),
			implementation: None,
			peer_implementation: Default::default(),
			extensions: Default::default(),
		})
		.unwrap()
		.0
	}

	/// A non-zero subgroup is refused on its own stream, never by closing the session.
	#[moq_clock::test]
	async fn a_non_zero_subgroup_is_stopped_without_closing_the_session() {
		let (log, result) = dispatch_uni(Version::Draft18, subgroup_header(Version::Draft18, 7, 1).await, None).await;

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
	#[moq_clock::test]
	async fn a_padding_stream_is_discarded() {
		for version in [Version::Draft18, Version::Draft22] {
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
	#[moq_clock::test]
	async fn uni_streams_before_setup_are_held_until_it_lands() {
		const VERSION: Version = Version::Draft18;

		let mut setup = Vec::new();
		setup::Setup {
			parameters: ietf::Parameters::default().encode_bytes(VERSION).unwrap(),
		}
		.encode(
			&mut crate::coding::Encoder::new(&mut setup, VERSION.into()),
			crate::Version::Ietf(VERSION),
		)
		.unwrap();

		let session = crate::lite::test_transport::ScriptedSession::new(Vec::new()).with_incoming_unis(vec![
			uni_stream(VERSION, PADDING),
			subgroup_header(VERSION, 7, 0).await,
			setup,
		]);
		let accepted = accept_setup(&mut crate::driver::Transport::new(session.clone()), VERSION)
			.await
			.expect("accept the SETUP");
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
	#[moq_clock::test]
	async fn an_unknown_uni_type_closes_the_session() {
		for (version, kind) in [
			(Version::Draft18, 0),
			// SUBGROUP_HEADER types with the reserved SUBGROUP_ID_MODE (0b11).
			(Version::Draft18, 0x56),
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
	#[moq_clock::test]
	async fn an_unknown_bidi_type_closes_the_session() {
		for version in [Version::Draft18, Version::Draft22] {
			for kind in [0u64, 1 << 53] {
				let mut payload = Vec::new();
				let mut w = crate::coding::Encoder::new(&mut payload, version.into());
				w.varint(kind).unwrap();
				w.u16(0);
				let session =
					crate::lite::test_transport::ScriptedSession::new(Vec::new()).with_incoming_bidis(vec![payload]);
				let log = session.log.clone();
				let (driver, _goaway, _) = start(Config {
					runtime: crate::time::sim(),
					stats: Default::default(),
					session: crate::driver::Transport::new(session.clone()),
					limits: Default::default(),
					client: false,
					publish: None,
					subscribe: None,
					peer_hop: None,
					cost: None,
					version,
					path: None,
					authority: None,
					token: None,
					peer_setup_stream: None,
					peer_declared: Some(peer::Peer::default()),
					auth: crate::auth::Link::new(false),
					early_unis: Vec::new(),
					implementation: None,
					peer_implementation: Default::default(),
					extensions: Default::default(),
				})
				.unwrap();

				let err = moq_clock::timeout(std::time::Duration::from_secs(10), driver)
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

	#[moq_clock::test]
	async fn an_invalid_group_order_closes_the_session() {
		for version in [Version::Draft18, Version::Draft22] {
			for (value, nested) in [
				(0u8, false),
				(3, false),
				(255, false),
				(0, true),
				(3, true),
				(255, true),
			] {
				// FILL_PARAMETERS starts in draft-22.
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
					runtime: crate::time::sim(),
					stats: Default::default(),
					session: crate::driver::Transport::new(session.clone()),
					client: false,
					publish: None,
					subscribe: None,
					peer_hop: None,
					cost: None,
					version,
					path: None,
					authority: None,
					token: None,
					peer_setup_stream: None,
					peer_declared: Some(peer::Peer::default()),
					auth: crate::auth::Link::new(false),
					early_unis: Vec::new(),
					implementation: None,
					peer_implementation: Default::default(),
					extensions: Default::default(),
					limits: Default::default(),
				})
				.unwrap();
				let err = moq_clock::timeout(std::time::Duration::from_secs(10), driver)
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

	/// A SUBSCRIBE for `room/video` whose parameters are one AUTHORIZATION TOKEN holding
	/// `token`, framed as a control message.
	fn subscribe_with_token(version: Version, token: &[u8]) -> Vec<u8> {
		let mut body = Vec::new();
		let mut w = crate::coding::Encoder::new(&mut body, version.into());
		// The request ID, namespace and name; every parameter but the token defaults.
		w.slice(&[0, 1, 4, b'r', b'o', b'o', b'm', 5, b'v', b'i', b'd', b'e', b'o']);
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
	/// endpoint cannot use closes the session with the draft's code.
	#[moq_clock::test]
	async fn a_request_token_that_cannot_be_used_closes_the_session() {
		for version in [Version::Draft22] {
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
				let session =
					crate::lite::test_transport::ScriptedSession::new(Vec::new()).with_incoming_bidis(vec![payload]);
				let log = session.log.clone();
				let session = crate::driver::Transport::new(session.clone());
				let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
				let (driver, _goaway, _) = start(Config {
					runtime: crate::time::sim(),
					stats: Default::default(),
					session,
					limits: Default::default(),
					client: false,
					publish: Some(origin.consume()),
					subscribe: None,
					peer_hop: None,
					cost: None,
					version,
					path: None,
					authority: None,
					token: None,
					peer_setup_stream: None,
					peer_declared: Some(peer::Peer::default()),
					auth: crate::auth::Link::new(false),
					early_unis: Vec::new(),
					implementation: None,
					peer_implementation: Default::default(),
					extensions: Default::default(),
				})
				.unwrap();

				let err = moq_clock::timeout(std::time::Duration::from_secs(10), driver)
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

	/// A framed message: its type, a `u16` length, then `body`.
	fn frame(version: Version, id: u64, body: &[u8]) -> Vec<u8> {
		let mut buf = Vec::new();
		let mut w = crate::coding::Encoder::new(&mut buf, version.into());
		w.varint(id).unwrap();
		w.u16(body.len() as u16);
		w.slice(body);
		buf
	}

	/// The peer's SETUP stream: a SETUP with `params`, then `after`.
	fn setup_stream(version: Version, params: ietf::Parameters, after: &[u8]) -> Vec<u8> {
		let mut buf = Vec::new();
		setup::Setup {
			parameters: params.encode_bytes(version).unwrap(),
		}
		.encode(
			&mut crate::coding::Encoder::new(&mut buf, version.into()),
			crate::Version::Ietf(version),
		)
		.unwrap();
		buf.extend_from_slice(after);
		buf
	}

	/// A GOAWAY with an empty New Session URI, and on draft-18 the control stream's Request ID.
	fn goaway(version: Version, request_id: Option<u64>) -> Vec<u8> {
		let msg = ietf::GoAway {
			new_session_uri: "".into(),
			timeout: 0,
			request_id: request_id.map(RequestId),
		};
		let mut body = Vec::new();
		msg.encode_msg(&mut crate::coding::Encoder::new(&mut body, version.into()), version)
			.unwrap();
		frame(version, ietf::GoAway::ID, &body)
	}

	/// The session code a client closes with after reading `stream` as the server's SETUP
	/// stream, or `None` while the session stays up.
	async fn client_reads_setup_stream(version: Version, stream: Vec<u8>, fin: bool) -> Option<SessionError> {
		let session = crate::lite::test_transport::ScriptedSession::new(Vec::new()).with_incoming_unis(vec![stream]);
		if fin {
			session.close(crate::lite::test_transport::Close::Fin);
		}
		let (_, result) = drive_unis(version, session, None, None).await;
		result.map(|result| SessionError::from(&result.expect_err("the loop ended without an error")))
	}

	/// What a server may not send in its SETUP closes the session with the draft's code.
	#[moq_clock::test]
	async fn a_server_setup_with_path_or_authority_closes_the_session() {
		for version in [Version::Draft18, Version::Draft22] {
			for (param, expected) in [
				(ietf::ParameterBytes::Path, SessionError::InvalidPath),
				(ietf::ParameterBytes::Authority, SessionError::InvalidAuthority),
			] {
				let mut params = ietf::Parameters::default();
				params.set_bytes(param, b"a".to_vec());
				let closed = client_reads_setup_stream(version, setup_stream(version, params, &[]), false).await;
				assert_eq!(closed, Some(expected), "{version:?}");
			}
		}
	}

	/// The server's AUTHORIZATION TOKEN is decoded even though a client has no use for it: a
	/// malformed one closes the session, and a registration overflowing our cache of 0 is
	/// read as the value it carries (draft-22 section 9.1.4).
	#[moq_clock::test]
	async fn a_server_setup_token_decodes_by_the_setup_rules() {
		for version in [Version::Draft18, Version::Draft22] {
			for (token, expected) in [
				// An Alias Type of 4 does not exist.
				(&[0x04][..], Some(SessionError::KeyValueFormatting)),
				// REGISTER alias 7, type 0, empty value.
				(&[0x01, 0x07, 0x00][..], None),
			] {
				let mut params = ietf::Parameters::default();
				params.set_bytes(ietf::ParameterBytes::AuthorizationToken, token.to_vec());
				let closed = client_reads_setup_stream(version, setup_stream(version, params, &[]), false).await;
				assert_eq!(closed, expected, "{version:?} {token:?}");
			}
		}
	}

	/// The SETUP stream carries one GOAWAY and nothing else: anything more, or a GOAWAY that
	/// is malformed, closes the session as a PROTOCOL_VIOLATION. One GOAWAY leaves it up.
	#[moq_clock::test]
	async fn the_setup_stream_carries_at_most_one_goaway() {
		for version in [Version::Draft18, Version::Draft22] {
			let request_id = (version == Version::Draft18).then_some(0);
			let one = goaway(version, request_id);
			let two = [one.clone(), one.clone()].concat();
			let unknown = frame(version, 0x3f, &[]);
			let after_goaway = [one.clone(), unknown.clone()].concat();
			let mut oversized = Vec::new();
			let mut w = crate::coding::Encoder::new(&mut oversized, version.into());
			w.string(&"x".repeat(8193)).unwrap();
			w.varint(0u64).unwrap();
			let oversized = frame(version, ietf::GoAway::ID, &oversized);
			// The length covers the URI but not the timeout after it.
			let short = frame(version, ietf::GoAway::ID, &[0]);
			let mut truncated = one.clone();
			truncated.pop();

			for (after, expected, why) in [
				(&one, None, "one GOAWAY"),
				(&two, Some(SessionError::ProtocolViolation), "two GOAWAYs"),
				(&unknown, Some(SessionError::ProtocolViolation), "an unknown message"),
				(
					&after_goaway,
					Some(SessionError::ProtocolViolation),
					"an unknown message after GOAWAY",
				),
				(&oversized, Some(SessionError::ProtocolViolation), "an 8193 byte URI"),
				(
					&short,
					Some(SessionError::ProtocolViolation),
					"a body short of its fields",
				),
				(
					&truncated,
					Some(SessionError::ProtocolViolation),
					"a body cut short by FIN",
				),
			] {
				let stream = setup_stream(version, ietf::Parameters::default(), after);
				let closed = client_reads_setup_stream(version, stream, why == "a body cut short by FIN").await;
				assert_eq!(closed, expected, "{version:?}: {why}");
			}
		}
	}

	#[moq_clock::test]
	async fn a_control_stream_fin_or_reset_is_a_protocol_violation() {
		use crate::lite::test_transport::{Close, DeadStreamSession, ScriptedSession};
		for version in [Version::Draft18, Version::Draft22] {
			let session = ScriptedSession::new(Vec::new()).with_incoming_unis(vec![setup_stream(
				version,
				ietf::Parameters::default(),
				&[],
			)]);
			session.close(Close::Fin);
			let (_, result) = drive_unis(version, session, None, None).await;
			assert!(
				matches!(result, Some(Err(ref err)) if SessionError::from(err) == SessionError::ProtocolViolation),
				"{version:?}: {result:?}"
			);

			let session = DeadStreamSession::unis(1);
			let recv = session.accept_uni().await.unwrap();
			let (_handle, protocol) = crate::goaway::Handle::new(false);
			let err = run_goaway(
				Reader::new(crate::transport::boxed::RecvStream::new(recv), version),
				version,
				true,
				protocol,
			)
			.await
			.unwrap_err();
			assert_eq!(SessionError::from(&err), SessionError::ProtocolViolation, "{version:?}");
		}
	}

	/// A second GOAWAY arriving with the first closes the session before the first surfaces,
	/// so an application leaving over it cannot close first, without our code.
	#[moq_clock::test]
	async fn a_goaway_with_more_behind_it_never_surfaces() {
		for version in [Version::Draft18, Version::Draft22] {
			let request_id = (version == Version::Draft18).then_some(0);
			let two = [goaway(version, request_id), goaway(version, request_id)].concat();
			let session = crate::lite::test_transport::ScriptedSession::new(Vec::new()).with_incoming_unis(vec![two]);
			let stream = kio::wait(|waiter| session.poll_accept_uni(waiter)).await.unwrap();
			let (handle, protocol) = crate::goaway::Handle::new(false);

			let err = read_goaways(
				Reader::new(crate::transport::boxed::RecvStream::new(stream), version),
				version,
				true,
				protocol,
			)
			.await
			.expect_err("a second GOAWAY is fatal");
			assert_eq!(SessionError::from(&err), SessionError::ProtocolViolation, "{version:?}");
			assert!(
				!matches!(
					handle.consumer().poll(&kio::Waiter::noop()),
					std::task::Poll::Ready(Ok(_))
				),
				"{version:?}: the first GOAWAY surfaced"
			);
		}
	}

	/// A fatal error closes the session before its streams drop. A reset of our SETUP stream
	/// reaching a strict peer first has it close over that, and it never sees our code.
	#[moq_clock::test]
	async fn the_session_closes_before_its_streams_reset() {
		const VERSION: Version = Version::Draft22;
		let mut params = ietf::Parameters::default();
		params.set_bytes(ietf::ParameterBytes::Path, b"a".to_vec());
		let session = crate::lite::test_transport::ScriptedSession::new(Vec::new())
			.with_incoming_unis(vec![setup_stream(VERSION, params, &[])]);
		let log = session.log.clone();
		let (driver, _goaway, _) = start(Config {
			runtime: crate::time::sim(),
			stats: Default::default(),
			session: crate::driver::Transport::new(session.clone()),
			limits: Default::default(),
			client: true,
			publish: None,
			subscribe: None,
			peer_hop: None,
			cost: None,
			version: VERSION,
			path: None,
			authority: None,
			token: None,
			peer_setup_stream: None,
			peer_declared: None,
			auth: crate::auth::Link::new(false),
			early_unis: Vec::new(),
			implementation: None,
			peer_implementation: Default::default(),
			extensions: Default::default(),
		})
		.unwrap();
		let _ = moq_clock::timeout(std::time::Duration::from_secs(10), driver).await;

		let closes = log.closes();
		assert_eq!(closes.len(), 1, "{closes:?}");
		assert_eq!(SessionError::from_code(closes[0].0), SessionError::InvalidPath);
		let before = log.before_close().unwrap();
		assert!(
			!before
				.trail
				.iter()
				.any(|(_, sent)| matches!(sent, crate::lite::test_transport::Sent::Reset(_))),
			"reset before the close: {before:?}"
		);
		assert!(before.stops.is_empty(), "STOP_SENDING before the close: {before:?}");
	}

	/// A draft-18 GOAWAY names one of the receiver's own Request IDs, so one with the sender's
	/// parity closes the session with INVALID_REQUEST_ID rather than redirecting it, and one
	/// without a Request ID closes it as a PROTOCOL_VIOLATION.
	#[moq_clock::test]
	async fn a_draft18_goaway_request_id_has_the_receivers_parity() {
		const VERSION: Version = Version::Draft18;
		// The loop runs as a client, whose Request IDs are even.
		for (request_id, expected) in [
			(Some(0), None),
			(Some(4), None),
			(Some(1), Some(SessionError::InvalidRequestId)),
			(Some(5), Some(SessionError::InvalidRequestId)),
			// Required on the control stream.
			(None, Some(SessionError::ProtocolViolation)),
		] {
			let stream = setup_stream(VERSION, ietf::Parameters::default(), &goaway(VERSION, request_id));
			let closed = client_reads_setup_stream(VERSION, stream, false).await;
			assert_eq!(closed, expected, "Request ID {request_id:?}");
		}
	}

	/// Draft-18 GOAWAY names the first peer ID past every one used, preserving the peer's
	/// parity, and still admits one a reordered stream skipped below it.
	#[moq_clock::test]
	async fn our_draft18_goaway_names_the_admitted_request_cutoff() {
		const VERSION: Version = Version::Draft18;
		for client in [false, true] {
			let session = crate::lite::test_transport::ScriptedSession::new(Vec::new());
			let log = session.log.clone();
			let control = Control::new(client);
			let first = u64::from(client);
			control.accept(RequestId(first)).unwrap();
			control.accept(RequestId(first + 4)).unwrap();
			let (handle, goaway_protocol) = crate::goaway::Handle::new(!client);
			let mut setup = std::pin::pin!(run_setup(
				crate::driver::Transport::new(session.clone()),
				VERSION,
				None,
				None,
				None,
				None,
				Hop::new(1).unwrap(),
				None,
				Default::default(),
				goaway_protocol,
				control.clone(),
				Default::default(),
			));
			handle.producer().send(crate::goaway::Goaway::new()).unwrap();
			assert!(futures::poll!(setup.as_mut()).is_pending());
			let sent = goaway(VERSION, Some(first + 6));
			assert_eq!(occurrences(&log, &sent), 1, "client={client}");
			control.accept(RequestId(first + 2)).unwrap();
			assert!(matches!(control.accept(RequestId(first + 6)), Err(Error::GoingAway)));
		}
	}

	/// Drive a session that reads `payloads` as bidi streams the peer opened, and return the
	/// code it closed with, or `None` while it stays up.
	///
	/// Read off the transport, since a request task closes the session itself and the
	/// scripted transport does not end the driver for it.
	async fn serve_bidis(version: Version, client: bool, payloads: Vec<Vec<u8>>) -> Option<SessionError> {
		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let session = crate::lite::test_transport::ScriptedSession::new(Vec::new()).with_incoming_bidis(payloads);
		// The peer FINs each stream after its script, so a message it cuts short is malformed.
		session.close(crate::lite::test_transport::Close::Fin);
		let log = session.log.clone();
		let (driver, _goaway, _) = start(Config {
			runtime: crate::time::sim(),
			stats: Default::default(),
			session: crate::driver::Transport::new(session.clone()),
			limits: Default::default(),
			client,
			publish: Some(origin.consume()),
			subscribe: None,
			peer_hop: None,
			cost: None,
			version,
			path: None,
			authority: None,
			token: None,
			peer_setup_stream: None,
			peer_declared: Some(peer::Peer::default()),
			auth: crate::auth::Link::new(false),
			early_unis: Vec::new(),
			implementation: None,
			peer_implementation: Default::default(),
			extensions: Default::default(),
		})
		.unwrap();
		let _ = moq_clock::timeout(std::time::Duration::from_secs(10), driver).await;
		let closes = log.closes();
		// A request stream closes at the source and its task may close again; the transport
		// keeps the first, so only a disagreeing code would reach the peer.
		assert!(
			closes.iter().all(|(code, _)| *code == closes[0].0),
			"closed with different codes: {closes:?}"
		);
		closes.first().map(|(code, _)| SessionError::from_code(*code))
	}

	/// A request past our GOAWAY cutoff is refused in the answer its kind reads: an AUTH
	/// with AUTH_ERROR carrying GOING_AWAY, since its presenter takes anything else as a
	/// violation, and the rest with REQUEST_ERROR.
	#[moq_clock::test]
	async fn a_request_past_our_goaway_is_refused_in_its_own_answer() {
		const VERSION: Version = Version::Draft18;
		let reason = b"going away";
		let body = [&[0x6, reason.len() as u8][..], reason].concat();
		for (id, answer) in [
			(auth::Auth::ID, frame(VERSION, auth::AuthError::ID, &body)),
			(
				ietf::Subscribe::ID,
				frame(VERSION, ietf::RequestError::ID, &[&[0x6, 0][..], &body[1..]].concat()),
			),
		] {
			let session = crate::lite::test_transport::ScriptedSession::new(Vec::new());
			let log = session.log.clone();
			let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), VERSION)
				.await
				.unwrap();
			refuse_going_away(stream, id, VERSION).await.unwrap();
			assert_eq!(*log.writes.lock().unwrap(), answer, "{id:#x}");
		}
	}

	/// A malformed request closes the session with the draft's code: a Request ID of the
	/// wrong parity, a namespace or name outside its bounds, or a body that does not fill
	/// its length.
	#[moq_clock::test]
	async fn a_malformed_request_closes_the_session() {
		for version in [Version::Draft18, Version::Draft22] {
			let subscribe = |body: &[u8]| frame(version, ietf::Subscribe::ID, body);
			// Request ID 1, the namespace "n", the name "x", and no parameters.
			let valid = [1, 1, 1, b'n', 1, b'x', 0];

			let mut oversized_name = vec![1, 1];
			let mut w = crate::coding::Encoder::new(&mut oversized_name, version.into());
			w.bytes(&[b'n'; 4096]).unwrap();
			w.bytes(b"x").unwrap();
			w.varint(0u64).unwrap();

			let mut oversized_prefix = vec![1, 1];
			let mut w = crate::coding::Encoder::new(&mut oversized_prefix, version.into());
			w.bytes(&[b'n'; 4097]).unwrap();
			w.varint(0u64).unwrap();

			let mut truncated = subscribe(&valid);
			truncated.pop();

			for (payload, expected, why) in [
				(
					frame(version, ietf::SubscribeNamespace::ID, &[0, 0, 0]),
					SessionError::InvalidRequestId,
					"a Request ID with our parity",
				),
				(
					subscribe(&[1, 1, 0, 1, b'x', 0]),
					SessionError::ProtocolViolation,
					"an empty namespace field",
				),
				(
					subscribe(&oversized_name),
					SessionError::ProtocolViolation,
					"a 4097 byte full track name",
				),
				(
					frame(version, ietf::SubscribeTracks::ID, &oversized_prefix),
					SessionError::ProtocolViolation,
					"a 4097 byte SUBSCRIBE_TRACKS prefix",
				),
				(
					subscribe(&[&valid[..], &[0]].concat()),
					SessionError::ProtocolViolation,
					"a body longer than its fields",
				),
				(truncated, SessionError::ProtocolViolation, "a body cut short by FIN"),
			] {
				let closed = serve_bidis(version, true, vec![payload]).await;
				assert_eq!(closed, Some(expected), "{version:?}: {why}");
			}
		}
	}

	/// A SUBSCRIBE_NAMESPACE for every namespace, then `after` on the same stream.
	fn subscribe_namespace_then(version: Version, request_id: u64, after: &[u8]) -> Vec<u8> {
		let mut body = Vec::new();
		ietf::SubscribeNamespace {
			request_id: RequestId(request_id),
			namespace: crate::Path::from_static(""),
			hidden: false,
		}
		.encode_msg(&mut crate::coding::Encoder::new(&mut body, version.into()), version)
		.unwrap();
		[frame(version, ietf::SubscribeNamespace::ID, &body), after.to_vec()].concat()
	}

	/// A request stream may carry one GOAWAY, migrating that request alone: it leaves the
	/// session up, even on two streams at once. A second on one stream, or an unknown
	/// message, closes the session as a PROTOCOL_VIOLATION.
	#[moq_clock::test]
	async fn a_request_stream_carries_at_most_one_goaway() {
		for version in [Version::Draft18, Version::Draft22] {
			let one = goaway(version, None);
			let two = [one.clone(), one.clone()].concat();

			let distinct = vec![
				subscribe_namespace_then(version, 1, &one),
				subscribe_namespace_then(version, 3, &one),
			];
			assert!(
				serve_bidis(version, true, distinct).await.is_none(),
				"{version:?}: a GOAWAY on each of two streams"
			);

			let mut malformed = vec![(two, "two GOAWAYs"), (frame(version, 0x7e, &[]), "an unknown message")];
			// Draft-18's Request ID belongs on the control stream only.
			if version == Version::Draft18 {
				malformed.push((goaway(version, Some(0)), "a GOAWAY with a Request ID"));
			}
			for (after, why) in malformed {
				let closed = serve_bidis(version, true, vec![subscribe_namespace_then(version, 1, &after)]).await;
				assert_eq!(closed, Some(SessionError::ProtocolViolation), "{version:?}: {why}");
			}
		}
	}

	/// Only a server may name a new session in a request stream GOAWAY: a client that does
	/// closes the session as a PROTOCOL_VIOLATION.
	#[moq_clock::test]
	async fn a_request_stream_goaway_redirects_only_from_a_server() {
		for version in [Version::Draft18, Version::Draft22] {
			let mut body = Vec::new();
			ietf::GoAway {
				new_session_uri: "https://example.com/moq".into(),
				timeout: 0,
				request_id: None,
			}
			.encode_msg(&mut crate::coding::Encoder::new(&mut body, version.into()), version)
			.unwrap();
			let redirect = frame(version, ietf::GoAway::ID, &body);

			// A server's Request IDs are odd and a client's even.
			for (client, request_id, expected) in [(true, 1, None), (false, 0, Some(SessionError::ProtocolViolation))] {
				let closed = serve_bidis(
					version,
					client,
					vec![subscribe_namespace_then(version, request_id, &redirect)],
				)
				.await;
				assert_eq!(closed, expected, "{version:?}: served as client={client}");
			}
		}
	}
}
