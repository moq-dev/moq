use crate::driver::Transport;
use crate::transport::poll::Session as _;
#[cfg(test)]
use std::sync::atomic::AtomicU64;

use crate::{SessionError, announce, frame, group, origin, track};
use rangemap::{RangeMap, RangeSet};
use std::{
	collections::HashMap,
	ops::{Bound, ControlFlow, Range},
	sync::{
		Arc, Mutex,
		atomic::{AtomicUsize, Ordering},
	},
	task::{Poll, ready},
	time::Duration,
};

use crate::{
	Error, Hop, Hops, StreamError,
	coding::{Decode, DecodeError, Decoder, Encode, Stream, Writer},
	lite,
	priority::{Priority, PriorityHandle, PriorityQueue},
	track::Order,
	transport::poll::{GroupOf, SendGroup as _, SendOptions},
};

use super::Version;

pub(super) struct PublisherConfig {
	/// The runtime that arms the publisher's timers.
	pub runtime: crate::time::Deadlines,
	pub session: Transport,
	/// The origin we read local broadcasts from. Traffic stats are attributed
	/// through this handle: tag it with [`origin::Consumer::with_stats`] first.
	pub origin: origin::Consumer,
	pub version: Version,
	/// The peer's SETUP (lite-05+), shared with the subscriber half that reads
	/// it. Carries the peer's declared origin id for split-horizon serving.
	pub peer_setup: super::PeerSetup,
	/// Receive-side GOAWAY signal: recorded when the peer's Goaway stream arrives.
	pub goaway: crate::goaway::Protocol,
	/// The origin (hop) id assigned to the peer, used whenever the peer doesn't
	/// declare one itself. See `Client::with_peer_hop`.
	pub peer_hop: Option<Hop>,
	/// Our tokens' grants, which bound what we announce and serve, and the app's
	/// claim on answering the peer's tokens.
	pub auth: crate::auth::Link,
	/// What the default acceptor grants the peer's connection credential.
	pub peer_grant: crate::auth::Grant,
	/// The supervisor's estimate, which a PROBE stream follows.
	pub estimate: kio::ProducerWeak<crate::session::Estimate>,
	/// Whether we dialed: only then does a publication outside our grant abort.
	pub client: bool,
	/// Counts the subscriptions the peer holds, for the session's subscriptions peak.
	pub subscriptions: crate::session::Slots,
}

/// Context shared by every control-stream child.
struct Shared {
	client: bool,
	withdrawal: crate::session::Withdrawal,
	session: Transport,
	origin: origin::Consumer,
	self_origin: Hop,
	// The peer's SETUP, read for the origin id it declared. Used to serve the
	// peer from a source whose chain excludes them, keeping the data plane on
	// the same split-horizon rule as the announces we send them.
	peer_setup: super::PeerSetup,
	// The identity assigned to the peer (a fresh per-session id, or one the caller
	// pinned with `with_peer_hop`), standing in wherever the
	// peer declines to declare one. Backs both the announce filter and the serving
	// origin, so a peer that names itself nowhere on the wire is still split-horizoned.
	peer_hop: Option<Hop>,
	// The excluded origin handle, resolved once: the peer sends exactly one
	// SETUP, so its declared id never changes for the session.
	serving: std::sync::OnceLock<origin::Consumer>,
	priority: PriorityQueue,
	version: Version,
	goaway: crate::goaway::Protocol,
	auth: crate::auth::Link,
	peer_grant: crate::auth::Grant,
	// Control streams still serving the peer data, which a draining close waits for.
	owed: AtomicUsize,
	// The subscriptions the peer holds, counted for the stats peak.
	subscriptions: crate::session::Slots,
	/// Holds this session's listings: one per broadcast, the newest replacing the rest.
	lister: crate::broadcast::Owner,
	// The send time an untimed track's frames carry on Lite05/06, which can't mark them
	// untimed (see `wire_timestamp`).
	runtime: crate::time::Deadlines,
	estimate: kio::ProducerWeak<crate::session::Estimate>,
}

/// Largest millisecond duration every implementation can carry losslessly.
pub(super) const MAX_SAFE_DELAY_MS: u64 = (1_u64 << 53) - 1;

/// The budget to serve a peer with, given what its wire could tell us.
///
/// A version without the field decodes as [`Duration::ZERO`], which is
/// indistinguishable from a peer genuinely asking for the live edge. Serving that
/// as real time would discard backlog a legacy subscriber never declined, so serve the
/// publisher's max age alone and leave enforcement to the receiver, exactly as the IETF
/// path does for the same reason.
fn serving_max_delay(version: Version, requested: Duration) -> Option<Duration> {
	version.carries_max_delay().then_some(requested)
}

/// The floor of a subscription's read cursor for the wire serving it.
///
/// On lite-06 there is nothing to do: `Consumer::subscribe` resolves the cursor from the
/// subscription itself, at the oldest group its own max delay still considers fresh, floored
/// at the group it named.
///
/// Pre-06 wires are the exception: their drafts define an absent `Group Start` as the
/// latest group, so say so explicitly rather than letting the budget reach back. Lite03-05
/// carry a `Subscriber Max Age`, but there it is a staleness tolerance only; Lite01/02 additionally get
/// an unbounded budget so nothing is dropped under them (see [`serving_max_delay`]), which
/// must not read as a request to replay the whole cache on join.
///
/// `latest` is the newest group when the SUBSCRIBE arrived, not once the subscription
/// resolved: anything written in between is newer, so it is delivered.
fn cursor_floor(version: Version, start_group: Option<u64>, latest: Option<u64>) -> u64 {
	if version.resolves_start() || start_group.is_some() {
		return 0;
	}
	latest.unwrap_or(0)
}

impl Shared {
	/// Watches whether the session still lets us serve `broadcast` to the peer: our
	/// grant, and the ceiling on what the peer may subscribe to.
	fn gate(&self, broadcast: &crate::Path) -> crate::auth::Gate {
		crate::auth::Gate::new(self.auth.clone(), broadcast.clone(), crate::auth::Direction::Publish)
	}

	/// The origin to resolve a peer-requested broadcast from: excludes routes
	/// through the peer, so a subscription is never served data that flowed
	/// through the subscriber. The identity is the one the peer declared in its
	/// SETUP, or the one the caller assigned it when it declared none, the same
	/// order the announce filter applies. The first call waits for the peer's
	/// SETUP (sent at startup on every lite-05+ session, well before it could
	/// learn of anything to subscribe to); the result is cached, since the peer
	/// sends exactly one SETUP per session.
	fn poll_serving_origin(&self, waiter: &kio::Waiter) -> Poll<origin::Consumer> {
		if let Some(origin) = self.serving.get() {
			return Poll::Ready(origin.clone());
		}
		// Pre-SETUP versions never declare an id, so only the assigned one applies.
		let declared = match self.version.has_setup_stream() {
			true => ready!(self.peer_setup.poll_hop(waiter)),
			false => None,
		};
		let origin = match declared.or(self.peer_hop) {
			Some(peer) => self.origin.clone().excluding(peer),
			None => self.origin.clone(),
		};
		// A concurrent first resolution may have won the race; either value is
		// identical, so keep whichever landed.
		Poll::Ready(self.serving.get_or_init(|| origin).clone())
	}
}

/// The publisher half: accepts control streams and drives each as a child state
/// machine. Resolves only on a transport error; children never end the session.
pub(super) struct Publisher {
	shared: Arc<Shared>,
	// Cloned into each control-stream child that arms timers (PROBE, announce linger).
	runtime: crate::time::Deadlines,
	// A dedicated accept handle: the poll interface takes `&mut self`, and the
	// shared context stays behind the Arc for the per-stream children.
	accept: Transport,
	children: kio::Tasks<Control>,
	/// Aborts the session on a publication outside our grant (dialing side only).
	enforce: Option<Enforce>,
}

impl Publisher {
	pub fn new(config: PublisherConfig) -> Self {
		// Identity stamped onto outbound announce hops. Derived from the
		// origin we're consuming so it matches the local relay identity
		// across every session, required for cross-session loop detection.
		let self_origin = config.origin.hop();
		let accept = config.session.clone();
		let enforce = (config.client && config.version.has_auth()).then(Enforce::default);
		Self {
			enforce,
			shared: Arc::new(Shared {
				client: config.client,
				withdrawal: Default::default(),
				session: config.session,
				// Serving the peer is not a local reader asking to discover anything.
				origin: config.origin.serving(),
				self_origin,
				peer_setup: config.peer_setup,
				peer_hop: config.peer_hop,
				serving: std::sync::OnceLock::new(),
				priority: Default::default(),
				version: config.version,
				goaway: config.goaway,
				auth: config.auth,
				peer_grant: config.peer_grant,
				owed: AtomicUsize::new(0),
				subscriptions: config.subscriptions,
				lister: crate::broadcast::Owner::new(),
				runtime: config.runtime.clone(),
				estimate: config.estimate,
			}),
			runtime: config.runtime,
			accept,
			children: kio::Tasks::new(),
		}
	}
}

impl Publisher {
	pub fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		if let Some(enforce) = &mut self.enforce
			&& let Poll::Ready(res) = enforce.poll(&self.shared, waiter)
		{
			res?;
			self.enforce = None;
		}

		let _ = self.children.poll(waiter);

		loop {
			match Stream::poll_accept(&self.accept, self.shared.version, waiter) {
				Poll::Ready(Ok(stream)) => {
					let stream = stream.with_stats(self.shared.origin.stats());
					self.children.push(Control {
						shared: self.shared.clone(),
						runtime: self.runtime.clone(),
						state: ControlState::Start { stream },
					});
				}
				Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
				Poll::Pending => break,
			}
		}

		// Newly accepted children start now rather than on the next wake.
		let _ = self.children.poll(waiter);
		Poll::Pending
	}

	/// Withdraw this session's announcements.
	pub fn close(&self) {
		self.shared.withdrawal.begin();
	}

	/// Whether withdrawals and finite control replies have reached the peer.
	pub fn drained(&self) -> bool {
		self.shared.withdrawal.drained() && self.shared.owed.load(Ordering::Relaxed) == 0
	}
}

#[cfg(test)]
impl Publisher {
	/// Test shim: drive one announce-interest stream like the old `run_announce`.
	async fn run_announce(
		stream: &mut Stream<Transport, Version>,
		origin: &origin::Consumer,
		announced: &mut announce::Consumer,
		self_origin: Hop,
		version: Version,
		auth: Option<crate::auth::Link>,
	) -> Result<(), Error> {
		let mut run = AnnounceRun::new(crate::Path::default(), self_origin, version);
		run.auth =
			auth.map(|handle| crate::auth::Gate::new(handle, crate::Path::empty(), crate::auth::Direction::Publish));
		kio::wait(|waiter| run.poll(stream, origin, announced, waiter)).await
	}
}

/// One accepted control stream, dispatched on its first varint.
struct Control {
	shared: Arc<Shared>,
	// Handed to the children that arm timers (PROBE, announce linger).
	runtime: crate::time::Deadlines,
	state: ControlState,
}

#[expect(
	clippy::large_enum_variant,
	reason = "a state machine's enum is its storage: one transient instance per stream, so the big variant is the working state, not padding held in bulk"
)]
enum ControlState {
	/// Reading the stream's type.
	Start {
		stream: Stream<Transport, Version>,
	},
	Announce(AnnounceServe),
	Subscribe(RequestServe<SubscribeServe>),
	Fetch(RequestServe<FetchServe>),
	TrackInfo(RequestServe<TrackInfoServe>),
	Tracks(RequestServe<TracksServe>),
	Probe(ProbeServe),
	Auth(AuthServe),
	/// Decoding the peer's GOAWAY, surfaced through [`crate::Session::draining`].
	Goaway {
		stream: Stream<Transport, Version>,
	},
	Done,
}

impl kio::Task for Control {
	type Output = ();

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		if let Err(err) = ready!(self.poll_serve(waiter)) {
			let err = lite::error::control(&mut self.shared.session.clone(), err);
			tracing::warn!(%err, "control stream error");
		}
		Poll::Ready(())
	}
}

impl Control {
	fn poll_serve(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		loop {
			match &mut self.state {
				ControlState::Start { stream } => {
					let kind = match ready!(stream.reader.poll_decode::<lite::ControlType>(waiter)) {
						Ok(kind) => Some(kind),
						// The only way a type decodes invalid is an unknown value.
						Err(Error::Decode(crate::DecodeError::InvalidValue)) => None,
						Err(err) => return Poll::Ready(Err(err)),
					};
					let version = self.shared.version;
					// PROBE is lite-03+, GOAWAY lite-04+, the Track Stream and FETCH lite-05+, and the
					// Auth and Tracks Streams lite-07+.
					let kind = kind.filter(|kind| match kind {
						lite::ControlType::Probe => !matches!(version, Version::Lite01 | Version::Lite02),
						lite::ControlType::Goaway => version.has_goaway(),
						lite::ControlType::Fetch | lite::ControlType::Track => version.has_track_stream(),
						lite::ControlType::Auth => version.has_auth(),
						lite::ControlType::Tracks => version.has_tracks(),
						_ => true,
					});

					let ControlState::Start { stream } = std::mem::replace(&mut self.state, ControlState::Done) else {
						unreachable!()
					};
					let Some(kind) = kind else {
						// A type this version does not know is an extension stream opened without
						// negotiating it: the draft refuses the stream and keeps the session.
						stream.writer.abort(&Error::Unsupported);
						return Poll::Ready(Ok(()));
					};
					self.state = match kind {
						lite::ControlType::Announce => {
							ControlState::Announce(AnnounceServe::new(self.shared.clone(), stream))
						}
						lite::ControlType::Subscribe => {
							ControlState::Subscribe(RequestServe::new(self.shared.clone(), stream))
						}
						lite::ControlType::Fetch => ControlState::Fetch(RequestServe::new(self.shared.clone(), stream)),
						lite::ControlType::Track => {
							ControlState::TrackInfo(RequestServe::new(self.shared.clone(), stream))
						}
						lite::ControlType::Tracks => {
							ControlState::Tracks(RequestServe::new(self.shared.clone(), stream))
						}
						lite::ControlType::Probe => {
							ControlState::Probe(ProbeServe::new(self.shared.clone(), self.runtime.clone(), stream))
						}
						lite::ControlType::Auth => ControlState::Auth(AuthServe::new(self.shared.clone(), stream)?),
						lite::ControlType::Goaway => ControlState::Goaway { stream },
						lite::ControlType::Session => return Poll::Ready(Err(Error::UnexpectedStream)),
					};
				}
				ControlState::Announce(serve) => return serve.poll(waiter),
				ControlState::Subscribe(serve) => return serve.poll(waiter),
				ControlState::Fetch(serve) => return serve.poll(waiter),
				ControlState::TrackInfo(serve) => return serve.poll(waiter),
				ControlState::Tracks(serve) => return serve.poll(waiter),
				ControlState::Probe(serve) => return serve.poll(waiter),
				ControlState::Auth(serve) => return serve.poll(waiter),
				ControlState::Goaway { stream } => {
					let version = self.shared.version;
					let uri = ready!(
						stream
							.reader
							.poll_decode_with(waiter, |r| { Ok(lite::Goaway::decode(r, version)?.uri.into_owned()) })
					)?;
					tracing::info!(uri_len = uri.len(), "received goaway");

					if !self.shared.client && !uri.is_empty() {
						return Poll::Ready(Err(Error::ProtocolViolation));
					}
					let goaway = crate::goaway::Goaway {
						uri: uri.clone(),
						// moq-lite has no timeout field on the wire.
						timeout: None,
					};

					self.shared.goaway.record(goaway)?;
					return Poll::Ready(Ok(()));
				}
				ControlState::Done => return Poll::Ready(Ok(())),
			}
		}
	}
}

/// Answers one of the peer's tokens: the app's verdict when it took the requests,
/// otherwise the grant our own origin handles allow for the connection's
/// credential. The stream lives as long as the token.
struct AuthServe {
	shared: Arc<Shared>,
	stream: Option<Stream<Transport, Version>>,
	/// Shared with the app's [`crate::auth::Request`] / [`crate::auth::Issued`], or
	/// filled in by the default acceptor.
	serving: Option<crate::auth::Serving>,
	/// The default acceptor's grant for the connection's credential, re-sent as a
	/// new limit changes it. `None` when the app answers.
	default: Option<crate::auth::DefaultGrant>,
	/// Our side is finished: FIN sent, waiting for the acknowledgement.
	finished: bool,
	unsupported: bool,
}

impl AuthServe {
	fn new(shared: Arc<Shared>, stream: Stream<Transport, Version>) -> Result<Self, Error> {
		// The Auth Stream is lite-07+ only.
		if !shared.version.has_auth() {
			return Err(Error::UnexpectedStream);
		}
		Ok(Self {
			shared,
			stream: Some(stream),
			serving: None,
			default: None,
			finished: false,
			unsupported: false,
		})
	}

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		let res = ready!(self.poll_serve(waiter)).map_err(crate::auth::protocol_error);
		let err = match res {
			Ok(()) if self.unsupported => Error::Unsupported,
			Ok(()) => Error::Cancel,
			Err(err) => {
				match &err {
					Error::Cancel
					| Error::Unsupported
					| Error::Stream(_)
					| Error::Session(_)
					| Error::Transport(_)
					| Error::IdleTimeout => {
						tracing::debug!(%err, "auth stream ended")
					}
					err => tracing::warn!(%err, "auth stream error"),
				}
				if let Some(stream) = self.stream.take() {
					stream.writer.abort(&err);
				}
				err
			}
		};
		if let Some(serving) = &self.serving {
			serving.end(err.clone());
		}
		if matches!(err, Error::ProtocolViolation) {
			self.shared.auth.close(err.clone());
			return Poll::Ready(Err(err));
		}
		Poll::Ready(Ok(()))
	}

	fn poll_serve(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		let stream = self.stream.as_mut().expect("stream present");

		let issue = match &self.serving {
			Some(serving) => serving.issue.clone(),
			None => {
				let msg = ready!(stream.reader.poll_decode::<lite::Auth>(waiter))?;
				let serving = crate::auth::Serving::new(self.shared.auth.clone());
				let issue = serving.issue.clone();
				match self.shared.auth.acceptor() {
					Some(requests) => {
						requests.push(crate::auth::Request::new(msg.token, issue.clone()));
					}
					// Only the connection's own credential has a default answer; a token
					// needs someone to verify it. Tell the presenter explicitly.
					None if !msg.token.is_empty() => {
						let mut state = issue.lock();
						state.outbox.push_back(crate::auth::Reply::Refuse {
							code: crate::SessionError::Unsupported,
							reason: Error::Unsupported.to_string(),
						});
						state.done = true;
						self.unsupported = true;
					}
					None => {
						self.default = Some(crate::auth::DefaultGrant::new(
							self.shared.auth.clone(),
							self.shared.peer_grant.clone(),
						));
					}
				}
				self.serving = Some(serving);
				issue
			}
		};

		loop {
			ready!(stream.writer.poll_flush(waiter))?;
			if self.finished {
				return stream.writer.poll_closed(waiter);
			}

			// The presenter withdrew the token, by FIN or by a cancelling reset, or its
			// stream died.
			if let Poll::Ready(res) = stream.reader.poll_read_chunk(waiter, 1).map(|res| {
				res.and_then(|chunk| match chunk {
					None => Ok(()),
					Some(_) => Err(Error::ProtocolViolation),
				})
			}) {
				match res {
					Ok(()) | Err(Error::Stream(crate::StreamError::Cancel)) => {}
					Err(err) => return Poll::Ready(Err(err)),
				}
				// That is why the token ended, whether or not our own FIN reaches a
				// presenter that left.
				issue.lock().peer.get_or_insert(Error::Cancel);
				stream.writer.finish()?;
				self.finished = true;
				continue;
			}

			if let Some(default) = &mut self.default
				&& let Poll::Ready(grant) = default.poll(waiter)
			{
				issue.lock().outbox.push_back(crate::auth::Reply::Grant(grant));
			}

			let mut state = match issue.poll(waiter, |issue| match issue.outbox.is_empty() && !issue.done {
				true => Poll::Pending,
				false => Poll::Ready(()),
			}) {
				Poll::Ready(state) => state,
				Poll::Pending => return Poll::Pending,
			};
			match state.outbox.pop_front() {
				Some(reply) => {
					drop(state);
					if !Self::write(&self.shared, stream, reply)? {
						self.unsupported = true;
						let mut state = issue.lock();
						state.outbox.clear();
						state.done = true;
					}
				}
				// The app is done with the grant, or refused the token: close our side.
				None => {
					drop(state);
					stream.writer.finish()?;
					self.finished = true;
				}
			}
		}
	}

	fn write(
		shared: &Shared,
		stream: &mut Stream<Transport, Version>,
		reply: crate::auth::Reply,
	) -> Result<bool, Error> {
		let msg = match reply {
			crate::auth::Reply::Grant(grant) => {
				let now = shared.runtime.now();
				lite::AuthReply::Ok(lite::AuthOk {
					publish: grant.publish,
					subscribe: grant.subscribe,
					expires: grant.expires.map(|at| at.saturating_duration_since(now)),
				})
			}
			crate::auth::Reply::Refuse { code, reason } => lite::AuthReply::Error(lite::AuthError {
				code: crate::lite::error::to_session_code(&code).into(),
				reason,
			}),
		};
		// Message encoding is transactional: a refused grant leaves no bytes behind.
		if let Err(err) = stream.writer.buffer(&msg) {
			tracing::debug!(%err, "auth reply cannot be encoded; refusing the token");
			stream.writer.buffer(&lite::AuthReply::Error(lite::AuthError {
				code: crate::lite::error::to_session_code(&crate::SessionError::Unsupported).into(),
				reason: Error::Unsupported.to_string(),
			}))?;
			return Ok(false);
		}
		Ok(true)
	}
}

/// Aborts the session when our origin announces a broadcast our grant does not
/// cover, instead of leaving it to wait for a subscription that never comes. See
/// [`crate::auth::Enforce`].
#[derive(Default)]
struct Enforce {
	check: crate::auth::Enforce,
	announced: Option<announce::Consumer>,
}

impl Enforce {
	fn poll(&mut self, shared: &Shared, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		let announced = match &mut self.announced {
			Some(announced) => announced,
			None => {
				let origin = ready!(shared.poll_serving_origin(waiter));
				self.announced.insert(origin.announced())
			}
		};
		let Some(path) = ready!(self.check.poll(&shared.auth, announced, waiter)) else {
			return Poll::Ready(Ok(()));
		};
		tracing::error!(
			broadcast = %crate::path::Rooted(shared.origin.root(), &path),
			"publishing outside our grant; closing the session"
		);
		let err = Error::Unauthorized;
		shared.session.clone().close(
			crate::lite::error::to_session_code(&SessionError::from(&err)),
			&crate::auth::unauthorized_reason(&path),
		);
		Poll::Ready(Err(err))
	}
}

/// Serves one PROBE stream: bandwidth estimates until the peer closes its side.
///
/// Reports when the supervisor's estimate moves enough to matter, or once the
/// last report ages out, rather than on a timer: an idle session costs nothing.
struct ProbeServe {
	shared: Arc<Shared>,
	runtime: crate::time::Deadlines,
	stream: Option<Stream<Transport, Version>>,
	/// The supervisor's estimate. Holding it is what keeps the supervisor sampling.
	estimate: kio::Consumer<crate::session::Estimate>,
	/// The estimate last weighed, so only a new one wakes this stream.
	seen: Option<crate::session::Estimate>,
	last_sent: Option<(lite::Probe, crate::time::Instant)>,
	/// When the unsent change becomes worth reporting by age alone.
	due: crate::time::Deadline,
	/// A peer streaming targets still lets the session's other tasks run.
	budget: kio::coop::Budget,
}

impl ProbeServe {
	/// The shortest age the bitrate threshold decays from.
	const PROBE_INTERVAL: Duration = Duration::from_millis(100);
	const PROBE_MAX_AGE: Duration = Duration::from_secs(10);
	const PROBE_MAX_DELTA: f64 = 0.25;
	const PROBE_RTT_DELTA: f64 = 0.25;

	/// Whether a metric moved enough to be worth another report. Gaining or
	/// losing a value always counts; both unknown never does.
	fn moved(prev: Option<u64>, next: Option<u64>, threshold: f64) -> bool {
		match (prev, next) {
			(None, None) => false,
			(Some(prev), Some(next)) => {
				if prev == 0 {
					return next != 0;
				}
				(next as f64 - prev as f64).abs() / prev as f64 >= threshold
			}
			_ => true,
		}
	}

	/// How old the last report must be before this bitrate change is worth
	/// another. The threshold decays linearly from [`Self::PROBE_MAX_DELTA`] at
	/// [`Self::PROBE_INTERVAL`] to zero at [`Self::PROBE_MAX_AGE`]: a stale estimate
	/// is worth refreshing for a smaller move.
	fn bitrate_due(prev: Option<u64>, next: Option<u64>) -> Duration {
		let (Some(prev), Some(next)) = (prev, next) else {
			return match prev == next {
				true => Self::PROBE_MAX_AGE,
				false => Duration::ZERO,
			};
		};
		if prev == 0 {
			return match next {
				0 => Self::PROBE_MAX_AGE,
				_ => Duration::ZERO,
			};
		}
		let delta = (next as f64 - prev as f64).abs() / prev as f64;
		if delta >= Self::PROBE_MAX_DELTA {
			return Duration::ZERO;
		}
		let range = Self::PROBE_MAX_AGE - Self::PROBE_INTERVAL;
		Self::PROBE_MAX_AGE - range.mul_f64(delta / Self::PROBE_MAX_DELTA)
	}

	fn new(shared: Arc<Shared>, runtime: crate::time::Deadlines, stream: Stream<Transport, Version>) -> Self {
		Self {
			estimate: shared.estimate.consume(),
			seen: None,
			shared,
			stream: Some(stream),
			last_sent: None,
			due: crate::time::Deadline::new(&runtime),
			runtime,
			budget: kio::coop::Budget::new(32),
		}
	}

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		match ready!(self.poll_probe(waiter)) {
			Ok(()) => tracing::debug!("probe stream closed"),
			Err(err) => {
				self.stream.take().expect("stream present").writer.abort(&err);
				match err {
					// The session or the peer ended it cleanly: not an error for the control stream.
					Error::Cancel
					| Error::Stream(crate::StreamError::Cancel)
					| Error::Session(crate::SessionError::Cancel) => tracing::debug!(%err, "probe stream cancelled"),
					err => {
						tracing::warn!(%err, "probe stream error");
						return Poll::Ready(Err(err));
					}
				}
			}
		}
		Poll::Ready(Ok(()))
	}

	fn poll_probe(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		let stream = self.stream.as_mut().expect("stream present");

		loop {
			// Deliver the previous estimate before weighing the next one.
			ready!(stream.writer.poll_flush(waiter))?;

			// Report ignores target values, but consumes them so buffered bytes
			// cannot hide the peer's FIN or a malformed complete message.
			loop {
				match stream.reader.poll_decode_maybe::<lite::Probe>(waiter) {
					Poll::Ready(Ok(Some(_))) => ready!(self.budget.poll_yield(waiter)),
					Poll::Ready(Ok(None)) => return Poll::Ready(Ok(())),
					Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
					Poll::Pending => break,
				}
			}

			let estimate = match self.seen {
				// The first report reads the transport: the supervisor only samples
				// for this stream from the next poll on.
				None => {
					// Mark the current value seen, so only the supervisor's next change wakes us.
					self.seen = Some(*self.estimate.read());
					crate::session::Estimate::read(&self.shared.session)
				}
				Some(seen) => {
					let changed = match self.estimate.poll(waiter, |current| match **current != seen {
						true => Poll::Ready(**current),
						false => Poll::Pending,
					}) {
						Poll::Ready(Ok(current)) => Some(current),
						// The session is ending; age alone can still refresh a report.
						Poll::Ready(Err(_)) | Poll::Pending => None,
					};
					match changed {
						Some(current) => {
							self.seen = Some(current);
							current
						}
						None => {
							ready!(self.due.poll(waiter));
							*self.estimate.read()
						}
					}
				}
			};

			// The two fields are independent on the wire, each using 0 for unknown,
			// so a transport that exposes only one still has something to report.
			// Anything this version can't carry is dropped here rather than by the
			// encoder, so it reads as unknown to every check below.
			let report = lite::Probe {
				bitrate: estimate.send_rate.map(crate::bandwidth::Rate::as_bps),
				rtt: self
					.shared
					.version
					.has_probe_rtt()
					.then(|| estimate.rtt.map(|d| d.as_millis() as u64))
					.flatten(),
			};

			// Nothing left to report. Say so once if it retracts a value the peer is
			// still holding, then stay quiet rather than repeating "unknown" every
			// time the max age comes around.
			let unknown = report.bitrate.is_none() && report.rtt.is_none();
			if unknown
				&& !self
					.last_sent
					.as_ref()
					.is_some_and(|(prev, _)| prev.bitrate.is_some() || prev.rtt.is_some())
			{
				self.due.set(None);
				continue;
			}

			let now = self.runtime.now();
			let due = match &self.last_sent {
				None => Some(now),
				Some((prev, at)) => {
					let wait = match Self::moved(prev.rtt, report.rtt, Self::PROBE_RTT_DELTA) {
						true => Duration::ZERO,
						false => Self::bitrate_due(prev.bitrate, report.bitrate),
					};
					at.checked_add(wait)
				}
			};

			if due.is_some_and(|due| due <= now) {
				stream.writer.buffer(&report)?;
				self.last_sent = Some((report, now));
				// A retraction is final until something is measured again.
				self.due.set(match unknown {
					true => None,
					false => now.checked_add(Self::PROBE_MAX_AGE),
				});
			} else {
				self.due.set(due);
			}
		}
	}
}

/// Serves one announce-interest stream: the initial set, then updates as routes,
/// demand, and the origin change.
struct AnnounceServe {
	_withdrawing: crate::session::Withdrawing,
	shared: Arc<Shared>,
	stream: Option<Stream<Transport, Version>>,
	state: AnnounceState,
}

#[kio::machine(self.state: AnnounceState)]
impl AnnounceServe {
	fn new(shared: Arc<Shared>, stream: Stream<Transport, Version>) -> Self {
		Self {
			_withdrawing: shared.withdrawal.register(),
			shared,
			stream: Some(stream),
			state: AnnounceState::Start,
		}
	}

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		// Reading the ANNOUNCE_REQUEST.
		let interest = wait!(Decode => {
			let stream = self.stream.as_mut().expect("stream present");
			stream.reader.poll_decode::<lite::AnnounceRequest>(waiter)
		})?;
		let prefix: crate::Path = interest.prefix.clone();
		let hidden: bool = interest.hidden;

		// The identity whose routes we filter out. Lite-04/05 carry it per
		// announce stream; lite-06+ reads the session-wide SETUP Hop
		// parameter, the same identity the subscribe path excludes. A peer that
		// declares nothing falls back to the identity the session assigned it.
		let assigned: Hop = self.shared.peer_hop.unwrap_or(Hop::UNKNOWN);
		let exclude_hop: Option<Hop> = match self.shared.version.has_exclude_hop() {
			true => Some(match interest.exclude_hop {
				Hop::UNKNOWN => assigned,
				hop => hop,
			}),
			// Waits on the peer's SETUP below (lite-05, whose wire carries no
			// per-stream exclude_hop).
			false if self.shared.version.has_setup_stream() => None,
			false => Some(assigned),
		};
		let exclude_hop = wait!(ExcludeHop => match exclude_hop {
			Some(hop) => Poll::Ready(*hop),
			None => self.shared.peer_setup.poll_hop(waiter).map(|hop| hop.unwrap_or(*assigned)),
		});
		let (origin, announced, run): (origin::Consumer, announce::Consumer, AnnounceRun) =
			self.start(prefix, exclude_hop, hidden);

		// Streaming announce updates.
		wait!(Run => {
			let stream = self.stream.as_mut().expect("stream present");
			if self.shared.withdrawal.poll(waiter).is_ready() {
				run.withdraw(stream, origin, announced)?;
			}
			let res = ready!(run.poll(stream, origin, announced, waiter));
			if let Err(err) = res {
				match &err {
					Error::Cancel
					| Error::Stream(crate::StreamError::Cancel)
					| Error::Session(crate::SessionError::Cancel)
					| Error::Transport(_)
					| Error::IdleTimeout => {
						tracing::debug!(prefix = %origin.root(), "announcing cancelled");
					}
					err => {
						tracing::warn!(%err, prefix = %origin.root(), "announcing error");
					}
				}
				self.stream.take().expect("stream present").writer.abort(&err);
			}
			Poll::Ready(())
		});
		Poll::Ready(Ok(()))
	}

	fn start(
		&self,
		prefix: crate::Path,
		exclude_hop: Hop,
		hidden: bool,
	) -> (origin::Consumer, announce::Consumer, AnnounceRun) {
		// If the requested prefix is outside our scope (an empty origin, or a token
		// that doesn't grant it), we simply have nothing to announce. Respond with an
		// empty set and keep the stream open (the subscriber treats a FIN here as a
		// fatal stream close), rather than erroring, which would reset the stream.
		// The wire prefix decodes as a literal path; convert it explicitly to its
		// subtree grant, refusing anything that cannot be a subtree.
		// The cursor is rooted at the prefix so every update arrives named as its
		// wire suffix, a route covering the prefix included: that one presents at
		// the root, as the empty suffix.
		let scope = crate::Pattern::subtree(prefix.as_str())
			.map(|subtree| subtree.rebase(prefix.as_str()))
			.unwrap_or_default();
		let origin = self
			.shared
			.origin
			.scope(&prefix, &scope)
			.unwrap_or_else(|_| self.shared.origin.empty());
		// Register the split-horizon peer on the announce cursor too. The origin
		// model uses this exposure to park a reflected copy before it can replace
		// the source we are currently advertising to that peer.
		let origin = origin.excluding(exclude_hop);
		// Lite03 and later carry the hop path, so only routes with room for our hop
		// under the origin's cap are advertised; Lite01/02 hide the depth entirely.
		let origin = match self.shared.version {
			Version::Lite01 | Version::Lite02 => origin,
			_ => origin.forwardable(),
		};
		// Hidden routes are left out unless the peer opted in. A publish origin that
		// already opted in (the caller's choice for this peer) keeps them either way.
		let origin = match hidden {
			true => origin.with_hidden(true),
			false => origin,
		};
		let announced = origin.announced();
		let mut run = AnnounceRun::new(prefix, self.shared.self_origin, self.shared.version);
		run.auth = Some(crate::auth::Gate::new(
			self.shared.auth.clone(),
			run.prefix.clone(),
			crate::auth::Direction::Publish,
		));
		(origin, announced, run)
	}
}

/// The announce loop's state, minus the handles it borrows per poll so the test
/// shim can supply its own.
struct AnnounceRun {
	// The requested prefix. Updates arrive named relative to it, but the grant
	// matches paths relative to the origin's root.
	prefix: crate::Path,
	self_origin: Hop,
	version: Version,
	// Lite06+: announce ids. Every `active` we send implicitly assigns the next
	// per-stream ordinal, and `ended` references the id instead of repeating the
	// path. Only announces that actually hit the wire get an id (filtered ones
	// were never seen by the peer). Lite07 also picks compression bases here.
	encoder: lite::AnnounceEncoder,
	// The routes the peer currently holds, keyed by the suffix under the requested
	// prefix.
	live: HashMap<crate::Path, Advertised>,
	phase: AnnouncePhase,
	initial: Option<usize>,
	replay: bool,
	// Our grant and the ceiling on the peer: only what both let us publish is
	// announced, and a shrink withdraws what they no longer cover.
	auth: Option<crate::auth::Gate>,
	epoch: u64,
	// What we may announce right now.
	permit: crate::auth::Permit,
}

/// What the peer holds for one advertised suffix.
struct Advertised {
	/// The announce id, on versions that assign them.
	id: Option<u64>,
	/// The chain and cost last put on the wire. The origin also reports changes the
	/// wire cannot carry (the route's source, servability), which must not restart.
	hops: Hops,
	cost: crate::origin::Cost,
}

enum AnnouncePhase {
	/// The version-specific initial burst has not been sent yet.
	Init,
	Running,
	Withdrawing,
	/// The origin ended: FIN sent, waiting for the acknowledgement.
	Closing,
}

impl AnnounceRun {
	fn new(prefix: crate::Path, self_origin: Hop, version: Version) -> Self {
		Self {
			prefix,
			self_origin,
			version,
			encoder: lite::AnnounceEncoder::new(version),
			live: HashMap::new(),
			phase: AnnouncePhase::Init,
			initial: None,
			replay: false,
			auth: None,
			epoch: 0,
			permit: Default::default(),
		}
	}

	fn sent(&mut self, stream: &mut Stream<Transport, Version>) -> Result<(), Error> {
		if let Some(remaining) = &mut self.initial {
			*remaining = remaining.saturating_sub(1);
			if *remaining == 0 {
				stream.writer.buffer(&lite::AnnounceBroadcast::Synced)?;
				self.initial = None;
			}
		}
		Ok(())
	}

	/// Whether we may announce `suffix` (relative to the requested prefix).
	fn permitted(&self, suffix: &crate::Path) -> bool {
		self.permit.matches(self.prefix.rejoin(suffix).as_str())
	}

	/// Apply a change to what we may announce: withdraw what it no longer covers and,
	/// when it grew, re-read the origin to announce what it now does.
	fn regrant(
		&mut self,
		stream: &mut Stream<Transport, Version>,
		origin: &origin::Consumer,
		permit: crate::auth::Permit,
	) -> Result<(), Error> {
		let grew = !self.permit.covers(&permit);
		self.permit = permit;
		if self.version.has_synced() {
			self.replay = true;
			return Ok(());
		}

		let revoked: Vec<_> = self
			.live
			.keys()
			.filter(|suffix| !self.permitted(suffix))
			.cloned()
			.collect();
		for suffix in revoked {
			let absolute = origin.root().rejoin(&suffix);
			tracing::debug!(route = %absolute, "announce no longer authorized");
			self.retract(stream, suffix, &absolute)?;
		}
		if !grew {
			return Ok(());
		}

		// A route withheld earlier left no trace on the cursor, so read the origin's current
		// state from a fresh one and start what the grant now covers. The cursor itself stays:
		// whatever it has pending for the routes the peer holds (an update, a restart, an end
		// still waiting out its hold) arrives through the loop as usual.
		let mut snapshot = origin.announced();
		let mut current: HashMap<crate::Path, crate::origin::Route> = HashMap::new();
		while let Some(event) = snapshot.try_next() {
			match event {
				announce::Event::Start(update)
				| announce::Event::Update(update)
				| announce::Event::Restart(update)
				| announce::Event::Takeover { announce: update, .. } => {
					current.insert(update.prefix, update.route);
				}
				announce::Event::End(update) => {
					current.remove(&update.prefix);
				}
			}
		}

		for (suffix, route) in current {
			if self.live.contains_key(&suffix) || !self.permitted(&suffix) {
				continue;
			}
			let absolute = origin.root().rejoin(&suffix);
			if let Some((hops, cost)) = self.outgoing(&route, &absolute) {
				self.advertise(stream, suffix, hops, cost, route.epoch, &absolute)?;
			}
		}
		Ok(())
	}

	/// Put `suffix` on the wire with this chain and cost: start it, restart it in place
	/// when its metadata changed, or leave it alone when the peer already holds it.
	fn advertise(
		&mut self,
		stream: &mut Stream<Transport, Version>,
		suffix: crate::Path,
		hops: Hops,
		cost: crate::origin::Cost,
		epoch: Option<crate::Epoch>,
		absolute: &crate::Path,
	) -> Result<(), Error> {
		match self.live.get_mut(&suffix) {
			// The peer would decode what it already holds.
			Some(advertised) if advertised.hops == hops && advertised.cost == cost => {}
			// A metadata update on a live advertisement: restart it in
			// place (lite-05 restarts via a duplicate ANNOUNCE).
			Some(advertised) if lite::update_supported(self.version) => {
				tracing::debug!(route = %absolute, "reannounce");
				advertised.hops = hops.clone();
				advertised.cost = cost;
				match advertised.id {
					Some(id) => {
						let hops = self.encoder.update(id, hops);
						stream
							.writer
							.buffer(&lite::AnnounceBroadcast::Update { id, hops, cost })?
					}
					// lite-05: a duplicate ANNOUNCE, which assigns no id.
					None => stream.writer.buffer(&lite::AnnounceBroadcast::Active {
						epoch: None,
						suffix: lite::PathRef::literal(suffix),
						hops: lite::HopsRef::literal(hops),
						cost,
					})?,
				}
				self.sent(stream)?;
			}
			// Pre-restart versions have no way to update a live
			// advertisement; the peer keeps the original chain.
			Some(_) => {}
			None => {
				tracing::debug!(route = %absolute, "announce");
				self.start(stream, suffix, hops, cost, epoch)?;
			}
		}
		Ok(())
	}

	/// The chain and cost to put on the wire for `route`, or `None` when it must
	/// not be forwarded.
	fn outgoing(&self, route: &crate::origin::Route, absolute: &crate::Path) -> Option<(Hops, crate::origin::Cost)> {
		let mut hops = route.hops.clone();

		// A route that already passed through us is a reflection. The origin
		// filters these on receive, so this is defensive.
		if self.self_origin != Hop::UNKNOWN && hops.contains(&self.self_origin) {
			tracing::debug!(route = %absolute, "dropping reflected route");
			return None;
		}

		// Lite05+ moves the self-stamp to the receiver, which appends our id (reported
		// once via AnnounceOk) on receipt. Older versions stamp it here, dropping if the
		// chain is full.
		if !self.version.has_announce_ok() && hops.push(self.self_origin).is_err() {
			tracing::warn!(route = %absolute, "dropping announce; hop chain at MAX_HOPS (possible loop)");
			return None;
		}

		// Pre-lite-06 wires carry no cost at all, leaving hop count as the
		// effective metric exactly as before.
		let cost = match self.version.has_route_cost() {
			true => route.cost,
			false => crate::origin::Cost::UNKNOWN,
		};
		Some((hops, cost))
	}

	/// Start advertising `suffix`, recording its announce id.
	fn start(
		&mut self,
		stream: &mut Stream<Transport, Version>,
		suffix: crate::Path,
		hops: Hops,
		cost: crate::origin::Cost,
		// Fixed while the peer holds the advertisement: a new one is a restart.
		epoch: Option<crate::Epoch>,
	) -> Result<(), Error> {
		let (id, wire, chain) = self.encoder.start(suffix.clone(), hops.clone());
		self.live.insert(suffix, Advertised { id, hops, cost });
		stream.writer.buffer(&lite::AnnounceBroadcast::Active {
			epoch,
			suffix: wire,
			hops: chain,
			cost,
		})?;
		self.sent(stream)?;
		Ok(())
	}

	/// Advertise `suffix` as continuing the peer's live `predecessor` (suffix and id):
	/// ANNOUNCE_TAKEOVER, which retires the predecessor when it held `suffix`.
	#[expect(clippy::too_many_arguments, reason = "private plumbing mirroring start and restart")]
	fn takeover(
		&mut self,
		stream: &mut Stream<Transport, Version>,
		predecessor: (crate::Path, u64),
		suffix: crate::Path,
		hops: Hops,
		cost: crate::origin::Cost,
		epoch: Option<crate::Epoch>,
		absolute: &crate::Path,
	) -> Result<(), Error> {
		tracing::debug!(route = %absolute, predecessor = %predecessor.0, "takeover");
		let (id, wire, chain) = self.encoder.takeover(predecessor.1, suffix.clone(), hops.clone());
		self.live.insert(
			suffix,
			Advertised {
				id: Some(id),
				hops,
				cost,
			},
		);
		stream.writer.buffer(&lite::AnnounceBroadcast::Takeover {
			predecessor: predecessor.1,
			suffix: wire,
			epoch,
			hops: chain,
			cost,
		})?;
		self.sent(stream)?;
		Ok(())
	}

	/// Replace the peer's advertisement for `suffix` with another publisher instance:
	/// ANNOUNCE_RESTART on lite-07, an end and a fresh start before it.
	fn restart(
		&mut self,
		stream: &mut Stream<Transport, Version>,
		suffix: crate::Path,
		hops: Hops,
		cost: crate::origin::Cost,
		epoch: Option<crate::Epoch>,
		absolute: &crate::Path,
	) -> Result<(), Error> {
		tracing::debug!(route = %absolute, "restart");
		let id = self.live.get(&suffix).and_then(|advertised| advertised.id);
		match id {
			Some(id) if self.version.has_announce_restart() => {
				self.live.insert(
					suffix,
					Advertised {
						id: Some(id),
						hops: hops.clone(),
						cost,
					},
				);
				let hops = self.encoder.update(id, hops);
				stream
					.writer
					.buffer(&lite::AnnounceBroadcast::Restart { id, epoch, hops, cost })?;
				self.sent(stream)?;
			}
			_ => {
				self.retract(stream, suffix.clone(), absolute)?;
				self.start(stream, suffix, hops, cost, epoch)?;
			}
		}
		Ok(())
	}

	/// Retract the peer's advertisement for `suffix`, if it holds one.
	fn retract(
		&mut self,
		stream: &mut Stream<Transport, Version>,
		suffix: crate::Path,
		absolute: &crate::Path,
	) -> Result<(), Error> {
		let Some(advertised) = self.live.remove(&suffix) else {
			// Filtered on the way out; the peer never saw it.
			return Ok(());
		};
		tracing::debug!(route = %absolute, "unannounce");
		match advertised.id {
			Some(id) => {
				self.encoder.end(id);
				stream.writer.buffer(&lite::AnnounceBroadcast::EndedId { id })?
			}
			// An ended announce doesn't need hops; the receiver matches on path only.
			None => stream.writer.buffer(&lite::AnnounceBroadcast::Ended {
				suffix,
				hops: Hops::new(),
			})?,
		}
		self.sent(stream)?;
		Ok(())
	}

	/// Buffer ANNOUNCE_INIT (Lite01/02) or ANNOUNCE_OK and its initial actives (Lite05+).
	fn init(
		&mut self,
		stream: &mut Stream<Transport, Version>,
		origin: &origin::Consumer,
		announced: &mut announce::Consumer,
	) -> Result<(), Error> {
		if !matches!(self.version, Version::Lite01 | Version::Lite02) && !self.version.has_announce_ok() {
			return Ok(());
		}
		if self.version.has_synced() {
			stream.writer.buffer(&lite::AnnounceOk {
				origin: self.self_origin,
				active: 0,
			})?;
			self.initial = Some(announced.pending_len());
			return Ok(());
		}
		let initial: Vec<_> = super::replay::initial(std::iter::from_fn(|| announced.try_next()))
			.into_iter()
			.filter_map(|update| {
				let suffix = update.prefix;
				if !self.permitted(&suffix) {
					return None;
				}
				let absolute = origin.root().rejoin(&suffix);
				let (hops, cost) = self.outgoing(&update.route, &absolute)?;
				tracing::debug!(route = %absolute, "announce");
				Some((suffix, hops, cost, update.route.epoch))
			})
			.collect();
		match self.version {
			Version::Lite01 | Version::Lite02 => {
				let suffixes = initial.iter().map(|(suffix, ..)| suffix.clone()).collect();
				stream.writer.buffer(&lite::AnnounceInit { suffixes })?;
				// The peer holds these now, so a later retraction or narrower grant ends them.
				for (suffix, hops, cost, _) in initial {
					self.live.insert(suffix, Advertised { id: None, hops, cost });
				}
			}
			_ => {
				stream.writer.buffer(&lite::AnnounceOk {
					origin: self.self_origin,
					active: initial.len() as u64,
				})?;
				for (suffix, hops, cost, epoch) in initial {
					self.start(stream, suffix, hops, cost, epoch)?;
				}
			}
		}
		Ok(())
	}

	/// Retract every live announcement and FIN once flushed. An unanswered request
	/// still gets its initial burst first, since the peer expects that before a FIN.
	fn withdraw(
		&mut self,
		stream: &mut Stream<Transport, Version>,
		origin: &origin::Consumer,
		announced: &mut announce::Consumer,
	) -> Result<(), Error> {
		match self.phase {
			AnnouncePhase::Withdrawing | AnnouncePhase::Closing => return Ok(()),
			AnnouncePhase::Init => self.init(stream, origin, announced)?,
			AnnouncePhase::Running => {}
		}
		for suffix in self.live.keys().cloned().collect::<Vec<_>>() {
			self.retract(stream, suffix.clone(), &origin.root().rejoin(&suffix))?;
		}
		if self.initial.take().is_some() {
			stream.writer.buffer(&lite::AnnounceBroadcast::Synced)?;
		}
		self.phase = AnnouncePhase::Withdrawing;
		Ok(())
	}

	/// Stream updates as they arrive. Closure wins the race so a dead peer can't
	/// stall on a busy announce feed.
	fn poll(
		&mut self,
		stream: &mut Stream<Transport, Version>,
		origin: &origin::Consumer,
		announced: &mut announce::Consumer,
		waiter: &kio::Waiter,
	) -> Poll<Result<(), Error>> {
		if matches!(self.phase, AnnouncePhase::Init) {
			// Start from the grant the setup token earns, so the initial set never
			// advertises something it would withdraw, or abort over, a moment later.
			if let Some(auth) = &self.auth {
				ready!(auth.poll_setup_answered(waiter));
				if let Poll::Ready(permit) = auth.poll_permit(&mut self.epoch, waiter) {
					self.permit = permit;
				}
			}
			self.init(stream, origin, announced)?;
			self.phase = AnnouncePhase::Running;
		}

		loop {
			// Deliver the buffered updates before selecting more work.
			ready!(stream.writer.poll_flush(waiter))?;

			if matches!(self.phase, AnnouncePhase::Withdrawing) {
				stream.writer.finish()?;
				self.phase = AnnouncePhase::Closing;
			}

			if matches!(self.phase, AnnouncePhase::Closing) {
				return stream.writer.poll_closed(waiter);
			}

			if self.replay {
				let revoked = self
					.live
					.keys()
					.filter(|suffix| !self.permitted(suffix))
					.cloned()
					.collect();
				announced.replay(self.live.keys(), &revoked);
				self.replay = false;
			}

			// A grant change applies before the next update, so a route it no longer
			// covers is withdrawn rather than re-sent.
			if let Some(auth) = &self.auth
				&& let Poll::Ready(permit) = auth.poll_permit(&mut self.epoch, waiter)
				&& self.permit != permit
			{
				self.regrant(stream, origin, permit)?;
				continue;
			}

			if let Poll::Ready(res) = stream.reader.poll_closed(waiter) {
				return Poll::Ready(res);
			}
			if self.initial == Some(0) {
				stream.writer.buffer(&lite::AnnounceBroadcast::Synced)?;
				self.initial = None;
				continue;
			}
			let Poll::Ready(next) = announced.poll_next(waiter) else {
				if self.initial.take().is_some() {
					stream.writer.buffer(&lite::AnnounceBroadcast::Synced)?;
					continue;
				}
				return Poll::Pending;
			};

			let (update, active, restart, predecessor) = match next {
				Some(announce::Event::Start(update) | announce::Event::Update(update)) => (update, true, false, None),
				Some(announce::Event::Restart(update)) => (update, true, true, None),
				Some(announce::Event::Takeover { announce, predecessor }) => (announce, true, false, Some(predecessor)),
				Some(announce::Event::End(update)) => (update, false, false, None),
				None => {
					if self.initial.take().is_some() {
						stream.writer.buffer(&lite::AnnounceBroadcast::Synced)?;
						continue;
					}
					// The buffer is empty (flushed at the loop top), so FIN now and
					// wait for the acknowledgement.
					stream.writer.finish()?;
					self.phase = AnnouncePhase::Closing;
					continue;
				}
			};

			let absolute = origin.root().rejoin(&update.prefix);
			let suffix = update.prefix;

			if !active || !self.permitted(&suffix) {
				self.retract(stream, suffix, &absolute)?;
				continue;
			}

			// A takeover names its predecessor's id, so only one the peer holds. Elsewhere it
			// is another instance, in place or not.
			let taken = predecessor
				.as_ref()
				.filter(|_| self.version.has_announce_takeover())
				.filter(|predecessor| **predecessor == suffix || !self.live.contains_key(&suffix))
				.and_then(|predecessor| Some((predecessor.clone(), self.live.get(predecessor)?.id?)));
			let restart = restart || predecessor.as_ref() == Some(&suffix);
			match self.outgoing(&update.route, &absolute) {
				Some((hops, cost)) if let Some((predecessor, id)) = taken => {
					self.takeover(
						stream,
						(predecessor, id),
						suffix,
						hops,
						cost,
						update.route.epoch,
						&absolute,
					)?;
				}
				Some((hops, cost)) if restart && self.live.contains_key(&suffix) => {
					self.restart(stream, suffix, hops, cost, update.route.epoch, &absolute)?;
				}
				Some((hops, cost)) => {
					self.advertise(stream, suffix, hops, cost, update.route.epoch.clone(), &absolute)?
				}
				// The chain must not be forwarded (reflected, or full): retract
				// whatever the peer holds.
				None => self.retract(stream, suffix, &absolute)?,
			}
		}
	}
}

/// Whether an error means the stream was walked away from, rather than a fault worth a warning.
fn is_cancel(err: &Error) -> bool {
	// TODO better classify WebTransport errors.
	matches!(
		err,
		Error::Cancel
			| Error::Stream(crate::StreamError::Cancel)
			| Error::Session(crate::SessionError::Cancel)
			| Error::Transport(_)
			| Error::IdleTimeout
	)
}

/// One kind of request stream (TRACK, SUBSCRIBE, FETCH), served by [`RequestServe`].
trait Request: Sized {
	/// Names the request in logs.
	const KIND: &'static str;
	/// Whether we answer the requester's FIN with our own. Otherwise we reset: the reply
	/// would end short, and a FIN there would read as a whole one.
	const GRACEFUL: bool;
	/// Whether the request counts toward the session's subscriptions peak while it is open.
	/// Its stream's credit, not a count, bounds how many the peer holds.
	const SUBSCRIPTION: bool = false;
	/// Whether the request carries track data (a subscription or a fetch), which keeps
	/// the session from reading as idle while it is open.
	const DATA: bool = false;
	/// Whether the request stays open past its reply, keeping what serving it took, until
	/// the requester closes its side.
	const HOLDS: bool;
	/// The message that opens the stream, owned so it outlives the bytes it came from.
	type Message: std::fmt::Debug;
	/// What the requester may send after the request; [`NoUpdate`] for nothing.
	type Update: for<'a> Decode<'a, Version> + std::fmt::Debug;

	/// Decode the message that opens the stream.
	fn decode(r: &mut Decoder<'_>, version: Version) -> Result<Self::Message, DecodeError>;

	/// The broadcast and track the request names.
	fn target(msg: &Self::Message) -> (&crate::Path, &str);

	/// The publisher instance the request names, if any.
	fn epoch(msg: &Self::Message) -> Option<&crate::Epoch>;

	/// Start answering, once the broadcast resolves.
	fn start(shared: &Shared, msg: Self::Message, broadcast: crate::broadcast::Consumer) -> Result<Self, Error>;

	/// Apply an update from the requester.
	fn update(&mut self, update: Self::Update);

	/// Write the reply: `Continue` after each unit of work, so the requester is checked
	/// between them even while work stays ready, and `Break` once all of it is buffered.
	fn poll(
		&mut self,
		shared: &Shared,
		writer: &mut Writer<crate::transport::boxed::SendStream, Version>,
		waiter: &kio::Waiter,
	) -> Poll<Result<ControlFlow<()>, Error>>;
}

/// The update of a request that takes none: any byte after the request is a violation.
#[derive(Debug)]
enum NoUpdate {}

impl<'a> Decode<'a, Version> for NoUpdate {
	fn decode(_: &mut Decoder<'a>, _: Version) -> Result<Self, DecodeError> {
		Err(DecodeError::ExpectedEnd)
	}
}

/// Serves one request stream: decode the request, resolve its broadcast, drive the
/// kind's reply, then FIN.
///
/// It owns the reader for the whole wait, so however the reply is waiting, the requester
/// closing its send direction (FIN or reset) ends it. An abandoned request then releases
/// its upstream lookup instead of pinning it for an answer nobody reads. Updates are not a
/// close: they reach the reply, held until it starts.
struct RequestServe<R: Request> {
	shared: Arc<Shared>,
	// Taken to abort.
	stream: Option<Stream<Transport, Version>>,
	state: RequestState<R::Message, R>,
	// Counted in `Shared::owed` until the reply is delivered, so a draining close waits for it.
	owing: bool,
	// A subscription's place in the session's subscriptions peak.
	_subscription: Option<crate::session::Slot>,
	// Holds the session busy while a subscription or fetch is open.
	_working: Option<crate::session::Working>,
	// The latest update to arrive before the reply started. Each one replaces the last.
	update: Option<R::Update>,
	/// Ends the request, even mid-reply, once the session stops allowing its broadcast.
	gate: Option<crate::auth::Gate>,
	// Log context, filled in after the decode. Relative to the origin's root, so a path
	// too long under it is refused by the lookup, on this stream alone.
	broadcast: crate::Path,
	track: String,
	// A reply that always has another group ready still lets the session's other tasks run.
	budget: kio::coop::Budget,
	// The requester's update failed to decode: the peer's fault, unlike a failed reply.
	malformed: bool,
}

enum RequestState<M, R> {
	Decode,
	/// Resolving the split-horizon origin (waits on the peer's SETUP), then the
	/// broadcast (may wait on a dynamic handler).
	Resolve {
		msg: M,
		requesting: Option<origin::Lookup>,
	},
	Serve(R),
	/// FIN the reply, and keep the request until the requester closes its side.
	Hold {
		// Held for what serving it took (TRACK: the query).
		_request: R,
		finished: bool,
		acked: bool,
	},
	/// FIN and wait for the acknowledgement.
	Finish {
		finished: bool,
	},
}

impl<R: Request> RequestServe<R> {
	fn new(shared: Arc<Shared>, stream: Stream<Transport, Version>) -> Self {
		shared.owed.fetch_add(1, Ordering::Relaxed);
		let _working = R::DATA.then(|| shared.session.work().start());
		Self {
			shared,
			stream: Some(stream),
			state: RequestState::Decode,
			owing: true,
			_subscription: None,
			_working,
			update: None,
			gate: None,
			broadcast: Default::default(),
			track: Default::default(),
			budget: kio::coop::Budget::new(32),
			malformed: false,
		}
	}

	/// The reply no longer owes the peer anything a draining close should wait for.
	fn settle(&mut self) {
		if std::mem::take(&mut self.owing) {
			self.shared.owed.fetch_sub(1, Ordering::Relaxed);
		}
	}

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		match ready!(self.poll_serve(waiter)) {
			Ok(()) => {
				tracing::debug!(broadcast = %crate::path::Rooted(self.shared.origin.root(), &self.broadcast), track = %self.track, "{} complete", R::KIND);
				Poll::Ready(Ok(()))
			}
			// The peer's own malformed bytes are a control violation, even after request setup.
			Err(err) if matches!(self.state, RequestState::Decode) || self.malformed => Poll::Ready(Err(err)),
			Err(err) => {
				match &err {
					err if is_cancel(err) => {
						tracing::debug!(broadcast = %crate::path::Rooted(self.shared.origin.root(), &self.broadcast), track = %self.track, "{} cancelled", R::KIND)
					}
					Error::Unauthorized | Error::Stream(crate::StreamError::Unauthorized) => {
						tracing::info!(broadcast = %crate::path::Rooted(self.shared.origin.root(), &self.broadcast), track = %self.track, "{} unauthorized", R::KIND)
					}
					err => {
						tracing::warn!(broadcast = %crate::path::Rooted(self.shared.origin.root(), &self.broadcast), track = %self.track, %err, "{} error", R::KIND)
					}
				}
				self.stream.take().expect("stream present").writer.abort(&err);
				Poll::Ready(Ok(()))
			}
		}
	}

	fn poll_serve(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		if let Some(gate) = &mut self.gate
			&& gate.poll_denied(waiter).is_ready()
		{
			return Poll::Ready(Err(Error::Unauthorized));
		}
		loop {
			ready!(self.budget.poll_yield(waiter));
			// Once answered, the requester's FIN is the normal end, not a cancel.
			if matches!(self.state, RequestState::Resolve { .. } | RequestState::Serve(_))
				&& let Poll::Ready(next) = self.poll_requester(waiter)
			{
				// One update per pass, so the budget above charges each; reading again before
				// serving keeps the reader's waker registered.
				if let Some(update) = next? {
					self.apply(update);
					continue;
				}
				if !R::GRACEFUL {
					return Poll::Ready(Err(Error::Cancel));
				}
				// Drop the reply mid-flight: anything half-sent is cancelled rather than
				// completed for nobody.
				self.state = RequestState::Finish { finished: false };
			}

			let stream = self.stream.as_mut().expect("stream present");
			match &mut self.state {
				RequestState::Decode => {
					let version = self.shared.version;
					let msg = ready!(stream.reader.poll_decode_with(waiter, |r| R::decode(r, version)))?;
					let (broadcast, track) = R::target(&msg);
					self.broadcast = broadcast.clone();
					self.track = track.to_string();
					if R::SUBSCRIPTION {
						self._subscription = Some(self.shared.subscriptions.hold());
					}
					tracing::debug!(broadcast = %crate::path::Rooted(self.shared.origin.root(), &self.broadcast), track = %self.track, request = ?msg, "{} started", R::KIND);
					// Checked before anything is resolved, so a broadcast the session does not
					// allow is never served, not just cut off later.
					let mut gate = self.shared.gate(R::target(&msg).0);
					let denied = gate.poll_denied(waiter).is_ready();
					self.gate = Some(gate);
					self.state = RequestState::Resolve { msg, requesting: None };
					if denied {
						return Poll::Ready(Err(Error::Unauthorized));
					}
				}
				RequestState::Resolve { msg, requesting } => {
					if requesting.is_none() {
						// The peer requested this exact path, so it has already seen an
						// announcement for it. The immediate lookup resolves it at once, is
						// served on demand by the route covering it (an `origin::Requests`), or
						// errors when there is none.
						let origin = ready!(self.shared.poll_serving_origin(waiter));
						let path = R::target(msg).0.clone();
						*requesting = Some(origin.lookup(path, R::epoch(msg)).into_inner());
					}
					let broadcast = ready!(requesting.as_ref().expect("requesting").poll_ok(waiter))?;
					// Any placeholder but `Decode`, so a failed start is answered on the stream.
					let RequestState::Resolve { msg, .. } =
						std::mem::replace(&mut self.state, RequestState::Finish { finished: false })
					else {
						unreachable!()
					};
					let mut request = R::start(&self.shared, msg, broadcast)?;
					if let Some(update) = self.update.take() {
						request.update(update);
					}
					self.state = RequestState::Serve(request);
				}
				RequestState::Serve(request) => {
					if ready!(request.poll(&self.shared, &mut stream.writer, waiter))?.is_break() {
						let RequestState::Serve(request) =
							std::mem::replace(&mut self.state, RequestState::Finish { finished: false })
						else {
							unreachable!()
						};
						if R::HOLDS {
							self.state = RequestState::Hold {
								_request: request,
								finished: false,
								acked: false,
							};
						}
					}
				}
				RequestState::Hold { finished, acked, .. } => {
					if !*finished {
						// A reply blocked on flow control may never unblock, so a requester
						// leaving before it is out cancels it and lets go of the request.
						if stream.writer.poll_flush(waiter)?.is_pending() {
							let closed = ready!(self.poll_closed(waiter));
							self.state = RequestState::Finish { finished: false };
							closed?;
							return Poll::Ready(Err(Error::Cancel));
						}
						stream.writer.finish()?;
						*finished = true;
					}
					// The reply is the peer's once acknowledged; the hold itself owes nothing.
					let delivered = *acked || stream.writer.poll_close(waiter).is_ready();
					*acked = delivered;
					if delivered {
						self.settle();
					}
					let closed = ready!(self.poll_closed(waiter));
					// Let go of the request now, not when the serve is dropped.
					self.state = RequestState::Finish { finished: true };
					// A FIN or a reset both end the requester's interest; only a stray byte
					// is a fault.
					if let Err(err) = closed
						&& !is_cancel(&err)
					{
						return Poll::Ready(Err(err));
					}
					if delivered {
						return Poll::Ready(Ok(()));
					}
				}
				RequestState::Finish { finished } => {
					if !*finished {
						ready!(stream.writer.poll_flush(waiter))?;
						stream.writer.finish()?;
						*finished = true;
					}
					// A transport ACK does not say the application read the tail: a lite-07
					// subscriber FINs once its tail accounting settles, so wait for that.
					if R::GRACEFUL && self.shared.version.waits_for_subscriber_fin() {
						ready!(self.poll_closed(waiter))?;
					}
					let stream = self.stream.as_mut().expect("stream present");
					return stream.writer.poll_close(waiter);
				}
			}
		}
	}

	/// Read the requester's next message while we answer: an update, or `None` once it FINs
	/// its send direction.
	fn poll_requester(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<R::Update>, Error>> {
		let stream = self.stream.as_mut().expect("stream present");
		let res = ready!(stream.reader.poll_decode_maybe::<R::Update>(waiter));
		if let Err(err) = &res {
			self.malformed = matches!(err, Error::Decode(_));
		}
		Poll::Ready(res)
	}

	/// Ready once the requester closes its send direction, `Ok` for a FIN. Updates until then
	/// still reach the reply, each charged to the budget.
	fn poll_closed(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		while let Some(update) = ready!(self.poll_requester(waiter))? {
			self.apply(update);
			ready!(self.budget.poll_yield(waiter));
		}
		Poll::Ready(Ok(()))
	}

	/// Each update reaches the reply, or waits for it to start.
	fn apply(&mut self, update: R::Update) {
		match &mut self.state {
			RequestState::Serve(request) => request.update(update),
			_ => self.update = Some(update),
		}
	}
}

impl<R: Request> Drop for RequestServe<R> {
	fn drop(&mut self) {
		self.settle();
	}
}

/// Answers a TRACK stream with the track's TRACK_INFO.
///
/// The open stream is interest in the track: the query that answered it stays until the
/// requester closes its side, so demand holds while the requester moves on to SUBSCRIBE.
struct TrackInfoServe {
	querying: track::Resolving,
}

impl Request for TrackInfoServe {
	const KIND: &'static str = "track info";
	const GRACEFUL: bool = false;
	const HOLDS: bool = true;
	type Message = lite::Track<'static>;
	type Update = NoUpdate;

	fn decode(r: &mut Decoder<'_>, version: Version) -> Result<Self::Message, DecodeError> {
		lite::Track::decode(r, version).map(lite::Track::into_owned)
	}

	fn target(msg: &Self::Message) -> (&crate::Path, &str) {
		(&msg.broadcast, &msg.track)
	}

	fn epoch(msg: &Self::Message) -> Option<&crate::Epoch> {
		msg.epoch.as_ref()
	}

	fn start(_: &Shared, msg: Self::Message, broadcast: crate::broadcast::Consumer) -> Result<Self, Error> {
		let querying = broadcast.track(&msg.track)?.info().into_inner();
		Ok(Self { querying })
	}

	fn update(&mut self, update: NoUpdate) {
		match update {}
	}

	fn poll(
		&mut self,
		shared: &Shared,
		writer: &mut Writer<crate::transport::boxed::SendStream, Version>,
		waiter: &kio::Waiter,
	) -> Poll<Result<ControlFlow<()>, Error>> {
		let ietf = ready!(shared.peer_setup.poll_ietf(waiter));
		let info = ready!(self.querying.poll_ok(waiter))?;
		crate::ietf::flate::egress(&info, shared.version.has_compression())?;

		// TRACK_INFO only flows on Lite05+ (the encode errors otherwise).
		writer.buffer(&lite::TrackInfo::serve(&info, shared.version, ietf))?;
		if ietf {
			writer.buffer(&lite::ietf::TrackProperties(info.properties.clone()))?;
		}
		Poll::Ready(Ok(ControlFlow::Break(())))
	}
}

/// Answers a TRACKS stream: the broadcast's listed tracks, SYNCED, then each change,
/// until the broadcast ends or a newer listing of it on the session replaces this one.
///
/// Each listing queues the broadcast's whole listed set, so the session lists each
/// broadcast once: duplicates would multiply that memory and every change's fanout. A
/// newer listing replaces the older rather than being refused, since a subscriber that
/// stops listing and lists again can open the new stream before we see the old one close.
struct TracksServe {
	tracks: crate::broadcast::Tracks,
}

impl Request for TracksServe {
	const KIND: &'static str = "tracks";
	const GRACEFUL: bool = false;
	const HOLDS: bool = false;
	type Message = lite::Tracks;
	type Update = NoUpdate;

	fn decode(r: &mut Decoder<'_>, version: Version) -> Result<Self::Message, DecodeError> {
		lite::Tracks::decode(r, version)
	}

	fn target(msg: &Self::Message) -> (&crate::Path, &str) {
		(&msg.broadcast, "")
	}

	fn epoch(msg: &Self::Message) -> Option<&crate::Epoch> {
		msg.epoch.as_ref()
	}

	fn start(shared: &Shared, _: Self::Message, broadcast: crate::broadcast::Consumer) -> Result<Self, Error> {
		Ok(Self {
			tracks: broadcast.tracks_as(shared.lister)?,
		})
	}

	fn update(&mut self, update: NoUpdate) {
		match update {}
	}

	fn poll(
		&mut self,
		shared: &Shared,
		writer: &mut Writer<crate::transport::boxed::SendStream, Version>,
		waiter: &kio::Waiter,
	) -> Poll<Result<ControlFlow<()>, Error>> {
		// Pull an event only once the last is out, so a stalled requester's changes collapse
		// in the listing's queue rather than pile up here.
		ready!(writer.poll_flush(waiter))?;
		let event = match ready!(self.tracks.poll_next(waiter))? {
			Some(crate::broadcast::TracksEvent::Added { name, info }) => lite::TracksEvent::Added {
				name: name.to_string().into(),
				// Untimed even to an extension peer: TRACKS_ADD has no TRACK_PROPERTIES to
				// carry the unchecked provenance, so a relay would re-list it as native lite.
				info: lite::TrackInfo::serve(&info, shared.version, false),
			},
			Some(crate::broadcast::TracksEvent::Removed { name }) => lite::TracksEvent::Removed {
				name: name.to_string().into(),
			},
			Some(crate::broadcast::TracksEvent::Finished { name }) => lite::TracksEvent::Finished {
				name: name.to_string().into(),
			},
			Some(crate::broadcast::TracksEvent::Synced) => lite::TracksEvent::Synced,
			None => return Poll::Ready(Ok(ControlFlow::Break(()))),
		};
		writer.buffer(&event)?;
		Poll::Ready(Ok(ControlFlow::Continue(())))
	}
}

/// Answers a SUBSCRIBE stream: streams the track's groups (and best-effort datagrams)
/// until the track ends.
enum SubscribeServe {
	/// A time anchor waits for the track's timescale, which places it, before the model
	/// subscription opens.
	Query {
		/// Boxed: the request is far larger than the other states' handles.
		msg: Box<lite::Subscribe<'static>>,
		track: track::Consumer,
		querying: track::Resolving,
		/// The newest group when the SUBSCRIBE arrived; see [`cursor_floor`].
		latest: Option<u64>,
		update: Option<lite::SubscribeUpdate>,
	},
	/// Waiting for the model subscription to be confirmed.
	Confirm {
		/// Boxed: the request is far larger than the other states' handles.
		msg: Box<lite::Subscribe<'static>>,
		bounds: Bounds,
		subscribing: track::Subscribing,
		/// The newest group when the SUBSCRIBE arrived; see [`cursor_floor`].
		latest: Option<u64>,
		update: Option<lite::SubscribeUpdate>,
	},
	/// Streaming groups and datagrams. Boxed: by far the largest state, and the enum
	/// is moved on every transition.
	Run(Box<TrackRun>),
	/// The track finished: draining the in-flight group streams before the FIN.
	Drain {
		children: kio::Tasks<GroupServe>,
		/// The subscription's demand lasts until its groups drain. A relay cancels its
		/// upstream subscription once nobody subscribes, and the publisher then resets
		/// every group still on the wire. `None` only in passing between states.
		_track: Option<Box<track::Subscriber>>,
	},
}

impl Request for SubscribeServe {
	const KIND: &'static str = "subscribed";
	// Ending a subscription early is still a valid end.
	const GRACEFUL: bool = true;
	const SUBSCRIPTION: bool = true;
	const DATA: bool = true;
	const HOLDS: bool = false;
	type Message = lite::Subscribe<'static>;
	type Update = lite::SubscribeUpdate;

	fn decode(r: &mut Decoder<'_>, version: Version) -> Result<Self::Message, DecodeError> {
		lite::Subscribe::decode(r, version).map(lite::Subscribe::into_owned)
	}

	fn target(msg: &Self::Message) -> (&crate::Path, &str) {
		(&msg.broadcast, &msg.track)
	}

	fn epoch(msg: &Self::Message) -> Option<&crate::Epoch> {
		msg.epoch.as_ref()
	}

	fn start(shared: &Shared, msg: Self::Message, broadcast: crate::broadcast::Consumer) -> Result<Self, Error> {
		let track = broadcast.track(&msg.track)?;
		let latest = track.latest_sequence();
		if Bounds::has_time(&msg.ranges) {
			let querying = track.info().into_inner();
			return Ok(Self::Query {
				msg: Box::new(msg),
				track,
				querying,
				latest,
				update: None,
			});
		}
		let bounds = Bounds::new(&msg.ranges, None)?;
		Ok(Self::subscribe(shared, msg, bounds, &track, latest, None))
	}

	fn update(&mut self, update: lite::SubscribeUpdate) {
		match self {
			Self::Query { update: pending, .. } | Self::Confirm { update: pending, .. } => *pending = Some(update),
			Self::Run(run) => run.update(update),
			// The track is over: nothing left to steer.
			Self::Drain { .. } => {}
		}
	}

	fn poll(
		&mut self,
		shared: &Shared,
		writer: &mut Writer<crate::transport::boxed::SendStream, Version>,
		waiter: &kio::Waiter,
	) -> Poll<Result<ControlFlow<()>, Error>> {
		loop {
			match self {
				Self::Query { querying, .. } => {
					let ietf = ready!(shared.peer_setup.poll_ietf(waiter));
					let info = ready!(querying.poll_ok(waiter))?;
					let Self::Query {
						msg,
						track,
						latest,
						update,
						..
					} = std::mem::replace(
						self,
						Self::Drain {
							children: Default::default(),
							_track: None,
						},
					)
					else {
						unreachable!()
					};
					let bounds = Bounds::new(&msg.ranges, info.lite_timescale(ietf))?;
					*self = Self::subscribe(shared, *msg, bounds, &track, latest, update);
				}
				Self::Confirm { subscribing, .. } => {
					let ietf = ready!(shared.peer_setup.poll_ietf(waiter));
					let track = ready!(subscribing.poll_ok(waiter))?;
					crate::ietf::flate::egress(track.info(), shared.version.has_compression())?;
					let Self::Confirm {
						msg,
						bounds,
						latest,
						update,
						..
					} = std::mem::replace(
						self,
						Self::Drain {
							children: Default::default(),
							_track: None,
						},
					)
					else {
						unreachable!()
					};

					// `None` omits the per-frame timestamp prefix: an untimed track on
					// Lite07+, or any track before Lite05 (whose frames still carry
					// timestamps in the model, just not on this wire).
					let stamp = Stamp::new(shared.version, track.info().lite_timescale(ietf));

					// Lite05+ accepts implicitly: no SUBSCRIBE_OK, the immutable properties
					// live in TRACK_INFO, and the resolved range arrives as
					// SUBSCRIBE_START/END from the run loop. Older drafts still acknowledge
					// with SUBSCRIBE_OK here.
					if !shared.version.has_track_stream() {
						let info = lite::SubscribeOk {
							priority: msg.priority,
							max_delay: Duration::ZERO,
							start_group: None,
							end_group: None,
						};
						writer.buffer(&lite::SubscribeResponse::Ok(info))?;
					}

					// Track-level subscriber priority and group order. SUBSCRIBE_UPDATE
					// messages broadcast new values to the run loop (so future groups inherit
					// them) and the in-flight group machines (so they update via
					// PriorityHandle::steer).
					let steer = Steer {
						priority: msg.priority,
						order: msg.order,
					};
					let steer_tx = kio::Producer::new(steer);

					let session = shared.session.clone();
					let publisher = track.info().priority;
					let group = session.send_group(crate::priority::group(msg.priority, publisher));
					let sub = Subscription {
						session,
						id: msg.id,
						track_name: Arc::from(track.name()),
						priority: shared.priority.clone(),
						publisher,
						group,
						steer: steer_tx.consume(),
						steer_seen: steer,
						version: shared.version,
						stamp,
						runtime: shared.runtime.clone(),
						outcomes: Default::default(),
						ietf,
					};

					let floor = cursor_floor(shared.version, bounds.groups().start(), latest);
					let mut run = TrackRun::new(sub, track, bounds, floor, steer_tx);
					if let Some(update) = update {
						run.update(update);
					}
					*self = Self::Run(Box::new(run));
				}
				Self::Run(run) => {
					// The live edge reached the boundary, so SUBSCRIBE_END was already sent (or
					// the version predates the track stream), or a bounded lite-07
					// subscription is covered. Drain the in-flight group machines, then FIN.
					if ready!(run.poll_step(writer, waiter))?.is_continue() {
						return Poll::Ready(Ok(ControlFlow::Continue(())));
					}
					let Self::Run(run) = std::mem::replace(
						self,
						Self::Drain {
							children: Default::default(),
							_track: None,
						},
					) else {
						unreachable!()
					};
					let TrackRun { children, track, .. } = *run;
					*self = Self::Drain {
						children,
						_track: Some(Box::new(track)),
					};
				}
				Self::Drain { children, .. } => {
					ready!(children.poll(waiter));
					return Poll::Ready(Ok(ControlFlow::Break(())));
				}
			}
		}
	}
}

impl SubscribeServe {
	/// Open the model subscription for `msg`'s `bounds`.
	///
	/// One subscriber for the whole subscription: the run loop polls its groups and its
	/// best-effort datagrams from this single cursor, so a group-only or datagram-only track
	/// opens exactly one subscription (no duplicate demand).
	fn subscribe(
		shared: &Shared,
		msg: lite::Subscribe<'static>,
		bounds: Bounds,
		track: &track::Consumer,
		latest: Option<u64>,
		update: Option<lite::SubscribeUpdate>,
	) -> Self {
		let subscription = crate::track::Subscription {
			priority: msg.priority,
			max_delay: serving_max_delay(shared.version, msg.max_delay),
			order: msg.order,
			..bounds.groups()
		};
		let subscribing = track.subscribe(subscription).into_inner();
		Self::Confirm {
			msg: Box::new(msg),
			bounds,
			subscribing,
			latest,
			update,
		}
	}
}

/// Answers a FETCH stream: a single cached group, streamed in order.
#[allow(
	clippy::large_enum_variant,
	reason = "a state machine's enum is its storage: one transient instance per stream, so the big variant is the working state, not padding held in bulk; an allow, since the size gap varies by target"
)]
enum FetchServe {
	/// Waiting for the fetched group.
	Fetch {
		msg: lite::Fetch<'static>,
		fetching: track::Fetching,
	},
	/// Streaming the group's frames in order. The delta-timestamp baseline
	/// resets to 0, so the first served frame's delta is its absolute timestamp
	/// (the subscriber decodes against the same baseline).
	Serve {
		group: group::Consumer,
		stamp: Option<Stamp>,
		prev_ts: u64,
		frame: Option<frame::Consumer>,
		chunk: Option<crate::frame::Payload>,
		batch: Box<Objects>,
		batch_pos: usize,
		/// The `ietf` extension's object records, when negotiated.
		records: Option<lite::ietf::Records>,
	},
}

/// A batch of frames read for egress, each with what the wire needs beside it.
type Objects = arrayvec::ArrayVec<frame::Object, 8>;

impl Request for FetchServe {
	const KIND: &'static str = "fetch";
	// The response carries no header, so a run cut short would read as whole.
	const GRACEFUL: bool = false;
	const DATA: bool = true;
	const HOLDS: bool = false;
	type Message = lite::Fetch<'static>;
	type Update = NoUpdate;

	fn decode(r: &mut Decoder<'_>, version: Version) -> Result<Self::Message, DecodeError> {
		lite::Fetch::decode(r, version).map(lite::Fetch::into_owned)
	}

	fn target(msg: &Self::Message) -> (&crate::Path, &str) {
		(&msg.broadcast, &msg.track)
	}

	fn epoch(msg: &Self::Message) -> Option<&crate::Epoch> {
		msg.epoch.as_ref()
	}

	fn start(_: &Shared, msg: Self::Message, broadcast: crate::broadcast::Consumer) -> Result<Self, Error> {
		let fetching = broadcast
			.track(&msg.track)?
			.fetch_range(
				msg.group,
				track::FetchOptions {
					options: group::Fetch {
						priority: msg.priority,
						max_delay: msg.max_delay,
						frame_start: msg.start_frame,
					},
					end: msg.end_frame.and_then(|end| end.checked_add(1)),
				},
			)
			.into_inner();
		Ok(Self::Fetch { msg, fetching })
	}

	fn update(&mut self, update: NoUpdate) {
		match update {}
	}

	fn poll(
		&mut self,
		shared: &Shared,
		writer: &mut Writer<crate::transport::boxed::SendStream, Version>,
		waiter: &kio::Waiter,
	) -> Poll<Result<ControlFlow<()>, Error>> {
		loop {
			match self {
				Self::Fetch { msg, fetching } => {
					// Before the group: a fetched group polled while this waited would be lost.
					let ietf = ready!(shared.peer_setup.poll_ietf(waiter));
					let group = ready!(kio::Task::poll(fetching, waiter))?.with_range(msg.start_frame..);

					// The response carries no header, so a short run is indistinguishable
					// from one that started elsewhere: only serve a range we can cover
					// exactly. `fetch_group` already positions the consumer, so this is a
					// belt-and-braces check on a promise the wire can't restate.
					if group.index() != msg.start_frame {
						return Poll::Ready(Err(Error::Lagged));
					}
					// The end is a serving cap only: the cached group runs to the end of
					// the group so it stays usable for anyone else (see
					// `group::Fetch::frame_start`). Only the end, so the start stays where
					// the check above found it.
					let mut group = group.with_end(msg.end_frame.and_then(|end| end.checked_add(1)));

					// Matches the timescale TRACK_INFO declared for the track.
					let stamp = Stamp::new(shared.version, group.lite_timescale(ietf));
					// And its compression, which a lite-07 peer receives as stored.
					if shared.version.has_compression() {
						group = group.compressed();
					}
					let capped = msg.end_frame.is_some();
					let timescale = stamp.map(|stamp| stamp.timescale);
					let records = lite::ietf::Records::start(writer, ietf, &group, capped, timescale)?;

					*self = Self::Serve {
						group,
						stamp,
						prev_ts: 0,
						frame: None,
						chunk: None,
						batch: Box::default(),
						batch_pos: 0,
						records,
					};
				}
				Self::Serve {
					group,
					stamp,
					prev_ts,
					frame,
					chunk,
					batch,
					batch_pos,
					records,
				} => loop {
					ready!(writer.poll_flush(waiter))?;
					if let Some(pending) = chunk {
						ready!(writer.poll_write(waiter, pending))?;
						if !bytes::Buf::has_remaining(pending) {
							*chunk = None;
						}
					} else if let Some(pending) = frame {
						match ready!(pending.poll_read_chunk(waiter))? {
							Some(next) => *chunk = Some(next.into()),
							None => *frame = None,
						}
					} else if *batch_pos < batch.len() {
						let index = group.index() - batch.len() as u64 + *batch_pos as u64;
						let batched = &mut batch[*batch_pos];
						if let Some(records) = records {
							records.frame(writer, index, batched.fields.as_deref())?;
						}
						buffer_frame_info(
							writer,
							batched.frame.timestamp,
							batched.inflated,
							batched.frame.payload.len() as u64,
							*stamp,
							prev_ts,
							&shared.runtime,
						)?;
						let payload = std::mem::take(&mut batched.frame.payload);
						if !payload.is_empty() {
							*chunk = Some(payload);
						}
						*batch_pos += 1;
						group.keep_alive();
					} else {
						match group.poll_read_objects(waiter, batch) {
							Poll::Ready(Ok(count)) if count > 0 => {
								*batch_pos = 0;
								return Poll::Ready(Ok(ControlFlow::Continue(())));
							}
							Poll::Ready(Ok(_)) => {
								if let Some(records) = records {
									records.end(writer, group)?;
								}
								return Poll::Ready(Ok(ControlFlow::Break(())));
							}
							Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
							Poll::Pending => {}
						}
						match ready!(group.poll_next_frame(waiter))? {
							Some(next) => {
								if let Some(records) = records {
									records.frame(writer, group.index() - 1, next.fields.as_deref())?;
								}
								buffer_frame_info(
									writer,
									next.timestamp,
									next.inflated,
									next.size,
									*stamp,
									prev_ts,
									&shared.runtime,
								)?;
								*frame = Some(next);
								return Poll::Ready(Ok(ControlFlow::Continue(())));
							}
							None => {
								if let Some(records) = records {
									records.end(writer, group)?;
								}
								return Poll::Ready(Ok(ControlFlow::Break(())));
							}
						}
					}
				},
			}
		}
	}
}

#[cfg(test)]
mod test {
	use super::*;
	use crate::{Timestamp, broadcast};

	fn track_producer(name: impl Into<Arc<str>>) -> track::Producer {
		track::Producer::new(Arc::new(broadcast::Info::default()), name, crate::track::Info::timed())
	}

	/// A pre-06 wire is served from the live edge when it names no start: those drafts
	/// define an absent `Group Start` as the latest group, and Lite01/02 additionally get
	/// an unbounded budget (so nothing is dropped under them) that must not read as a
	/// request to replay the whole cache.
	#[test]
	fn a_pre06_wire_is_pinned_to_the_live_edge() {
		use futures::FutureExt;

		let producer = track_producer("test");
		for second in 0..3 {
			let mut group = producer.append_group().unwrap();
			group
				.write_frame(Timestamp::from_millis(second * 1000).unwrap(), b"x".to_vec())
				.unwrap();
			group.finish().unwrap();
		}

		// What run_subscribe hands the model for a peer that sent no budget at all.
		let served = |version| {
			producer.subscribe(
				track::Subscription::default().with_max_delay(serving_max_delay(version, std::time::Duration::ZERO)),
			)
		};
		let drain = |subscriber: &mut track::Subscriber| {
			let mut sequences = Vec::new();
			while let Some(Ok(Some(group))) = subscriber.recv_group().now_or_never() {
				sequences.push(group.sequence);
			}
			sequences
		};

		let mut unpinned = served(Version::Lite01);
		assert_eq!(
			drain(&mut unpinned),
			vec![0, 1, 2],
			"the unbounded budget alone resolves to the whole cache"
		);

		let legacy = served(Version::Lite01);
		let mut legacy = legacy.with_range(cursor_floor(Version::Lite01, None, producer.latest_sequence())..);
		assert_eq!(drain(&mut legacy), vec![2]);

		// Lite03-05 declare a budget, but their drafts define it as a staleness tolerance
		// and an absent start as the latest group, so they are pinned all the same.
		let tolerant =
			producer.subscribe(track::Subscription::default().with_max_delay(std::time::Duration::from_secs(5)));
		let mut tolerant = tolerant.with_range(cursor_floor(Version::Lite05, None, producer.latest_sequence())..);
		assert_eq!(drain(&mut tolerant), vec![2]);

		// On lite-06 the declared budget is what resolves the start, so it stands.
		let declared =
			producer.subscribe(track::Subscription::default().with_max_delay(std::time::Duration::from_secs(5)));
		let mut declared = declared.with_range(cursor_floor(Version::Lite06, None, producer.latest_sequence())..);
		assert_eq!(drain(&mut declared), vec![0, 1, 2]);
	}

	/// An unfloored join adopts the first served group: the cursor rises to it, so a
	/// group created below it is not served. An explicit floor does not; see
	/// [`explicit_floor_delivers_a_late_lower_group`].
	#[kio::test]
	async fn start_floor_suppresses_late_lower_arrivals() {
		use futures::FutureExt;

		let mut producer = track_producer("test");
		let mut subscriber = producer.subscribe(None);

		let write = |producer: &mut track::Producer, sequence: u64| {
			let mut group = producer.create_group(crate::group::Info { sequence }).unwrap();
			group
				.write_frame(Timestamp::from_millis(1).unwrap(), b"x".to_vec())
				.unwrap();
			group.finish().unwrap();
		};

		// Group 7 arrives first: run_track resolves the start there and raises
		// the floor.
		write(&mut producer, 7);
		match recv_next(&mut subscriber, false, false).await.unwrap() {
			Recv::Group(group) => {
				assert_eq!(group.sequence, 7);
				subscriber.set_range(group.sequence..);
			}
			_ => panic!("expected the first group"),
		}

		// Group 5 lands late: below the resolved start, it must not be served.
		write(&mut producer, 5);
		assert!(
			recv_next(&mut subscriber, false, false).now_or_never().is_none(),
			"a group below the resolved start must be suppressed"
		);
	}

	/// The explicit-floor twin of [`start_floor_suppresses_late_lower_arrivals`].
	/// The subscription named group 0, so group 5 is still inside the floor when
	/// it is created after group 7.
	#[kio::test]
	async fn explicit_floor_delivers_a_late_lower_group() {
		let mut producer = track_producer("test");
		let mut subscriber = producer.subscribe(
			track::Subscription::default()
				.with_start(0)
				.with_max_delay(std::time::Duration::from_secs(60)),
		);

		let write = |producer: &mut track::Producer, sequence: u64| {
			let mut group = producer.create_group(crate::group::Info { sequence }).unwrap();
			group
				.write_frame(Timestamp::from_millis(1).unwrap(), b"x".to_vec())
				.unwrap();
			group.finish().unwrap();
		};

		write(&mut producer, 7);
		match recv_next(&mut subscriber, false, false).await.unwrap() {
			Recv::Group(group) => assert_eq!(group.sequence, 7),
			_ => panic!("expected the first group"),
		}

		write(&mut producer, 5);
		match recv_next(&mut subscriber, false, false).await.unwrap() {
			Recv::Group(group) => assert_eq!(group.sequence, 5),
			_ => panic!("expected the late group above the floor"),
		}
	}

	#[kio::test]
	async fn recv_next_drains_datagram_before_finished() {
		let mut producer = track_producer("test");
		let mut subscriber = producer.subscribe(None);

		producer
			.append_datagram(Timestamp::from_millis(1).unwrap(), &b"last"[..])
			.unwrap();
		producer.finish().unwrap();

		match recv_next(&mut subscriber, true, false).await.unwrap() {
			Recv::Datagram(datagram) => assert_eq!(&datagram.payload[..], b"last"),
			_ => panic!("expected datagram before finished"),
		}

		match recv_next(&mut subscriber, true, false).await.unwrap() {
			Recv::Finished => {}
			_ => panic!("expected finished after datagram"),
		}
	}

	#[kio::test]
	async fn recv_next_reports_future_boundary_before_finished() {
		let mut producer = track_producer("test");
		let mut subscriber = producer.subscribe(None);

		// The last group is 6 (exclusive 7), but only group 5 has been produced so far.
		producer
			.create_group(group::Info { sequence: 5 })
			.unwrap()
			.finish()
			.unwrap();
		producer.finish_at(track::Position::group(7), None).unwrap();

		// Group 5 is delivered first.
		match recv_next(&mut subscriber, false, true).await.unwrap() {
			Recv::Group(group) => assert_eq!(group.sequence, 5),
			_ => panic!("expected group 5"),
		}

		// With no more groups ready yet, the declared boundary surfaces even though the
		// track isn't finished (group 6 is still outstanding).
		match recv_next(&mut subscriber, false, true).await.unwrap() {
			Recv::Boundary(end) => assert_eq!(end.group, 7),
			_ => panic!("expected the future boundary"),
		}

		// The caller stops requesting the boundary once sent. The trailing group arrives,
		// then the track finishes.
		producer
			.create_group(group::Info { sequence: 6 })
			.unwrap()
			.finish()
			.unwrap();
		match recv_next(&mut subscriber, false, false).await.unwrap() {
			Recv::Group(group) => assert_eq!(group.sequence, 6),
			_ => panic!("expected group 6"),
		}
		match recv_next(&mut subscriber, false, false).await.unwrap() {
			Recv::Finished => {}
			_ => panic!("expected finished once the boundary is reached"),
		}
	}

	/// Without the end error on the wire, an errored end is not declared early: a clean
	/// boundary could let the subscriber settle before the reset at the end arrives.
	#[kio::test]
	async fn recv_next_withholds_an_errored_boundary_without_the_end_error() {
		use futures::FutureExt;

		for (end_error, capped) in [(false, false), (true, false), (false, true)] {
			let mut producer = track_producer("test");
			let subscriber = producer.subscribe(None);
			// A range that stops short of the end is owed no error, so its boundary goes out.
			let mut subscriber = match capped {
				true => subscriber.with_range(..3),
				false => subscriber,
			};
			producer
				.finish_at(track::Position::group(7), Some(Error::App(7)))
				.unwrap();
			let next = |subscriber: &mut track::Subscriber| {
				kio::wait(|waiter| poll_recv_next(subscriber, true, false, true, end_error, waiter)).now_or_never()
			};
			match (end_error || capped, next(&mut subscriber)) {
				(true, Some(Ok(Recv::Boundary(end)))) => {
					assert_eq!(end.group, 7);
					assert_eq!(end.error.is_some(), end_error);
				}
				(false, None) => {}
				_ => panic!("end_error={end_error} capped={capped}: unexpected boundary"),
			}
		}
	}

	/// A relay can ingest back-to-back groups micro-reordered (the upstream leg
	/// sends newest-first). The older group is cached and in demand, so serving
	/// must still deliver it; a sequence cursor would skip it permanently.
	#[kio::test]
	async fn recv_next_serves_late_arrival_after_newer_group() {
		use futures::FutureExt;

		let mut producer = track_producer("test");
		let mut subscriber = producer.subscribe(track::Subscription::default().with_max_delay(Duration::from_secs(5)));

		producer
			.create_group(group::Info { sequence: 2 })
			.unwrap()
			.finish()
			.unwrap();
		match recv_next(&mut subscriber, false, false).await.unwrap() {
			Recv::Group(group) => assert_eq!(group.sequence, 2),
			_ => panic!("expected group 2"),
		}

		// Group 1 lands after group 2 was already served.
		producer
			.create_group(group::Info { sequence: 1 })
			.unwrap()
			.finish()
			.unwrap();
		match recv_next(&mut subscriber, false, false).now_or_never() {
			Some(Ok(Recv::Group(group))) => assert_eq!(group.sequence, 1),
			Some(_) => panic!("expected the late-arriving group"),
			None => panic!("the late-arriving group was skipped"),
		}

		// Staleness is the max delay window's job, not arrival order's: the track
		// still finishes normally afterward.
		producer.finish_at(track::Position::group(3), None).unwrap();
		match recv_next(&mut subscriber, false, false).await.unwrap() {
			Recv::Finished => {}
			_ => panic!("expected finished"),
		}
	}
}

/// The announce loop: forwarding route announcements, metadata restarts, and
/// retractions onto the wire.
#[cfg(test)]
mod announce_test {
	use super::*;
	use crate::coding::{Decode, Reader};
	use crate::lite::test_transport::*;
	use crate::model::ProduceTest;
	use std::sync::Mutex;

	type TestPublisher = Publisher;

	const VERSION: Version = Version::Lite06;

	#[test]
	fn initial_replay_keeps_the_last_event_and_its_order() {
		let update = |prefix: &'static str, cost| announce::Announce {
			prefix: crate::Path::try_from(prefix).unwrap(),
			captures: None,
			route: origin::Route::default().with_cost(cost),
		};
		let events = [
			announce::Event::Start(update("a", 1)),
			announce::Event::Start(update("b", 2)),
			announce::Event::Update(update("a", 3)),
			announce::Event::Start(update("c", 4)),
			announce::Event::End(update("b", 2)),
			announce::Event::Restart(update("c", 5)),
			announce::Event::End(update("c", 5)),
			announce::Event::Start(update("b", 6)),
		];
		let initial: Vec<_> = super::super::replay::initial(events)
			.into_iter()
			.map(|update| (update.prefix, update.route.cost))
			.collect();
		assert_eq!(
			initial,
			vec![
				(crate::Path::from_static("a"), origin::Cost::new(3)),
				(crate::Path::from_static("b"), origin::Cost::new(6))
			]
		);
	}

	#[test]
	fn synced_survives_a_removed_initial_route_and_live_churn() {
		let origin = Hop::new(1).unwrap().produce();
		let removed = origin.announce("z/removed", origin::Route::default()).unwrap();
		let _initial = origin.announce("z/retained", origin::Route::default()).unwrap();
		let consumer = origin.consume();
		let mut announced = consumer.announced();
		let budget = kio::Producer::new(0usize);
		let log = Log::default();
		let mut wire = Wire {
			writes: log.writes.clone(),
			cursor: 0,
			version: Version::Lite07,
		};
		let mut stream = Stream::<Transport, Version> {
			writer: Writer::new(
				crate::transport::boxed::SendStream::new(SinkSend::stepped(log, budget.consume())),
				Version::Lite07,
			),
			reader: Reader::new(crate::transport::boxed::RecvStream::new(PendingRecv), Version::Lite07),
		};
		let mut run = AnnounceRun::new(crate::Path::from_static(""), consumer.hop(), Version::Lite07);
		let waiter = kio::Waiter::noop();
		assert!(run.poll(&mut stream, &consumer, &mut announced, &waiter).is_pending());
		drop(removed);
		let mut churn = Vec::new();
		for limit in 1..=4 {
			churn.push(origin.announce(format!("a/{limit}"), origin::Route::default()).unwrap());
			*budget.write().ok().unwrap() = limit;
			assert!(run.poll(&mut stream, &consumer, &mut announced, &waiter).is_pending());
		}
		assert_eq!(wire.take_ok().active, 0);
		let messages = wire.take_announces();
		assert!(
			matches!(&messages[0], lite::AnnounceBroadcast::Active { suffix, .. } if suffix.rest.as_str() == "z/retained")
		);
		assert_eq!(messages[2], lite::AnnounceBroadcast::Synced);
		assert!(!messages.iter().any(
			|msg| matches!(msg, lite::AnnounceBroadcast::Active { suffix, .. } if suffix.rest.as_str() == "z/removed")
		));
	}

	#[test]
	fn a_partial_regrant_cannot_overtake_an_initial_route() {
		let origin = Hop::new(1).unwrap().produce();
		let _a = origin.announce("a", origin::Route::default()).unwrap();
		let _b = origin.announce("b", origin::Route::default()).unwrap();
		let consumer = origin.consume();
		let mut announced = consumer.announced();
		let budget = kio::Producer::new(1usize);
		let log = Log::default();
		let mut wire = Wire {
			writes: log.writes.clone(),
			cursor: 0,
			version: Version::Lite07,
		};
		let mut stream = Stream::<Transport, Version> {
			writer: Writer::new(
				crate::transport::boxed::SendStream::new(SinkSend::stepped(log, budget.consume())),
				Version::Lite07,
			),
			reader: Reader::new(crate::transport::boxed::RecvStream::new(PendingRecv), Version::Lite07),
		};
		let mut run = AnnounceRun::new(crate::Path::from_static(""), consumer.hop(), Version::Lite07);
		let waiter = kio::Waiter::noop();
		assert!(run.poll(&mut stream, &consumer, &mut announced, &waiter).is_pending());
		let auth = crate::auth::Link::new(false);
		auth.handle().authorize(&crate::auth::Grant {
			subscribe: crate::Pattern::literal("b").unwrap().into(),
			..Default::default()
		});
		let Poll::Ready(restricted) = auth.poll_permit(crate::auth::Direction::Publish, &mut 0, &waiter) else {
			panic!("the changed grant is ready");
		};
		run.regrant(&mut stream, &consumer, restricted).unwrap();
		*budget.write().ok().unwrap() = 8;
		assert!(run.poll(&mut stream, &consumer, &mut announced, &waiter).is_pending());
		wire.take_ok();
		let messages = wire.take_announces();
		assert!(matches!(&messages[1], lite::AnnounceBroadcast::Active { suffix, .. } if suffix.rest.as_str() == "b"));
		assert_eq!(messages[2], lite::AnnounceBroadcast::Synced);
		assert_eq!(messages[3], lite::AnnounceBroadcast::EndedId { id: 0 });
	}

	#[test]
	fn synced_cuts_a_regrant_burst_at_the_initial_bound() {
		let origin = Hop::new(1).unwrap().produce();
		let _a = origin.announce("a", origin::Route::default()).unwrap();
		let _b = origin.announce("b", origin::Route::default()).unwrap();
		let consumer = origin.consume();
		let mut announced = consumer.announced();
		let budget = kio::Producer::new(0usize);
		let log = Log::default();
		let mut wire = Wire {
			writes: log.writes.clone(),
			cursor: 0,
			version: Version::Lite07,
		};
		let mut stream = Stream::<Transport, Version> {
			writer: Writer::new(
				crate::transport::boxed::SendStream::new(SinkSend::stepped(log, budget.consume())),
				Version::Lite07,
			),
			reader: Reader::new(crate::transport::boxed::RecvStream::new(PendingRecv), Version::Lite07),
		};
		let mut run = AnnounceRun::new(crate::Path::from_static(""), consumer.hop(), Version::Lite07);
		let waiter = kio::Waiter::noop();
		assert!(run.poll(&mut stream, &consumer, &mut announced, &waiter).is_pending());
		let auth = crate::auth::Link::new(false);
		auth.handle().authorize(&crate::auth::Grant::default());
		let Poll::Ready(restricted) = auth.poll_permit(crate::auth::Direction::Publish, &mut 0, &waiter) else {
			panic!("the changed grant is ready");
		};
		run.regrant(&mut stream, &consumer, restricted).unwrap();
		let _live = origin.announce("c", origin::Route::default()).unwrap();
		run.regrant(&mut stream, &consumer, crate::auth::Permit::default())
			.unwrap();
		*budget.write().ok().unwrap() = 4;
		assert!(run.poll(&mut stream, &consumer, &mut announced, &waiter).is_pending());
		wire.take_ok();
		let messages = wire.take_announces();
		assert_eq!(messages.len(), 4);
		let initial: std::collections::BTreeSet<_> = messages[..2]
			.iter()
			.map(|message| match message {
				lite::AnnounceBroadcast::Active { suffix, .. } => suffix.rest.as_str(),
				other => panic!("expected an initial start, got {other:?}"),
			})
			.collect();
		assert_eq!(initial, std::collections::BTreeSet::from(["a", "b"]));
		assert_eq!(messages[2], lite::AnnounceBroadcast::Synced);
		assert_eq!(
			messages
				.iter()
				.filter(|msg| matches!(msg, lite::AnnounceBroadcast::Synced))
				.count(),
			1
		);
	}

	#[test]
	fn a_seen_retraction_can_precede_synced() {
		let origin = Hop::new(1).unwrap().produce();
		let first = origin.announce("a", origin::Route::default()).unwrap();
		let unseen = origin.announce("b", origin::Route::default()).unwrap();
		let consumer = origin.consume();
		let mut announced = consumer.announced();
		let budget = kio::Producer::new(1usize);
		let log = Log::default();
		let mut wire = Wire {
			writes: log.writes.clone(),
			cursor: 0,
			version: Version::Lite07,
		};
		let mut stream = Stream::<Transport, Version> {
			writer: Writer::new(
				crate::transport::boxed::SendStream::new(SinkSend::stepped(log, budget.consume())),
				Version::Lite07,
			),
			reader: Reader::new(crate::transport::boxed::RecvStream::new(PendingRecv), Version::Lite07),
		};
		let mut run = AnnounceRun::new(crate::Path::from_static(""), consumer.hop(), Version::Lite07);
		let waiter = kio::Waiter::noop();
		assert!(run.poll(&mut stream, &consumer, &mut announced, &waiter).is_pending());
		drop(unseen);
		drop(first);
		*budget.write().ok().unwrap() = 4;
		assert!(run.poll(&mut stream, &consumer, &mut announced, &waiter).is_pending());
		wire.take_ok();
		assert!(matches!(
			wire.take_announces().as_slice(),
			[
				lite::AnnounceBroadcast::Active { .. },
				lite::AnnounceBroadcast::EndedId { id: 0 },
				lite::AnnounceBroadcast::Synced
			]
		));
		assert!(run.poll(&mut stream, &consumer, &mut announced, &waiter).is_pending());
		wire.assert_quiet();
	}

	#[test]
	fn an_empty_cold_relay_syncs_without_its_upstream() {
		let origin = Hop::new(1).unwrap().produce();
		let _upstream = origin.snapshot("");
		let consumer = origin.consume();
		let mut announced = consumer.announced();
		assert!(!announced.is_synced());
		let log = Log::default();
		let mut wire = Wire {
			writes: log.writes.clone(),
			cursor: 0,
			version: Version::Lite07,
		};
		let mut stream = Stream::<Transport, Version> {
			writer: Writer::new(
				crate::transport::boxed::SendStream::new(SinkSend::new(log)),
				Version::Lite07,
			),
			reader: Reader::new(crate::transport::boxed::RecvStream::new(PendingRecv), Version::Lite07),
		};
		let mut run = AnnounceRun::new(crate::Path::from_static(""), consumer.hop(), Version::Lite07);
		assert!(
			run.poll(&mut stream, &consumer, &mut announced, &kio::Waiter::noop())
				.is_pending()
		);
		wire.take_ok();
		assert_eq!(wire.take_announces(), vec![lite::AnnounceBroadcast::Synced]);
	}

	/// The hops stamped on every harness route.
	fn pub_hops() -> Hops {
		Hops::try_from(vec![Hop::new(9).unwrap()]).unwrap()
	}

	/// A cursor over the captured announce-stream bytes, decoding messages
	/// incrementally so each test step asserts exactly what it caused.
	struct Wire {
		writes: Arc<Mutex<Vec<u8>>>,
		cursor: usize,
		version: Version,
	}

	impl Wire {
		fn pending(&self) -> Vec<u8> {
			self.writes.lock().unwrap()[self.cursor..].to_vec()
		}

		/// Decode the AnnounceOk that opens the stream.
		fn take_ok(&mut self) -> lite::AnnounceOk {
			let buf = self.pending();
			let mut slice = &buf[..];
			let ok = crate::coding::decode_buf(&mut slice, self.version, |r, v| lite::AnnounceOk::decode(r, v))
				.expect("announce ok");
			self.cursor += buf.len() - slice.len();
			ok
		}

		/// Decode every announce message written since the last call.
		fn take_announces(&mut self) -> Vec<lite::AnnounceBroadcast> {
			let buf = self.pending();
			let mut slice = &buf[..];
			let mut msgs = Vec::new();
			while !slice.is_empty() {
				msgs.push(
					crate::coding::decode_buf(&mut slice, self.version, |r, v| lite::AnnounceBroadcast::decode(r, v))
						.expect("announce message"),
				);
			}
			self.cursor += buf.len();
			msgs
		}

		/// Assert nothing hit the wire since the last decode.
		fn assert_quiet(&self) {
			let pending = self.pending();
			assert!(pending.is_empty(), "unexpected wire bytes: {pending:?}");
		}
	}

	struct Harness {
		/// Held for the whole test: dropping the origin producer retracts every
		/// route under it, which would end the announce loop.
		origin: origin::Producer,
		/// The initial announcement; drop to retract, update to restart.
		announcement: crate::model::AnnounceProducer,
		wire: Wire,
		task: kio::clock::sim::JoinHandle<Result<(), Error>>,
	}

	impl Harness {
		/// Assert the loop is quiet *and* still alive. A panicked announce task
		/// also writes nothing, so silence alone would pass for the wrong reason
		/// (`kio::clock::sim::spawn` parks the panic in the handle until it's joined).
		fn assert_idle(&self) {
			self.wire.assert_quiet();
			assert!(!self.task.is_finished(), "the announce loop ended unexpectedly");
		}
	}

	async fn settle() {
		kio::clock::sleep(Duration::from_millis(1)).await;
	}

	/// Announce one route with cost 7 and run the announce loop against it.
	async fn harness() -> Harness {
		harness_on(VERSION).await
	}

	/// [`harness`] on another version.
	async fn harness_on(version: Version) -> Harness {
		harness_with(version, None).await
	}

	/// A relay answers with what it holds now: an upstream still delivering its own
	/// initial set never delays the ANNOUNCE_OK, so a peer syncs with this relay.
	#[kio::test]
	async fn a_silent_upstream_never_delays_the_answer() {
		let origin = Hop::new(1).unwrap().produce();
		let _route = origin.announce("cam", crate::origin::Route::default()).unwrap();
		// An upstream that never answers its announce request.
		let _silent = origin.clone().with_link(crate::origin::Link::Upstream).snapshot("");

		let log = Log::default();
		let writes = log.writes.clone();
		let consumer = origin.consume();
		assert!(!consumer.announced().is_synced(), "the relay itself is still syncing");
		let mut stream = Stream::<Transport, Version> {
			writer: Writer::new(crate::transport::boxed::SendStream::new(SinkSend::new(log)), VERSION),
			reader: Reader::new(crate::transport::boxed::RecvStream::new(PendingRecv), VERSION),
		};
		let _task = kio::clock::sim::spawn(async move {
			let mut announced = consumer.announced();
			let self_origin = consumer.hop();
			TestPublisher::run_announce(&mut stream, &consumer, &mut announced, self_origin, VERSION, None).await
		});
		settle().await;

		let mut wire = Wire {
			writes,
			cursor: 0,
			version: VERSION,
		};
		assert_eq!(wire.take_ok().active, 1, "answered with the current view");
		assert_eq!(wire.take_announces().len(), 1);
	}

	/// [`harness_on`], announcing under `auth`'s grant.
	async fn harness_with(version: Version, auth: Option<crate::auth::Link>) -> Harness {
		let origin = Hop::new(1).unwrap().produce();
		let announcement = origin
			.announce(
				"cam",
				crate::origin::Route::default().with_hops(pub_hops()).with_cost(7),
			)
			.unwrap();

		let log = Log::default();
		let writes = log.writes.clone();
		let consumer = origin.consume();
		let mut stream = Stream::<Transport, Version> {
			writer: Writer::new(crate::transport::boxed::SendStream::new(SinkSend::new(log)), version),
			reader: Reader::new(crate::transport::boxed::RecvStream::new(PendingRecv), version),
		};
		let task = kio::clock::sim::spawn(async move {
			let mut announced = consumer.announced();
			let self_origin = consumer.hop();
			TestPublisher::run_announce(&mut stream, &consumer, &mut announced, self_origin, version, auth).await
		});
		settle().await;

		let mut wire = Wire {
			writes,
			cursor: 0,
			version,
		};
		assert_eq!(
			wire.take_ok().active,
			if version.has_synced() { 0 } else { 1 },
			"initial count on old versions"
		);
		let mut initial = wire.take_announces();
		if version.has_synced() {
			assert_eq!(initial.pop(), Some(lite::AnnounceBroadcast::Synced));
		}
		match initial.as_slice() {
			[lite::AnnounceBroadcast::Active { suffix, hops, cost, .. }] => {
				assert_eq!(suffix.rest.as_str(), "cam");
				assert_eq!(hops, &lite::HopsRef::literal(pub_hops()));
				assert_eq!(*cost, crate::origin::Cost::new(7));
			}
			other => panic!("expected the initial announce, got {other:?}"),
		}

		Harness {
			origin,
			announcement,
			wire,
			task,
		}
	}

	/// A live announce goes out as an Active with a fresh id; its retraction
	/// references the id.
	#[kio::test]
	async fn announce_and_retract() {
		let mut h = harness().await;

		let late = h
			.origin
			.announce("mic", crate::origin::Route::default().with_hops(pub_hops()))
			.unwrap();
		settle().await;
		match h.wire.take_announces().as_slice() {
			[lite::AnnounceBroadcast::Active { suffix, .. }] => assert_eq!(suffix.rest.as_str(), "mic"),
			other => panic!("expected an announce, got {other:?}"),
		}

		drop(late);
		settle().await;
		match h.wire.take_announces().as_slice() {
			// "cam" took id 0 in the initial burst, so "mic" is id 1.
			[lite::AnnounceBroadcast::EndedId { id: 1 }] => {}
			other => panic!("expected the retraction, got {other:?}"),
		}
		h.assert_idle();
	}

	/// A metadata update restarts the advertisement in place, keeping its id.
	#[kio::test]
	async fn update_restarts_in_place() {
		let mut h = harness().await;

		h.announcement
			.update(crate::origin::Route::default().with_hops(pub_hops()).with_cost(3))
			.unwrap();
		kio::clock::sleep(crate::origin::Config::default().update_hold).await;
		settle().await;
		match h.wire.take_announces().as_slice() {
			[lite::AnnounceBroadcast::Update { id: 0, hops, cost }] => {
				assert_eq!(hops, &lite::HopsRef::literal(pub_hops()));
				assert_eq!(*cost, crate::origin::Cost::new(3));
			}
			other => panic!("expected a restart, got {other:?}"),
		}
		h.assert_idle();
	}

	/// An identical update never reaches the wire: the origin coalesces it away.
	#[kio::test]
	async fn identical_update_is_quiet() {
		let h = harness().await;
		h.announcement
			.update(crate::origin::Route::default().with_hops(pub_hops()).with_cost(7))
			.unwrap();
		settle().await;
		h.assert_idle();
	}

	/// A new best route the wire cannot tell apart (another session, same chain and
	/// cost) and without an epoch is another source, in either direction: an
	/// ANNOUNCE_RESTART on lite-07, which may carry an epoch, and an end and a fresh
	/// start before it.
	#[kio::test]
	async fn source_flip_restarts() {
		for version in [Version::Lite06, Version::Lite07] {
			let mut h = harness_on(version).await;
			let route = crate::origin::Route::default().with_hops(pub_hops()).with_cost(7);
			let newer = h.origin.announce("cam", route.clone()).unwrap();
			// A restart waits out the update hold like an update.
			kio::clock::sleep(crate::origin::Config::default().update_hold).await;
			settle().await;
			let restarted = |msgs: Vec<lite::AnnounceBroadcast>| match (version, msgs.as_slice()) {
				(
					Version::Lite07,
					[
						lite::AnnounceBroadcast::Restart {
							id: 0,
							epoch: None,
							cost,
							..
						},
					],
				) => {
					assert_eq!(*cost, crate::origin::Cost::new(7))
				}
				(
					Version::Lite06,
					[
						lite::AnnounceBroadcast::EndedId { .. },
						lite::AnnounceBroadcast::Active { suffix, .. },
					],
				) => {
					assert_eq!(suffix.rest.as_str(), "cam")
				}
				(_, other) => panic!("expected a restart on {version}, got {other:?}"),
			};
			restarted(h.wire.take_announces());

			drop(newer);
			kio::clock::sleep(crate::origin::Config::default().update_hold).await;
			settle().await;
			restarted(h.wire.take_announces());
			h.assert_idle();
		}
	}

	/// A newer epoch restarts the advertisement, carrying the epoch on lite-07.
	#[kio::test]
	async fn a_newer_epoch_restarts_with_it() {
		let mut h = harness_on(Version::Lite07).await;
		let epoch = crate::Epoch::mint();
		let _newer = h
			.origin
			.announce(
				"cam",
				crate::origin::Route::default()
					.with_epoch(epoch.clone())
					.with_hops(pub_hops())
					.with_cost(7),
			)
			.unwrap();
		kio::clock::sleep(crate::origin::Config::default().update_hold).await;
		settle().await;
		match h.wire.take_announces().as_slice() {
			[
				lite::AnnounceBroadcast::Restart {
					id: 0,
					epoch: Some(sent),
					..
				},
			] => assert_eq!(*sent, epoch),
			other => panic!("expected a restart, got {other:?}"),
		}
		h.assert_idle();
	}

	/// A grant that grows while a new instance replaces an advertised one still restarts it,
	/// whether the instance carries a newer epoch or is an anonymous source: the regrant takes
	/// in what the old cursor had pending rather than dropping it.
	#[kio::test]
	async fn a_restart_pending_when_the_grant_grows_still_restarts() {
		// The peer's ceiling: what it may subscribe to is what we may announce to it.
		let grant = |paths: &[&str]| crate::auth::Grant {
			publish: Default::default(),
			subscribe: paths
				.iter()
				.map(|path| crate::Pattern::subtree(path).unwrap())
				.collect(),
			expires: None,
		};
		for anonymous in [false, true] {
			let auth = crate::auth::Link::new(false);
			auth.handle().authorize(&grant(&["cam"]));
			let mut h = harness_with(Version::Lite07, Some(auth.clone())).await;

			// Same chain and cost, so only the instance changed. Both land before the loop polls.
			let route = crate::origin::Route::default().with_hops(pub_hops()).with_cost(7);
			let epoch = (!anonymous).then(crate::Epoch::mint);
			let route = match &epoch {
				Some(epoch) => route.with_epoch(epoch.clone()),
				None => route,
			};
			let _newer = h.origin.announce("cam", route).unwrap();
			auth.handle().authorize(&grant(&["cam", "other"]));
			kio::clock::sleep(crate::origin::Config::default().update_hold).await;
			settle().await;
			match h.wire.take_announces().as_slice() {
				[lite::AnnounceBroadcast::Restart { id: 0, epoch: sent, .. }] => {
					assert_eq!(*sent, epoch, "anonymous={anonymous}")
				}
				other => panic!("expected a restart (anonymous={anonymous}), got {other:?}"),
			}
			h.assert_idle();
		}
	}

	/// Costs past the wire ceiling clamp to the same value, so moving between them
	/// sends nothing.
	#[kio::test]
	async fn clamped_cost_change_is_quiet() {
		let mut h = harness().await;
		let route = |cost| crate::origin::Route::default().with_hops(pub_hops()).with_cost(cost);
		h.announcement.update(route(u64::MAX)).unwrap();
		kio::clock::sleep(crate::origin::Config::default().update_hold).await;
		settle().await;
		assert_eq!(h.wire.take_announces().len(), 1, "expected the clamped restart");

		h.announcement.update(route(u64::MAX - 1)).unwrap();
		kio::clock::sleep(crate::origin::Config::default().update_hold).await;
		settle().await;
		h.assert_idle();
	}

	/// A route whose chain contains the excluded peer is invisible to that peer's
	/// announce stream (control-plane split horizon via the cursor).
	#[kio::test]
	async fn excluded_routes_are_filtered() {
		let peer = Hop::new(42).unwrap();
		let origin = Hop::new(1).unwrap().produce();

		let tainted = Hops::try_from(vec![peer]).unwrap();
		let _tainted = origin
			.announce("echoed", crate::origin::Route::default().with_hops(tainted))
			.unwrap();
		let _clean = origin
			.announce("local", crate::origin::Route::default().with_hops(pub_hops()))
			.unwrap();

		let log = Log::default();
		let writes = log.writes.clone();
		let consumer = origin.consume().excluding(peer);
		let mut stream = Stream::<Transport, Version> {
			writer: Writer::new(crate::transport::boxed::SendStream::new(SinkSend::new(log)), VERSION),
			reader: Reader::new(crate::transport::boxed::RecvStream::new(PendingRecv), VERSION),
		};
		let task = kio::clock::sim::spawn(async move {
			let mut announced = consumer.announced();
			let self_origin = consumer.hop();
			TestPublisher::run_announce(&mut stream, &consumer, &mut announced, self_origin, VERSION, None).await
		});
		settle().await;

		let mut wire = Wire {
			writes,
			cursor: 0,
			version: VERSION,
		};
		assert_eq!(wire.take_ok().active, 1, "only the clean route is announced");
		match wire.take_announces().as_slice() {
			[lite::AnnounceBroadcast::Active { suffix, .. }] => assert_eq!(suffix.rest.as_str(), "local"),
			other => panic!("expected the clean announce, got {other:?}"),
		}
		task.abort();
	}

	/// A route announced after the initial burst goes out as ANNOUNCE_START.
	#[kio::test]
	async fn late_route_emits_announce_start() {
		let mut h = harness().await;
		let _late = h
			.origin
			.announce("mic", crate::origin::Route::default().with_hops(pub_hops()))
			.unwrap();
		settle().await;
		match h.wire.take_announces().as_slice() {
			[lite::AnnounceBroadcast::Active { suffix, .. }] => assert_eq!(suffix.rest.as_str(), "mic"),
			other => panic!("expected ANNOUNCE_START, got {other:?}"),
		}
		h.assert_idle();
	}

	/// A cost past the wire ceiling is clamped rather than rejected.
	#[kio::test]
	async fn cost_clamps_to_the_wire_ceiling() {
		let mut h = harness().await;
		h.announcement
			.update(
				crate::origin::Route::default()
					.with_hops(pub_hops())
					.with_cost(u64::MAX),
			)
			.unwrap();
		kio::clock::sleep(crate::origin::Config::default().update_hold).await;
		settle().await;
		match h.wire.take_announces().as_slice() {
			[lite::AnnounceBroadcast::Update { cost, .. }] => {
				assert_eq!(*cost, crate::origin::Cost::MAX);
			}
			other => panic!("expected a clamped restart, got {other:?}"),
		}
	}
}

/// Buffer the per-frame timing prefix when the track advertises a timescale:
/// `[zigzag-delta timestamp]` (the lite-05 FRAME format). With `None` the field is
/// omitted entirely, saving the bytes on tracks where timing isn't meaningful
/// (catalogs, control channels, IETF transport).
///
/// `prev_ts` carries the running baseline, so the first frame deltas against 0. The
/// model layer (`group::Producer::create_frame`) already converted a timed frame into
/// the track timescale, so its raw value goes straight onto the wire. Mirrors the
/// decode in the subscriber's `run_group`.
fn buffer_frame_info<W: crate::transport::poll::SendStream>(
	writer: &mut Writer<W, Version>,
	timestamp: Option<crate::Timestamp>,
	inflated: Option<u64>,
	size: u64,
	stamp: Option<Stamp>,
	prev_ts: &mut u64,
	runtime: &crate::time::Deadlines,
) -> Result<(), Error> {
	if let Some(stamp) = stamp {
		buffer_zigzag_delta(writer, stamp.wire(timestamp, runtime)?, prev_ts)?;
	}
	// Only a compressed group read in its stored form has one, and only on lite-07.
	if let Some(inflated) = inflated {
		writer.buffer_varint(inflated)?;
	}
	writer.buffer_varint(size)?;
	Ok(())
}

/// How a lite hop stamps a track's frames and datagrams.
#[derive(Clone, Copy, Debug)]
struct Stamp {
	/// The wire timescale TRACK_INFO declared (see [`Version::wire_timescale`]).
	timescale: crate::Timescale,
	/// Whether each frame's own timestamp goes out. Lite05/06 cannot encode an untimed
	/// track, so one served untimed carries its send time instead, unchecked ones included.
	timed: bool,
}

impl Stamp {
	/// How `version` stamps a track served at `timescale` (see
	/// [`track::Info::lite_timescale`]), or `None` when it carries no timestamps.
	fn new(version: Version, timescale: Option<crate::Timescale>) -> Option<Self> {
		Some(Self {
			timescale: version.wire_timescale(timescale)?,
			timed: timescale.is_some(),
		})
	}

	/// A frame or datagram's `timestamp` as its raw wire value, or its send time on
	/// `runtime` when the track goes out untimed.
	fn wire(self, timestamp: Option<crate::Timestamp>, runtime: &crate::time::Deadlines) -> Result<u64, Error> {
		timestamp
			.filter(|_| self.timed)
			.unwrap_or_else(|| crate::Timestamp::from(runtime.now()))
			.convert(self.timescale)
			.map(|time| time.value())
			.map_err(|_| Error::BoundsExceeded(crate::coding::BoundsExceeded))
	}
}

/// Buffer `curr` as a zigzag-mapped varint delta against `*prev`, then advance
/// `*prev` to `curr`.
fn buffer_zigzag_delta<W: crate::transport::poll::SendStream>(
	writer: &mut Writer<W, Version>,
	curr: u64,
	prev: &mut u64,
) -> Result<(), Error> {
	let delta: i64 = (curr as i128 - *prev as i128)
		.try_into()
		.map_err(|_| Error::BoundsExceeded(crate::coding::BoundsExceeded))?;
	writer.buffer_varint(crate::coding::varint::zigzag(delta))?;
	*prev = curr;
	Ok(())
}

/// What [`recv_next`] pulled from the one subscriber: the next group to serve, the next
/// best-effort datagram to forward, the track declaring its exclusive final sequence, or
/// the track finishing (the live edge having reached that boundary).
enum Recv {
	Group(group::Consumer),
	Datagram(crate::model::datagram::Stored),
	Boundary(lite::SubscribeEnd),
	Finished,
}

/// The group boundary SUBSCRIBE_END carries. Lite cannot name a frame, so a mid-group end
/// rounds up past its group, whose own stream FIN says where it stops.
fn final_sequence(end: track::Position) -> u64 {
	end.final_sequence().unwrap_or(u64::MAX)
}

/// Poll a single [`track::Subscriber`] for the next group (cap-aware, in arrival order) or
/// datagram from one `&mut` borrow, so groups and datagrams share the same subscription. Groups
/// are polled first so a datagram burst can't starve them; datagrams are polled only when the
/// transport carries them.
///
/// Groups are served in arrival order (`poll_recv_group`), not sequence order: on a relay, a
/// burst can be ingested micro-reordered by the upstream leg, and a sequence cursor would then
/// permanently skip the older group even though it is cached and in demand. Staleness is
/// governed by the max age window (cache expiry), not arrival raciness.
///
/// Without `groups` the cursor hands out none, leaving them in the cache, so it never reports
/// [`Recv::Finished`] either.
///
/// When `emit_boundary` is set, a declared-but-not-yet-reached final sequence surfaces as
/// [`Recv::Boundary`] in an idle moment (after groups and datagrams), so the caller can send
/// SUBSCRIBE_END as soon as the ending is known rather than waiting for the live edge to reach
/// it. The caller clears `emit_boundary` after the first boundary so it fires once.
///
/// When `end_error` is set, the boundary carries the error an errored end has, and reaching
/// that end finishes like a clean one. Otherwise reaching it is the error, which resets.
fn poll_recv_next(
	track: &mut track::Subscriber,
	groups: bool,
	datagrams: bool,
	emit_boundary: bool,
	end_error: bool,
	waiter: &kio::Waiter,
) -> Poll<Result<Recv, Error>> {
	{
		let mut groups_finished = false;
		let next = match groups {
			true => track.poll_recv_group(waiter),
			false => Poll::Pending,
		};
		match next {
			Poll::Ready(Ok(Some(group))) => return Poll::Ready(Ok(Recv::Group(group))),
			Poll::Ready(Ok(None)) => groups_finished = true,
			Poll::Ready(Err(_)) if end_error && track.ended_with_error() => groups_finished = true,
			Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
			Poll::Pending => {}
		}
		if datagrams {
			match track.poll_recv_stored_datagram(waiter)? {
				Poll::Ready(Some(datagram)) => return Poll::Ready(Ok(Recv::Datagram(datagram))),
				// Datagram side finished but groups are still paused/pending: keep waiting on groups.
				Poll::Ready(None) => {}
				Poll::Pending => {}
			}
		}
		// No live data ready: report the boundary (if declared) before signalling Finished, so a
		// future boundary reaches the subscriber while the trailing groups are still in flight.
		if emit_boundary && let Poll::Ready(res) = track.poll_finished(waiter) {
			let error = track.end_error();
			// Without the end error on the wire, an errored end is only the reset at that end: a
			// clean boundary sent early could let the subscriber settle before the reset arrives.
			// A range that stops short of the end is owed no error, so its boundary goes out.
			let withheld = !end_error
				&& !groups_finished
				&& error.is_some()
				&& res.as_ref().is_ok_and(|end| track.reaches(final_sequence(*end)));
			if !withheld {
				let error = error.filter(|_| end_error);
				return Poll::Ready(res.map(|end| {
					Recv::Boundary(lite::SubscribeEnd {
						group: final_sequence(end),
						error: error.as_ref().map(StreamError::from),
					})
				}));
			}
		}
		if groups_finished {
			return Poll::Ready(Ok(Recv::Finished));
		}
		Poll::Pending
	}
}

/// The async form of [`poll_recv_next`], for callers with nothing else to poll.
#[cfg(test)]
async fn recv_next(track: &mut track::Subscriber, datagrams: bool, emit_boundary: bool) -> Result<Recv, Error> {
	kio::wait(|waiter| poll_recv_next(track, true, datagrams, emit_boundary, true, waiter)).await
}

/// Bound `group` to what this subscription asked for, or `None` when it cannot be served
/// at all.
///
/// Frame bounds qualify the start and end group only; everything in between is served
/// whole. `None` means the group's head is missing and the subscriber never asked for a
/// partial group, so it must be skipped: a group is the unit of decodability, and only
/// the subscriber knows whether a partial one is any use to it.
fn position_group(group: group::Consumer, frames: &Frames) -> Option<group::Consumer> {
	let expected = frames.start(group.sequence);
	let end = frames.end(group.sequence).map_or(Bound::Unbounded, Bound::Included);

	// The start clamps up to the first frame the group still holds, so landing higher
	// than asked means the frames below it are gone.
	let group = group.with_range((Bound::Included(expected), end));
	(group.index() == expected).then_some(group)
}

/// The frame bounds of a subscription's ranges, which the model leaves to the publisher:
/// it reads whole groups.
///
/// A frame bound qualifies the group its range starts or ends at, so everything in between
/// is served whole. Ranges naming the same group serve the span covering all of their
/// frames in it; a bound still applies where another range covers its group whole, which a
/// subscriber only asks for by overlapping its own ranges.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Frames {
	/// `(group, first frame, last frame)` for each range starting or ending partway through
	/// a group: what that range serves of it, with an inclusive last frame, or `None` for
	/// the rest of the group.
	parts: Vec<(u64, u64, Option<u64>)>,
}

impl Frames {
	/// The first frame to serve of `group`: 0 unless every range naming it starts partway.
	fn start(&self, group: u64) -> u64 {
		self.parts
			.iter()
			.filter(|(part, ..)| *part == group)
			.map(|(_, first, _)| *first)
			.min()
			.unwrap_or(0)
	}

	/// The last frame to serve of `group`, inclusive, or `None` for all of it: a range
	/// naming it with no end bound there runs to its end.
	fn end(&self, group: u64) -> Option<u64> {
		let mut end = None;
		for (_, _, last) in self.parts.iter().filter(|(part, ..)| *part == group) {
			let last = (*last)?;
			end = Some(end.map_or(last, |end: u64| end.max(last)));
		}
		end
	}
}

/// A subscription's requested ranges, in the model's whole groups, with the frame bounds
/// the model leaves to [`position_group`].
#[derive(Debug)]
struct Bounds {
	ranges: Vec<crate::track::Range>,
	frames: Frames,
}

impl Bounds {
	/// Read `ranges` as they arrived, with time anchors in `timescale`, the track's as this
	/// peer was told it.
	///
	/// A time anchor on a track the peer was told is untimed is refused, since it names no
	/// group.
	fn new(ranges: &[lite::Range], timescale: Option<crate::Timescale>) -> Result<Self, Error> {
		use crate::track::{Anchor, Range};

		let anchor = |anchor: lite::Anchor| -> Result<Anchor, Error> {
			Ok(match anchor {
				lite::Anchor::Absolute(position) => Anchor::Absolute(position.group),
				lite::Anchor::Relative(back) => Anchor::Relative(back),
				lite::Anchor::Time(pts) => {
					let scale = timescale.ok_or(Error::TimestampMismatch)?;
					Anchor::Time(
						crate::Timestamp::new(pts, scale)
							.map_err(|_| Error::BoundsExceeded(crate::coding::BoundsExceeded))?,
					)
				}
			})
		};

		let mut bounds = Self {
			ranges: Vec::with_capacity(ranges.len()),
			frames: Frames::default(),
		};
		for range in ranges {
			// Empty partway through a group, which whole groups can't say: keep its place,
			// covering no group and narrowing no frame of one.
			if let (Some(lite::Anchor::Absolute(start)), Some(lite::Anchor::Absolute(end))) = (range.start, range.end)
				&& end <= start
			{
				bounds.ranges.push(Range::groups(start.group..start.group));
				continue;
			}
			let start = match range.start {
				None => Range::oldest().start,
				Some(start) => anchor(start)?,
			};
			let first = match range.start {
				Some(lite::Anchor::Absolute(start)) if start.frame != 0 => Some((start.group, start.frame)),
				_ => None,
			};
			let mut last = None;
			let end = match range.end {
				None => None,
				// A mid-group end includes its group, up to the frame before it.
				Some(lite::Anchor::Absolute(end)) => Some(Anchor::Absolute(match end.frame.checked_sub(1) {
					Some(frame) => {
						last = Some((end.group, frame));
						end.group.checked_add(1).ok_or(Error::ProtocolViolation)?
					}
					None => end.group,
				})),
				Some(end) => Some(anchor(end)?),
			};
			match (first, last) {
				(Some((group, first)), Some((end, last))) if group == end => {
					bounds.frames.parts.push((group, first, Some(last)));
				}
				_ => {
					if let Some((group, first)) = first {
						bounds.frames.parts.push((group, first, None));
					}
					if let Some((group, last)) = last {
						bounds.frames.parts.push((group, 0, Some(last)));
					}
				}
			}
			bounds.ranges.push(Range { start, end });
		}
		Ok(bounds)
	}

	/// Whether a subscription to these ranges over `version` ends once they are covered:
	/// lite-07 with every range bounded. Earlier versions read a FIN as the track's end.
	fn ends(&self, version: Version) -> bool {
		version.has_ranges() && self.ranges.iter().all(|range| range.end.is_some())
	}

	/// Whether any end is a presentation time, which needs the track's timescale.
	fn has_time(ranges: &[lite::Range]) -> bool {
		ranges.iter().any(|range| {
			matches!(range.start, Some(lite::Anchor::Time(_))) || matches!(range.end, Some(lite::Anchor::Time(_)))
		})
	}

	/// The model's preferences for these ranges.
	fn groups(&self) -> crate::track::Subscription {
		crate::track::Subscription::default().with_ranges(self.ranges.iter().copied())
	}
}

/// The part of each group's rank a SUBSCRIBE_UPDATE can move.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Steer {
	priority: i8,
	order: Order,
}

/// The subscription owns the transport handles used to open its group streams.
struct Subscription {
	session: Transport,
	id: u64,
	track_name: Arc<str>,
	priority: PriorityQueue,
	/// The publisher's priority for the track, which breaks subscriber priority ties.
	publisher: i8,
	/// The subscription's send group, ranking it against the session's others, or `None`
	/// on a transport without groups, where `priority` ranks every group stream instead.
	group: Option<GroupOf<Transport>>,
	steer: kio::Consumer<Steer>,
	/// Last steer observed by this clone, so a change only fires once.
	steer_seen: Steer,
	version: Version,
	/// How frames and datagrams are stamped, or `None` when they carry no timestamp.
	stamp: Option<Stamp>,
	/// The clock an untimed track's Lite05/06 send times are read from.
	runtime: crate::time::Deadlines,
	/// Outcomes shared by the subscription and every group it serves.
	outcomes: Arc<GroupOutcomes>,
	/// Whether the session negotiated the `ietf` extension, so streams carry relay state.
	ietf: bool,
}

/// Each group keeps its own steering cursor, without a session or send-group owner.
struct GroupContext {
	id: u64,
	steer: kio::Consumer<Steer>,
	/// Last steer observed by this clone, so a change only fires once.
	steer_seen: Steer,
	version: Version,
	/// How frames and datagrams are stamped, or `None` when they carry no timestamp.
	stamp: Option<Stamp>,
	/// The clock an untimed track's Lite05/06 send times are read from.
	runtime: crate::time::Deadlines,
	/// Outcomes shared by the subscription and every group it serves.
	outcomes: Arc<GroupOutcomes>,
	/// Whether the session negotiated the `ietf` extension, so streams carry relay state.
	ietf: bool,
}

impl GroupContext {
	/// Remember an encode failure the group stream could not carry.
	///
	/// The first one wins. The run loop resets the subscribe stream with it.
	fn note_unencodable(&self, err: &Error) {
		let mut slot = self.outcomes.unencodable.lock().unwrap();
		if slot.is_none() {
			*slot = Some(err.clone());
		}
	}
}

/// Group outcomes the subscription must report even if no header reached the peer.
#[derive(Default)]
struct GroupOutcomes {
	#[cfg(test)]
	opened: std::sync::atomic::AtomicU64,
	unencodable: Mutex<Option<Error>>,
	dropped: Mutex<Vec<lite::SubscribeDrop>>,
}

impl Subscription {
	fn group_context(&self) -> GroupContext {
		GroupContext {
			id: self.id,
			steer: self.steer.clone(),
			steer_seen: self.steer_seen,
			version: self.version,
			stamp: self.stamp,
			runtime: self.runtime.clone(),
			outcomes: self.outcomes.clone(),
			ietf: self.ietf,
		}
	}

	/// Send one datagram best-effort over a QUIC datagram (lite-05 §6.4).
	///
	/// It goes in the subscription's send group, ranked like a stream carrying its group, so
	/// it merges into the subscription's group order and wins a tie with that group's stream.
	/// The datagram is dropped (there is no group fallback) if the encoded body doesn't fit the
	/// transport's datagram limit or the send fails (congestion / no capacity right now).
	/// Returns whether it was handed to the transport.
	fn serve_datagram(&mut self, datagram: crate::model::datagram::Stored) -> bool {
		// Only the `ietf` extension can say a datagram is a status rather than a payload.
		let status = datagram
			.wire
			.as_ref()
			.is_some_and(|wire| wire.status.is_some_and(|status| status != 0));
		if status && !self.ietf {
			return false;
		}
		let Ok(timestamp) = self
			.stamp
			.filter(|_| !status)
			.map(|stamp| stamp.wire(datagram.timestamp, &self.runtime))
			.transpose()
		else {
			return false;
		};
		let ietf = match (self.ietf, &datagram.wire) {
			(false, _) => None,
			(true, None) => Some(None),
			// A sealed Timestamp keeps its units, which the receiver reads as the track's.
			(true, Some(wire))
				if wire
					.properties
					.sealed_time
					.is_some_and(|sealed| self.stamp.map(|stamp| stamp.timescale) != Some(sealed.scale())) =>
			{
				return false;
			}
			(true, Some(wire)) => Some(Some(wire.as_ref().clone())),
		};
		let sequence = datagram.sequence;
		// A lite-07 peer takes a compressed track's datagram as stored, others inflated.
		let (payload, inflated) = datagram.sent(self.version.has_compression());
		let body = lite::Datagram {
			subscribe: self.id,
			sequence,
			timestamp,
			inflated,
			payload: payload.clone(),
			ietf,
		};
		// has_datagrams is checked before this runs, and the stamp is set wherever the
		// version needs a timestamp, so encoding never hits a version guard.
		let Ok(body) = body.encode_bytes(self.version) else {
			return false;
		};

		let max = self.session.max_datagram_size().min(lite::Datagram::MAX_SIZE);
		if body.len() > max {
			tracing::debug!(sequence, size = body.len(), max, "dropping oversized datagram");
			return false;
		}

		// The run loop may not have served a group since the last SUBSCRIBE_UPDATE.
		let priority = crate::priority::in_group(sequence, 0, self.steer_current().order);
		let options = SendOptions {
			group: self.group.as_ref(),
			priority,
		};
		let _ = self.session.send_datagram(body, options);
		true
	}

	/// Read the latest SUBSCRIBE_UPDATE steer, marking it seen.
	fn steer_current(&mut self) -> Steer {
		self.steer_seen = *self.steer.read();
		self.steer_seen
	}

	/// Take the encode failure, if a group recorded one.
	fn take_unencodable(&self) -> Option<Error> {
		self.outcomes.unencodable.lock().unwrap().take()
	}

	/// Test shim: drive one group stream like the old `serve_group`, surfacing the
	/// error the machine otherwise swallows after aborting the stream.
	#[cfg(test)]
	async fn serve_group(
		mut self,
		sequence: u64,
		frame_start: u64,
		priority: PriorityHandle,
		group: group::Consumer,
	) -> Result<(), Error> {
		let mut serve = Box::new(GroupServe::new(
			self.group_context(),
			sequence,
			frame_start,
			false,
			Rank::Queue(priority),
			group,
		));
		kio::wait(|waiter| serve.poll_open(&mut self.session, self.group.as_ref(), waiter)).await?;
		kio::wait(move |waiter| serve.poll_serve(waiter)).await
	}
}

/// A subscription's run loop: one subscriber cursor serving groups, datagrams,
/// and SUBSCRIBE_UPDATE messages, with a machine per group it serves, one at a time
/// waiting for stream credit.
struct TrackRun {
	ctx: Subscription,
	track: track::Subscriber,
	// The read cursor's floor and cap, assigned together on every move.
	floor: u64,
	cap: Bound<u64>,
	/// Broadcasts SUBSCRIBE_UPDATE priorities to the in-flight group machines.
	steer_tx: kio::Producer<Steer>,
	// The frame bounds of the requested ranges, which the model leaves to `position_group`.
	frames: Frames,
	// A SUBSCRIBE_UPDATE this track cannot serve, such as a time anchor on a track the
	// peer was told is untimed, which ends the subscription.
	refused: Option<Error>,
	// Lite05+ resolves the range on the Subscribe Stream itself: SUBSCRIBE_START
	// once the first group is known, SUBSCRIBE_END as soon as the track declares its
	// exclusive final sequence (which may be ahead of the live edge).
	emit_range: bool,
	// Where SUBSCRIBE_START resolved the feed, once sent.
	start: Option<u64>,
	// The first servable group, held until the source resolves where its feed starts.
	first: Option<group::Consumer>,
	// Groups given up before the start resolved, which it must not name, with why.
	skipped: RangeMap<u64, StreamError>,
	end_sent: bool,
	// Lite-07 with every range bounded: the subscription ends once nothing in its ranges is
	// left to deliver, rather than parking for an update to widen it.
	bounded: bool,
	// Lite05+ names every sequence below the end that this subscription never
	// got with SUBSCRIBE_DROP, so the subscriber settles without waiting out its grace
	// for a group that will not come. The sequences served, sent as a datagram, or
	// dropped. A gap that never arrived stays until the end sweep names it: this side
	// cannot tell when the subscriber stopped waiting for it.
	// `None` on other versions.
	served: Option<RangeSet<u64>>,
	// Serve datagrams off this same subscriber, but only on lite-05+ over a
	// datagram-capable transport (qmux/WebSocket/TCP/UDS report size 0). No group
	// fallback: otherwise off.
	datagrams: bool,
	/// The one group waiting for stream credit. The rest of the backlog waits in the cache,
	/// which bills, ages, and evicts it, and the cursor's budget skips whatever went stale
	/// meanwhile: a machine per parked group would hold state for as long as the peer
	/// withholds credit.
	opening: Option<GroupServe>,
	/// The groups with a stream, serving or awaiting the acknowledgement of their FIN.
	children: kio::Tasks<GroupServe>,
}

impl TrackRun {
	fn new(
		ctx: Subscription,
		track: track::Subscriber,
		bounds: Bounds,
		floor: u64,
		steer_tx: kio::Producer<Steer>,
	) -> Self {
		// Apply the initial cap from the original Subscribe. Subsequent updates
		// flow through the SUBSCRIBE_UPDATE arm below.
		let cap = bounds.groups().end().map_or(Bound::Unbounded, Bound::Excluded);
		let track = track.with_range((Bound::Included(floor), cap));

		let emit_range = ctx.version.has_track_stream();
		let datagrams = ctx.version.has_datagrams() && ctx.session.max_datagram_size() > 0;
		// Lite03 and Lite04 declare no end, so neither side could settle on a drop.
		let served = emit_range.then(RangeSet::new);
		let bounded = bounds.ends(ctx.version);

		Self {
			bounded,
			frames: bounds.frames,
			refused: None,
			ctx,
			track,
			floor,
			cap,
			steer_tx,
			emit_range,
			start: None,
			first: None,
			skipped: RangeMap::new(),
			end_sent: false,
			served,
			datagrams,
			opening: None,
			children: kio::Tasks::new(),
		}
	}

	/// Group machines held: the one waiting for stream credit, and those with a stream.
	#[cfg(test)]
	fn machines(&self) -> usize {
		self.children.len() + usize::from(self.opening.is_some())
	}

	/// Apply a SUBSCRIBE_UPDATE.
	///
	/// Before lite-07 an end is a serving cap, not a subscription terminator: groups past
	/// the cap are held in the producer's cache until the subscriber raises the cap (or
	/// unsets it), then served in order. Only a peer FIN ends the subscription. This is
	/// what lets relays pause an upstream subscription across consumer churn without
	/// tearing it down. Lite-07 ends one whose every range is bounded once it is covered.
	fn update(&mut self, upd: lite::SubscribeUpdate) {
		// Every update restates the wanted group. Past our largest it is a join, which
		// repeats the pending ask; at or below it the group already exists.
		if upd
			.wanted_group
			.is_some_and(|wanted| self.track.peek_largest().is_none_or(|largest| wanted > largest.group))
		{
			self.track.join();
		}
		// The steer wakes every in-flight group, so an update that keeps it wakes none.
		let steer = Steer {
			priority: upd.priority,
			order: upd.order,
		};
		if *self.steer_tx.read() != steer {
			if let Ok(mut value) = self.steer_tx.write() {
				*value = steer;
			}
			// One move for the whole subscription; its streams keep their order within it.
			if let Some(group) = &mut self.ctx.group {
				group.set_priority(crate::priority::group(upd.priority, self.ctx.publisher));
			}
		}
		// Feed the full update into the model subscriber so the producer's
		// aggregate reflects it (and a relay re-forwards it upstream).
		// Read first: `update` replaces these preferences.
		let requested = self.track.subscription().start();
		let floored = requested.is_some_and(|group| group > 0);
		let bounds = match Bounds::new(&upd.ranges, self.ctx.stamp.map(|stamp| stamp.timescale)) {
			Ok(bounds) => bounds,
			Err(err) => {
				self.refused = Some(err);
				return;
			}
		};
		let groups = bounds.groups();
		let (start, end) = (groups.start(), groups.end());
		let _ = self.track.update(crate::track::Subscription {
			priority: upd.priority,
			max_delay: serving_max_delay(self.ctx.version, upd.max_delay),
			order: upd.order,
			..groups
		});
		// An explicit start moves the read cursor. Lite-06+ encodes an absent
		// start as no floor, so a subscription that had one above group 0 has to
		// drop back to 0: the cursor only rises on its own, and a finished group
		// below the old floor (a quiet catalog) is never served otherwise. A
		// cursor that was already at 0 stays where the first served group put it.
		// No fresh SUBSCRIBE_START follows: the subscriber clears its permanent-miss
		// floor on the same update, so both sides have to change together.
		// Pre-06 an absent start means the latest group: see the re-pin below.
		let lowered = match start {
			Some(start) => Some(start),
			None if floored && self.ctx.version.resolves_start() => Some(0),
			None => None,
		};
		// `position_cursor` pins an unfloored SUBSCRIBE to the latest group, so a pre-06
		// update that drops a named floor re-pins there too. On an empty track the next
		// group is the latest, whatever its sequence.
		let floor = match lowered {
			None if requested.is_some() && !self.ctx.version.resolves_start() => {
				Some(self.track.latest_sequence().unwrap_or(0))
			}
			floor => floor,
		};
		let range = (self.floor, self.cap);
		if let Some(start) = floor {
			self.floor = start;
			// Before SUBSCRIBE_START nothing is owed yet; the start accounts for it.
			if let Some(resolved) = self.start {
				// A lowered floor owes the groups it newly asks for, so the end sweep covers them.
				// A raised one sends no early drop: the subscriber already stops owing below its
				// floor, and a later update may lower it again to reopen those groups.
				self.start = Some(resolved.min(start));
			}
		}
		self.cap = end.map_or(Bound::Unbounded, Bound::Excluded);
		// Moving the cursor wakes its readers, so an update that keeps the range moves nothing.
		if (self.floor, self.cap) != range {
			self.move_cursor();
		}
		self.bounded = bounds.ends(self.ctx.version);
		self.frames = bounds.frames;
	}

	/// Apply [`Self::floor`] and [`Self::cap`] to the read cursor.
	fn move_cursor(&mut self) {
		self.track.set_range((Bound::Included(self.floor), self.cap));
	}

	/// Raise the read cursor's floor to `start`, keeping any higher floor already set.
	fn raise_floor(&mut self, start: u64) {
		if start > self.floor {
			self.floor = start;
			self.move_cursor();
		}
	}

	/// Serve until the live edge reaches the track's boundary.
	#[cfg(test)]
	fn poll(
		&mut self,
		writer: &mut Writer<crate::transport::boxed::SendStream, Version>,
		waiter: &kio::Waiter,
	) -> Poll<Result<(), Error>> {
		while ready!(self.poll_step(writer, waiter))?.is_continue() {}
		writer.poll_flush(waiter)
	}

	/// Serve one ready group or datagram (`Continue`), or `Break` once the live edge
	/// reaches the track's boundary or a bounded lite-07 subscription is covered. Returning per item lets the caller watch the
	/// requester while work stays ready.
	fn poll_step(
		&mut self,
		writer: &mut Writer<crate::transport::boxed::SendStream, Version>,
		waiter: &kio::Waiter,
	) -> Poll<Result<ControlFlow<()>, Error>> {
		// Deliver the buffered range messages before selecting more work.
		ready!(writer.poll_flush(waiter))?;

		// The group waiting for stream credit joins the others once it has one, or fails
		// without one.
		if let Some(opening) = &mut self.opening
			&& let Poll::Ready(res) = opening.poll_open(&mut self.ctx.session, self.ctx.group.as_ref(), waiter)
		{
			let opening = self.opening.take().expect("polled above");
			match res {
				Ok(()) => self.children.push(opening),
				Err(err) => opening.settle_failed(&err),
			}
		}
		// Drive the in-flight group machines; completions just retire.
		let _ = self.children.poll(waiter);
		// A group whose header does not fit this peer's varint wrote nothing. The
		// reset is on a stream the subscriber never tied to this subscription, so
		// forward it here or the reader waits forever.
		if let Some(err) = self.ctx.take_unencodable().or_else(|| self.refused.take()) {
			return Poll::Ready(Err(err));
		}

		let dropped = std::mem::take(&mut *self.ctx.outcomes.dropped.lock().unwrap());
		if !dropped.is_empty() {
			for dropped in dropped {
				writer.buffer(&lite::SubscribeResponse::Drop(dropped))?;
			}
			return Poll::Ready(Ok(ControlFlow::Continue(())));
		}

		if self.poll_forward_drop(writer, waiter)? {
			return Poll::Ready(Ok(ControlFlow::Continue(())));
		}

		// The first group waits for the source to resolve where its feed starts; datagrams
		// keep flowing meanwhile.
		if let Some(group) = self.first.take() {
			match self.track.poll_start(waiter) {
				Poll::Ready(source) => {
					self.start(group, source, writer)?;
					return Poll::Ready(Ok(ControlFlow::Continue(())));
				}
				Poll::Pending => {
					self.first = Some(group);
					if self.datagrams
						&& let Poll::Ready(Some(datagram)) = self.track.poll_recv_stored_datagram(waiter)?
					{
						self.serve_datagram(datagram);
						return Poll::Ready(Ok(ControlFlow::Continue(())));
					}
					return Poll::Pending;
				}
			}
		}

		// A start past everything the live feed has (a subscriber resuming just after what it
		// holds) is answered at once with the largest position, on versions that carry it:
		// a quiet track may not reach that start for a while, and the subscriber judges
		// what it holds against the answer.
		if self.emit_range
			&& self.start.is_none()
			&& self.ctx.version.has_largest()
			&& let Some(start) = self.track.subscription().start()
			&& let Poll::Ready(Some(largest)) = self.track.poll_live(waiter)
			&& (track::Position {
				group: start,
				frame: self.frames.start(start),
			}) > largest
		{
			self.start = Some(start);
			writer.buffer(&lite::SubscribeResponse::Start(lite::SubscribeStart {
				largest: Some(largest),
				starts: self.starts(start),
			}))?;
			self.raise_floor(start);
			return Poll::Ready(Ok(ControlFlow::Continue(())));
		}

		// One cursor drives the whole subscription: poll the cap-aware arrival-order
		// group and, when enabled, the next best-effort datagram. Groups are polled
		// first so a datagram burst can't starve them; datagrams flow whenever no
		// group is ready (including while groups are parked above the cap).
		let emit_boundary = self.emit_range && !self.end_sent;
		let groups = self.opening.is_none();
		let end_error = self.ctx.version.has_end_error();
		if let Poll::Ready(res) = poll_recv_next(
			&mut self.track,
			groups,
			self.datagrams,
			emit_boundary,
			end_error,
			waiter,
		) {
			match res? {
				Recv::Group(group) => {
					// Never a second stream for a sequence this subscription already accounted
					// for (served, sent as a datagram, or dropped): a group re-created after its
					// DROP stays dropped, since the subscriber may settle before it arrives.
					if self
						.served
						.as_ref()
						.is_some_and(|served| served.contains(&group.sequence))
					{
						return Poll::Ready(Ok(ControlFlow::Continue(())));
					}
					let sequence = group.sequence;
					let Some(group) = position_group(group, &self.frames) else {
						// Its head is gone, and this subscriber didn't ask for a
						// partial group. Skip it rather than open a stream that can
						// only be reset; the next servable group resolves the start.
						tracing::debug!(subscribe = self.ctx.id, track = %self.ctx.track_name, sequence, "skipping group with a missing head");
						if self.emit_range && self.start.is_none() {
							// At u64::MAX no later group can resolve a start past this one.
							if let Some(end) = sequence.checked_add(1) {
								self.skipped.insert(sequence..end, StreamError::NotFound);
							}
						} else if self.start.is_some()
							&& let Some(end) = sequence.checked_add(1)
						{
							// Lite-05+ names it now; a no-op on other versions.
							self.drop_range(sequence..end, &StreamError::NotFound, writer)?;
						}
						return Poll::Ready(Ok(ControlFlow::Continue(())));
					};
					match self.emit_range && self.start.is_none() {
						true => self.first = Some(group),
						false => self.serve(group),
					}
				}
				Recv::Datagram(datagram) => self.serve_datagram(datagram),
				Recv::Boundary(end) => {
					// The track declared its exclusive final sequence. Forward it now,
					// even if trailing groups (below it) are still in flight, then
					// keep serving them until the live edge reaches the boundary.
					self.end_sent = true;
					writer.buffer(&lite::SubscribeResponse::End(end))?;
				}
				Recv::Finished => {
					if !self.children.is_empty() {
						return Poll::Pending;
					}
					self.drop_unserved(writer)?;
					return Poll::Ready(Ok(ControlFlow::Break(())));
				}
			}
			return Poll::Ready(Ok(ControlFlow::Continue(())));
		}

		// Receiving can queue a stale skip after the poll above found nothing, without a wake.
		if self.poll_forward_drop(writer, waiter)? {
			return Poll::Ready(Ok(ControlFlow::Continue(())));
		}
		// The FIN ends the subscription, not the track, so no SUBSCRIBE_END precedes it.
		if self.covered() {
			return Poll::Ready(Ok(ControlFlow::Break(())));
		}
		Poll::Pending
	}

	/// Whether a bounded subscription has nothing left to deliver: every group its ranges
	/// name was served or dropped, and every stream it opened is done.
	fn covered(&mut self) -> bool {
		if !self.bounded || self.opening.is_some() || self.first.is_some() || !self.children.is_empty() {
			return false;
		}
		match (self.start, &self.served) {
			(Some(start), Some(served)) => self.track.covered(start, served),
			// Before SUBSCRIBE_START the subscriber owes itself nothing (see
			// `SubStream::owed`), so only a range given up whole is covered. Datagrams
			// resolve no start, and a FIN would overtake them, so a range they served
			// keeps the subscription open.
			(None, _) => {
				let skipped: RangeSet<u64> = self.skipped.iter().map(|(range, _)| range.clone()).collect();
				self.track.covered(self.floor, &skipped)
			}
			(Some(_), None) => false,
		}
	}

	/// Lite-05+ names a sequence as soon as this cursor gives it up; whether one was taken.
	/// Before SUBSCRIBE_START there is no owed range yet, so the eventual start accounts
	/// for it instead, including ranges skipped while finding the first servable group.
	fn poll_forward_drop(
		&mut self,
		writer: &mut Writer<crate::transport::boxed::SendStream, Version>,
		waiter: &kio::Waiter,
	) -> Result<bool, Error> {
		if !self.emit_range {
			return Ok(false);
		}
		let Poll::Ready((range, err)) = self.track.poll_drop(waiter) else {
			return Ok(false);
		};
		match self.start {
			None => self.skipped.insert(range, err),
			Some(_) => self.drop_range(range, &err, writer)?,
		}
		Ok(true)
	}

	/// Send a datagram, recording its sequence once it went out.
	fn serve_datagram(&mut self, datagram: crate::model::datagram::Stored) {
		// Untyped lite can name only object 0. Drop other payload objects before
		// metering them; status objects keep their existing separate drop path.
		if !self.ctx.ietf
			&& let Some(wire) = &datagram.wire
			&& wire.status.is_none_or(|status| status == 0)
			&& wire.object_id.is_some_and(|id| id > 0)
		{
			tracing::debug!(
				subscribe = self.ctx.id,
				sequence = datagram.sequence,
				object_id = wire.object_id,
				"dropping nonzero datagram object on a lite hop without ietf"
			);
			return;
		}
		if let Some(wire) = &datagram.wire
			&& crate::ietf::flate::egress_object(
				&wire.properties,
				self.track.info().ietf(),
				self.ctx.version.has_compression(),
			)
			.is_err()
		{
			return;
		}
		let sequence = datagram.sequence;
		// The model bounds whole groups, so the peer's frame bounds are applied here, against
		// the Object ID a relayed moq-transport datagram keeps (a public one is frame 0).
		let object = datagram.wire.as_ref().and_then(|wire| wire.object_id).unwrap_or(0);
		if object < self.frames.start(sequence) || self.frames.end(sequence).is_some_and(|end| object > end) {
			return;
		}
		// Metered in the form the wire carries.
		let (payload, _) = datagram.sent(self.ctx.version.has_compression());
		self.track.meter_datagram(payload.len() as u64);
		if self.ctx.serve_datagram(datagram) {
			self.mark_served(sequence);
		}
	}

	/// Record `sequence` as served, for [`Self::drop_unserved`].
	fn mark_served(&mut self, sequence: u64) {
		let Some(served) = &mut self.served else {
			return;
		};
		if let Some(next) = sequence.checked_add(1) {
			served.insert(sequence..next);
		}
	}

	/// Drop, with `err`, whatever part of `range` this subscription has not been served or
	/// dropped yet.
	fn drop_range(
		&mut self,
		range: Range<u64>,
		err: &StreamError,
		writer: &mut Writer<crate::transport::boxed::SendStream, Version>,
	) -> Result<(), Error> {
		let Some(served) = &mut self.served else {
			return Ok(());
		};
		if range.is_empty() {
			return Ok(());
		}
		let gaps: Vec<_> = served.gaps(&range).collect();
		for gap in gaps {
			served.insert(gap.clone());
			let drop = lite::SubscribeDrop::new(gap.start..=gap.end - 1, err, self.ctx.version);
			writer.buffer(&lite::SubscribeResponse::Drop(drop))?;
		}
		Ok(())
	}

	/// Name every sequence from the start to the end that this subscription never got. The
	/// track has ended, so none of them will be served now: groups it never produced (a
	/// skipped sequence), and groups the cursor passed over (stale, or missing their head).
	fn drop_unserved(
		&mut self,
		writer: &mut Writer<crate::transport::boxed::SendStream, Version>,
	) -> Result<(), Error> {
		let Some(start) = self.start else {
			// Without a SUBSCRIBE_START the subscriber owes itself no group (see
			// `SubStream::owed`) and settles on the FIN alone, so there is nothing to drop.
			return Ok(());
		};
		let Poll::Ready(Ok(fin)) = self.track.poll_finished(&kio::Waiter::noop()) else {
			// A track closed without a final sequence sent no SUBSCRIBE_END, so the subscriber
			// owes itself no range either.
			return Ok(());
		};
		// A cap below the end bounds what the subscriber is owed, as on its side.
		let fin = final_sequence(fin);
		let end = self.track.subscription().end().map_or(fin, |end| end.min(fin));
		self.drop_range(start..end, &StreamError::NotFound, writer)
	}
}

impl TrackRun {
	/// Where each requested range starts once delivery starts at `start`. Before lite-07
	/// there is one range, which starts there.
	fn starts(&mut self, start: u64) -> Vec<Option<u64>> {
		match self.ctx.version.has_ranges() {
			true => self.track.starts(start),
			false => vec![Some(start)],
		}
	}

	/// Send SUBSCRIBE_START for the first servable group, then serve it.
	///
	/// A relay caches groups in upstream arrival order, and a newer group's stream can
	/// beat an older one, so the first group here need not be the oldest the source
	/// serves. `source` is where the source's feed starts, raised to this cursor's floor,
	/// so the resolved start is the lower of the two: resolving from the later group would
	/// drop the older one for good.
	fn start(
		&mut self,
		group: group::Consumer,
		source: Option<u64>,
		writer: &mut Writer<crate::transport::boxed::SendStream, Version>,
	) -> Result<(), Error> {
		let mut start = source.map_or(group.sequence, |source| source.min(group.sequence));
		// A group given up cannot be where the feed starts. This stops at the held group
		// at the latest, since it was not skipped.
		// Adjacent runs given up with different errors stay separate, so walk them all.
		while start < group.sequence
			&& let Some((skipped, _)) = self.skipped.get_key_value(&start)
		{
			start = skipped.end.min(group.sequence);
		}
		let skipped = std::mem::take(&mut self.skipped);
		self.start = Some(start);
		// Only groups: the subscriber derives the start frame from its own request
		// (see `lite::SubscribeStart`). The track is live by now, since its first group
		// was readable. A relay can hold just the first group of a backlog its source
		// already answered past, so the source's largest says where the live feed is.
		let live = match self.track.poll_live(&kio::Waiter::noop()) {
			Poll::Ready(largest) => largest,
			Poll::Pending => None,
		};
		let largest = live.max(self.track.peek_largest());
		writer.buffer(&lite::SubscribeResponse::Start(lite::SubscribeStart {
			largest,
			starts: self.starts(start),
		}))?;
		// The subscriber now owes itself everything from the start, so a range given up
		// above it is named as it would have been after the start.
		for (range, err) in skipped.iter() {
			self.drop_range(range.start.max(start)..range.end, err, writer)?;
		}
		// SUBSCRIBE_START names where delivery starts. Groups already skipped between
		// the requested floor and this group are not served. A later group at or above an
		// explicit floor, still inside the subscriber's max age, is delivered, so the cursor
		// stays at that floor.
		//
		// A pre-06 subscription that named no group is the exception: those drafts define an
		// absent Group Start as the latest group, and the resolved start becomes the
		// floor. Lite-06 encodes a floor of group 0 as 0, which decodes as no named floor;
		// that 0 is group 0, so it is not pinned. Raised, not assigned: an update that
		// landed while the group was held may already have raised it past.
		if self.track.subscription().start().is_none() && !self.ctx.version.resolves_start() {
			self.raise_floor(start);
		}
		self.serve(group);
		Ok(())
	}

	/// Open a group machine for `group`, which waits for stream credit before the cursor
	/// hands out another.
	fn serve(&mut self, group: group::Consumer) {
		let sequence = group.sequence;
		let frame_start = group.index();
		// The stream stops at the end frame, before the group may end.
		let capped = self.frames.end(sequence).is_some();
		self.mark_served(sequence);
		tracing::debug!(subscribe = self.ctx.id, track = %self.ctx.track_name, sequence, "serving group");

		// Use the latest steer for new groups so SUBSCRIBE_UPDATE applies to them too.
		let steer = self.ctx.steer_current();
		let rank = match &self.ctx.group {
			Some(_) => Rank::Group,
			// The subscribe id scopes the group tie-break: one queue serves every
			// subscription on the session, and only groups of the same one may be
			// ranked against each other by sequence, in that subscription's order.
			None => Rank::Queue(
				self.ctx.priority.insert(
					Priority::new(steer.priority, self.ctx.id, sequence)
						.with_publisher(self.ctx.publisher)
						.with_order(steer.order),
				),
			),
		};
		debug_assert!(self.opening.is_none(), "one group waits for stream credit at a time");
		self.opening = Some(GroupServe::new(
			self.ctx.group_context(),
			sequence,
			frame_start,
			capped,
			rank,
			group,
		));
	}
}

/// Where a group stream is ranked.
enum Rank {
	/// In the subscription's send group, by its sequence in the subscription's order.
	Group,
	/// In the session-wide queue, on a transport without send groups.
	Queue(PriorityHandle),
}

/// Why to stop writing `group` while the transport parks it: the subscription's budget gave
/// it up, or the frames this cursor has yet to read are gone with no route to continue them.
/// A clean finish is no reason.
///
/// Waking on every frame written to a parked group costs what a reader parked at the live
/// edge already pays: each of its pending reads judges the budget too.
fn poll_given_up(group: &mut group::Consumer, waiter: &kio::Waiter) -> Option<Error> {
	if group.poll_expired(waiter) {
		return Some(Error::Old);
	}
	match group.poll_finished(waiter) {
		Poll::Ready(Err(err)) => Some(err),
		_ => None,
	}
}

/// Serves one group on its own unidirectional stream: the header, then every
/// frame, applying queue and SUBSCRIBE_UPDATE priority changes as they land.
struct GroupServe {
	ctx: GroupContext,
	rank: Rank,
	group: group::Consumer,
	sequence: u64,
	frame_start: u64,
	/// The stream stops at a subscription's end frame, so its FIN cannot end the group.
	capped: bool,
	// Lite05+ delta-encodes per-frame timestamps within the group. The first
	// frame's delta is absolute (against an implicit prev value of 0), every
	// subsequent delta is signed against the previous frame.
	prev_ts: u64,
	state: GroupState,
	// A long cached group drains a slice per poll, so its siblings still run.
	budget: kio::coop::Budget,
}

#[cfg_attr(
	target_pointer_width = "64",
	expect(
		clippy::large_enum_variant,
		reason = "a state machine's enum is its storage: one transient instance per stream, so the big variant is the working state, not padding held in bulk"
	)
)]
enum GroupState {
	/// Waiting for stream credit on the subscription's session handle.
	Open,
	/// Streaming frames: the write buffer drains first, then the pending chunk,
	/// then the pending frame, then the next frame.
	Serve {
		writer: Writer<crate::transport::boxed::SendStream, Version>,
		frame: Option<frame::Consumer>,
		chunk: Option<crate::frame::Payload>,
		batch: Box<Objects>,
		batch_pos: usize,
		/// The `ietf` extension's object records, when negotiated.
		records: Option<lite::ietf::Records>,
	},
	/// Every frame is written and the FIN sent: wait for the acknowledgement so a
	/// late cancel or expiry can still reset the stream.
	Closed {
		writer: Writer<crate::transport::boxed::SendStream, Version>,
	},
	Done,
}

impl GroupServe {
	fn new(
		ctx: GroupContext,
		sequence: u64,
		frame_start: u64,
		capped: bool,
		rank: Rank,
		group: group::Consumer,
	) -> Self {
		// A lite-07 peer receives a compressed group as stored, others inflated.
		let group = match ctx.version.has_compression() {
			true => group.compressed(),
			false => group,
		};
		Self {
			ctx,
			rank,
			group,
			sequence,
			frame_start,
			capped,
			prev_ts: 0,
			state: GroupState::Open,
			budget: kio::coop::Budget::new(32),
		}
	}

	/// Count opened streams for the scheduling tests.
	fn settle_open(&mut self, _opened: bool) {
		#[cfg(test)]
		if _opened {
			self.ctx.outcomes.opened.fetch_add(1, Ordering::Relaxed);
		}
	}

	/// Wait for stream credit, then write the group header, leaving the machine serving.
	/// Gives up once the group can no longer be sent, so a parked group never spends the
	/// credit it was waiting for on a reset.
	fn poll_open(
		&mut self,
		session: &mut Transport,
		group: Option<&GroupOf<Transport>>,
		waiter: &kio::Waiter,
	) -> Poll<Result<(), Error>> {
		if !matches!(self.state, GroupState::Open) {
			return Poll::Ready(Ok(()));
		}
		if let Some(err) = poll_given_up(&mut self.group, waiter) {
			self.settle_open(false);
			self.state = GroupState::Done;
			return Poll::Ready(Err(err));
		}
		let options = match &mut self.rank {
			Rank::Group => SendOptions {
				group,
				priority: crate::priority::in_group(self.sequence, self.frame_start, self.ctx.steer_seen.order),
			},
			Rank::Queue(handle) => SendOptions {
				group: None,
				priority: handle.send_order().into(),
			},
		};
		let stream = match ready!(session.poll_open_uni(waiter, options)) {
			Ok(stream) => stream,
			Err(err) => {
				self.settle_open(false);
				self.state = GroupState::Done;
				return Poll::Ready(Err(crate::lite::error::from_transport(err)));
			}
		};
		self.settle_open(true);
		let mut writer = Writer::new(stream, self.ctx.version);

		let msg = lite::Group {
			subscribe: self.ctx.id,
			sequence: self.sequence,
			frame_start: self.frame_start,
		};
		let header = writer.buffer(&lite::DataType::Group).and_then(|()| writer.buffer(&msg));
		let records = header.and_then(|()| {
			lite::ietf::Records::start(
				&mut writer,
				self.ctx.ietf,
				&self.group,
				self.capped,
				self.ctx.stamp.map(|stamp| stamp.timescale),
			)
		});
		let records = match records {
			Ok(records) => records,
			Err(err) => {
				// The header wrote nothing, so the group-stream reset has nothing to pin
				// it to this subscription. The run loop reads the slot and resets the
				// subscribe stream instead.
				self.ctx.note_unencodable(&err);
				self.state = GroupState::Done;
				writer.abort(&err);
				return Poll::Ready(Err(err));
			}
		};
		self.state = GroupState::Serve {
			writer,
			frame: None,
			chunk: None,
			batch: Box::default(),
			batch_pos: 0,
			records,
		};
		Poll::Ready(Ok(()))
	}

	/// Serve the group, aborting the stream with the real reason (Old, Lagged,
	/// Evicted, ...) on failure so the subscriber can tell a truncated group from
	/// a routine cancel. Without this the Writer's Drop fallback would report
	/// every failure as Cancel.
	fn poll_serve(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		loop {
			// Queue and SUBSCRIBE_UPDATE priority changes apply on every pass, whatever
			// the stream is blocked on, including a FIN awaiting its acknowledgement
			// (the transport still schedules its retransmissions). The rank is re-read
			// as a send order when handled, since the two conventions are inverted.
			if let GroupState::Serve { writer, .. } | GroupState::Closed { writer } = &mut self.state {
				if let Rank::Queue(handle) = &mut self.rank {
					while let Poll::Ready(rank) = handle.poll_next(waiter) {
						writer.set_priority(PriorityHandle::send_order_of(rank).into());
					}
				}
				let seen = self.ctx.steer_seen;
				// A dropped producer just disables this arm, like the queue arm above.
				if let Poll::Ready(Ok(value)) = self.ctx.steer.poll(waiter, |value| {
					if **value != seen {
						Poll::Ready(**value)
					} else {
						Poll::Pending
					}
				}) {
					self.ctx.steer_seen = value;
					// The run loop moves the send group itself; a stream only follows the order.
					let order = match &mut self.rank {
						Rank::Group => crate::priority::in_group(self.sequence, self.frame_start, value.order),
						Rank::Queue(handle) => {
							PriorityHandle::send_order_of(handle.steer(value.priority, value.order)).into()
						}
					};
					writer.set_priority(order);
				}
			}

			match &mut self.state {
				GroupState::Open => unreachable!("the subscription opens a group before scheduling it"),
				GroupState::Serve {
					writer,
					frame,
					chunk,
					batch,
					batch_pos,
					records,
				} => {
					let outcome = 'serve: {
						// The peer closing first cancels the group.
						if writer.poll_closed(waiter).is_ready() {
							break 'serve Err(Error::Cancel);
						}
						loop {
							ready!(self.budget.poll_yield(waiter));
							match writer.poll_flush(waiter) {
								Poll::Ready(Ok(())) => {}
								Poll::Ready(Err(err)) => break 'serve Err(err),
								// Parking on the transport is the one stall the group cursor cannot
								// see, and the only place a served group applies the drift budget:
								// flow control must not pin a stream that has gone stale, or one
								// whose unread rest the cache evicted.
								Poll::Pending => {
									if let Some(err) = poll_given_up(&mut self.group, waiter) {
										break 'serve Err(err);
									}
									return Poll::Pending;
								}
							}
							if let Some(pending) = chunk {
								match writer.poll_write(waiter, pending) {
									Poll::Ready(Ok(_)) => {
										if !bytes::Buf::has_remaining(pending) {
											*chunk = None;
										}
									}
									Poll::Ready(Err(err)) => break 'serve Err(err),
									// Parking on the transport is the one stall the group cursor cannot
									// see, and the only place a served group applies the drift budget:
									// flow control must not pin a stream that has gone stale, or one
									// whose unread rest the cache evicted.
									Poll::Pending => {
										if let Some(err) = poll_given_up(&mut self.group, waiter) {
											break 'serve Err(err);
										}
										return Poll::Pending;
									}
								}
							} else if let Some(pending) = frame {
								match pending.poll_read_chunk(waiter) {
									Poll::Ready(Ok(Some(next))) => *chunk = Some(next.into()),
									Poll::Ready(Ok(None)) => *frame = None,
									Poll::Ready(Err(err)) => break 'serve Err(err),
									Poll::Pending => return Poll::Pending,
								}
							} else if *batch_pos < batch.len() {
								let index = self.group.index() - batch.len() as u64 + *batch_pos as u64;
								let batched = &mut batch[*batch_pos];
								if let Some(fields) = &batched.fields
									&& let Err(err) = crate::ietf::flate::egress_object(
										&fields.properties,
										self.group.ietf(),
										self.ctx.version.has_compression(),
									) {
									break 'serve Err(err);
								}
								let buffered = records
									.as_mut()
									.map_or(Ok(()), |records| {
										records.frame(writer, index, batched.fields.as_deref())
									})
									.and_then(|()| {
										buffer_frame_info(
											writer,
											batched.frame.timestamp,
											batched.inflated,
											batched.frame.payload.len() as u64,
											self.ctx.stamp,
											&mut self.prev_ts,
											&self.ctx.runtime,
										)
									});
								if let Err(err) = buffered {
									break 'serve Err(err);
								}
								let payload = std::mem::take(&mut batched.frame.payload);
								if !payload.is_empty() {
									*chunk = Some(payload);
								}
								*batch_pos += 1;
								self.group.keep_alive();
							} else {
								match self.group.poll_read_objects(waiter, batch) {
									Poll::Ready(Ok(count)) if count > 0 => {
										*batch_pos = 0;
										continue;
									}
									// Taken, so the next pass finishes once the record flushed.
									Poll::Ready(Ok(_)) => match records.take() {
										Some(mut records) => match records.end(writer, &self.group) {
											Ok(()) => continue,
											Err(err) => break 'serve Err(err),
										},
										None => break 'serve Ok(()),
									},
									Poll::Ready(Err(err)) => break 'serve Err(err),
									Poll::Pending => {}
								}

								match self.group.poll_next_frame(waiter) {
									Poll::Ready(Ok(Some(next))) => {
										let index = self.group.index() - 1;
										if let Some(fields) = &next.fields
											&& let Err(err) = crate::ietf::flate::egress_object(
												&fields.properties,
												self.group.ietf(),
												self.ctx.version.has_compression(),
											) {
											break 'serve Err(err);
										}
										let buffered = records
											.as_mut()
											.map_or(Ok(()), |records| {
												records.frame(writer, index, next.fields.as_deref())
											})
											.and_then(|()| {
												buffer_frame_info(
													writer,
													next.timestamp,
													next.inflated,
													next.size,
													self.ctx.stamp,
													&mut self.prev_ts,
													&self.ctx.runtime,
												)
											});
										if let Err(err) = buffered {
											break 'serve Err(err);
										}
										*frame = Some(next);
									}
									Poll::Ready(Ok(None)) => match records.take() {
										Some(mut records) => match records.end(writer, &self.group) {
											Ok(()) => continue,
											Err(err) => break 'serve Err(err),
										},
										None => break 'serve Ok(()),
									},
									Poll::Ready(Err(err)) => break 'serve Err(err),
									Poll::Pending => return Poll::Pending,
								}
							}
						}
					};

					let GroupState::Serve { writer, .. } = std::mem::replace(&mut self.state, GroupState::Done) else {
						unreachable!()
					};
					match outcome {
						Ok(()) => {
							let mut writer = writer;
							// The buffer drained before the final frame resolved, so the
							// FIN follows the last byte.
							match writer.finish() {
								Ok(()) => self.state = GroupState::Closed { writer },
								// The writer drops here: the Drop reset stands in for the
								// abort, exactly like the old `finish()?`.
								Err(err) => return Poll::Ready(Err(err)),
							}
						}
						Err(err) => {
							writer.abort(&err);
							return Poll::Ready(Err(err));
						}
					}
				}
				GroupState::Closed { writer } => {
					// poll_close releases the stream on completion: the peer acknowledged
					// everything, so the Drop fallback must not reset the stream and
					// discard bytes still retransmitting.
					let res = match writer.poll_close(waiter) {
						Poll::Ready(res) => res,
						// Those bytes still hold the connection until acknowledged, so a
						// group gone stale meanwhile releases them like one still serving.
						Poll::Pending if self.group.poll_expired(waiter) => {
							let GroupState::Closed { writer } = std::mem::replace(&mut self.state, GroupState::Done)
							else {
								unreachable!()
							};
							writer.abort(&Error::Old);
							return Poll::Ready(Err(Error::Old));
						}
						Poll::Pending => return Poll::Pending,
					};
					self.state = GroupState::Done;
					return Poll::Ready(res.map(|()| {
						tracing::debug!(sequence = self.sequence, "finished group");
					}));
				}
				GroupState::Done => return Poll::Ready(Ok(())),
			}
		}
	}
}

impl kio::Task for GroupServe {
	type Output = ();

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		if let Err(err) = ready!(self.poll_serve(waiter)) {
			self.settle_failed(&err);
		}
		Poll::Ready(())
	}
}

impl GroupServe {
	/// Account for a group that failed with `err`: a reset can lose its header, and a group
	/// that never got a stream has none, so the control stream names it with that error.
	fn settle_failed(&self, err: &Error) {
		if self.ctx.version.has_track_stream() {
			let drop =
				lite::SubscribeDrop::new(self.sequence..=self.sequence, &StreamError::from(err), self.ctx.version);
			self.ctx.outcomes.dropped.lock().unwrap().push(drop);
		}
	}
}

/// A group that fails mid-stream must reset with its own error code. The subscriber uses
/// that code to tell a truncated group (Old, Lagged, Evicted) from a routine cancel, so a
/// blanket [`Error::Cancel`] from the writer's drop fallback loses the reason.
#[cfg(test)]
mod serve_group_test {
	use super::*;
	use crate::lite::test_transport::*;
	use crate::{Timestamp, broadcast};
	use futures::FutureExt;

	/// The wire's bounds map to the model's whole groups with an exclusive end: a frame
	/// bound widens to its group, which `position_group` cuts when serving.
	#[test]
	fn bounds_convert_to_whole_groups() {
		let group = |group, frame| Some(lite::Anchor::Absolute(track::Position { group, frame }));
		let bounds = |start, end| Bounds::new(&[lite::Range { start, end }], None).unwrap();

		let whole = bounds(None, group(6, 0));
		assert_eq!(whole.groups().end(), Some(6));
		assert_eq!(whole.frames, Frames::default());

		// Through frame 2 of group 5.
		let capped = bounds(None, group(5, 3));
		assert_eq!(capped.groups().end(), Some(6));
		assert_eq!(capped.frames.end(5), Some(2));

		let started = bounds(group(5, 3), None);
		assert_eq!((started.groups().start(), started.groups().end()), (Some(5), None));
		assert_eq!(started.frames.start(5), 3);

		// Empty partway through group 5 covers no group, rather than all of it.
		let empty = bounds(group(5, 3), group(5, 3));
		assert_eq!(empty.ranges, [crate::track::Range::groups(5..5)]);
		assert_eq!(empty.frames, Frames::default());

		// A time anchor names no group on a track the peer was told is untimed.
		let time = [lite::Range {
			start: Some(lite::Anchor::Time(1_000)),
			end: None,
		}];
		assert!(matches!(Bounds::new(&time, None), Err(Error::TimestampMismatch)));
		let timed = Bounds::new(&time, Some(crate::Timescale::MILLI)).unwrap();
		assert_eq!(
			timed.ranges[0].start,
			crate::track::Anchor::Time(Timestamp::from_millis(1_000).unwrap())
		);
	}

	/// Ranges naming the same group serve the span covering all of their frames in it.
	#[test]
	fn frames_widen_across_ranges() {
		let frames = Frames {
			parts: vec![
				(3, 5, None),
				(3, 2, None),
				(4, 1, Some(6)),
				(7, 0, Some(1)),
				(7, 0, Some(3)),
			],
		};
		assert_eq!((frames.start(3), frames.start(4), frames.start(5)), (2, 1, 0));
		assert_eq!(
			(frames.end(3), frames.end(4), frames.end(7), frames.end(6)),
			(None, Some(6), Some(3), None)
		);

		// Disjoint ranges meeting partway through group 5 serve all of it between them.
		let group = |group, frame| Some(lite::Anchor::Absolute(track::Position { group, frame }));
		let ranges = [
			lite::Range {
				start: group(4, 0),
				end: group(5, 2),
			},
			lite::Range {
				start: group(5, 2),
				end: group(6, 0),
			},
		];
		let abutting = Bounds::new(&ranges, None).unwrap();
		assert_eq!((abutting.frames.start(5), abutting.frames.end(5)), (0, None));

		// A range inside one group keeps both of its bounds there.
		let inside = Bounds::new(
			&[lite::Range {
				start: group(5, 2),
				end: group(5, 4),
			}],
			None,
		)
		.unwrap();
		assert_eq!((inside.frames.start(5), inside.frames.end(5)), (2, Some(3)));
	}

	/// A group whose head the publisher no longer holds is skipped, not served short:
	/// only a subscriber that asked for a partial group may receive one.
	#[test]
	fn position_group_skips_a_missing_head() {
		let track = track::Producer::new(
			Arc::new(broadcast::Info::default()),
			"video",
			crate::track::Info::timed(),
		);
		let mut group = track.create_group(group::Info { sequence: 3 }).unwrap();
		group.start_at(5).unwrap();
		group.write_frame(Timestamp::ZERO, b"tail".to_vec()).unwrap();
		let starting = |frame| Frames {
			parts: vec![(3, frame, None)],
		};

		// Asked for whole groups, so the missing frames 0..5 make this unservable.
		assert!(position_group(group.consume(), &Frames::default()).is_none());

		// Asked for exactly where it starts: servable, and positioned there.
		let consumer = position_group(group.consume(), &starting(5)).unwrap();
		assert_eq!(consumer.index(), 5);

		// Asked for an earlier frame than the group holds: still a hole, still skipped.
		assert!(position_group(group.consume(), &starting(2)).is_none());

		// The offset belongs to the start group only; a later group is served whole.
		let mut other = track.create_group(group::Info { sequence: 4 }).unwrap();
		other.write_frame(Timestamp::ZERO, b"whole".to_vec()).unwrap();
		let consumer = position_group(other.consume(), &starting(5)).unwrap();
		assert_eq!(consumer.index(), 0);
	}

	/// The end bound caps the end group and leaves the others whole.
	#[test]
	fn position_group_caps_the_end_group() {
		let track = track::Producer::new(
			Arc::new(broadcast::Info::default()),
			"video",
			crate::track::Info::timed(),
		);
		let mut group = track.create_group(group::Info { sequence: 7 }).unwrap();
		for i in 0..4u8 {
			group.write_frame(Timestamp::ZERO, vec![i]).unwrap();
		}
		group.finish().unwrap();
		let ending = |group, frame| Frames {
			parts: vec![(group, 0, Some(frame))],
		};

		let mut consumer = position_group(group.consume(), &ending(7, 1)).unwrap();
		assert_eq!(
			consumer.read_frame().now_or_never().unwrap().unwrap().unwrap().payload[0],
			0
		);
		assert_eq!(
			consumer.read_frame().now_or_never().unwrap().unwrap().unwrap().payload[0],
			1
		);
		assert!(
			consumer.read_frame().now_or_never().unwrap().unwrap().is_none(),
			"capped"
		);

		// A cap naming another group leaves this one uncapped.
		let mut consumer = position_group(group.consume(), &ending(8, 1)).unwrap();
		for i in 0..4u8 {
			assert_eq!(
				consumer.read_frame().now_or_never().unwrap().unwrap().unwrap().payload[0],
				i
			);
		}
	}

	/// A track served untimed to lite-05/06 carries its send time, never its frames' own
	/// timestamps: those of an unchecked track may be moq-transport arrival times.
	#[kio::test]
	async fn an_unchecked_track_goes_out_with_send_times() {
		let runtime = crate::time::sim();
		let stamped = Timestamp::from_millis(36_000_000).unwrap();
		let sent = Timestamp::from(runtime.now()).value();
		let checked = track::Info::timed();
		let unchecked = track::Info {
			properties: Some(std::sync::Arc::new(crate::model::object::TrackProperties {
				version: crate::ietf::Version::Draft22,
				values: Vec::new(),
			})),
			..track::Info::timed()
		};

		let stamp = Stamp::new(Version::Lite06, checked.lite_timescale(false)).unwrap();
		assert_eq!(stamp.wire(Some(stamped), &runtime).unwrap(), stamped.value());
		let stamp = Stamp::new(Version::Lite06, unchecked.lite_timescale(false)).unwrap();
		assert_eq!(stamp.wire(Some(stamped), &runtime).unwrap(), sent);
		assert!(Stamp::new(Version::Lite07, unchecked.lite_timescale(false)).is_none());
		let stamp = Stamp::new(Version::Lite07, unchecked.lite_timescale(true)).unwrap();
		assert_eq!(stamp.wire(Some(stamped), &runtime).unwrap(), stamped.value());
	}

	#[kio::test]
	async fn resets_with_the_abort_code() {
		let log = Log::default();
		let session = SinkSession::new(log.clone());

		let steer = kio::Producer::new(Steer::default());
		let subscription = Subscription {
			session: crate::driver::Transport::new(session.clone()),
			id: 0,
			track_name: "test".into(),
			priority: PriorityQueue::default(),
			publisher: 0,
			group: None,
			steer: steer.consume(),
			steer_seen: Steer::default(),
			version: Version::Lite06,
			stamp: Stamp::new(Version::Lite06, Some(crate::Timescale::MILLI)),
			runtime: crate::time::sim(),
			outcomes: Default::default(),
			ietf: false,
		};

		let track = track::Producer::new(
			Arc::new(broadcast::Info::default()),
			"test",
			crate::track::Info::timed(),
		);
		let mut group = track.create_group(group::Info { sequence: 0 }).unwrap();
		group
			.write_frame(Timestamp::from_millis(0).unwrap(), b"hello".as_slice())
			.unwrap();

		let handle = subscription.priority.insert(Priority::new(0, 0, 0));
		let mut serve = std::pin::pin!(subscription.serve_group(0, 0, handle, group.consume()));

		// Drain the frame, leaving the task parked awaiting the next one.
		assert!(futures::poll!(serve.as_mut()).is_pending());

		// The group is dropped from the cache mid-stream: a truncated group, not a cancel.
		group.abort(Error::Old).unwrap();

		assert!(matches!(serve.await, Err(Error::Old)));
		assert_eq!(log.resets(), vec![crate::StreamError::Old.to_code()]);
	}

	/// A subscription group keeps checking the max delay while a transport write is
	/// flow-control blocked, so a stalled send cannot pin the stream indefinitely.
	#[kio::test]
	async fn blocked_transport_write_expires_with_the_group() {
		let gate = kio::Producer::new(false);
		let session = SinkSession::gated_uni(gate.consume());
		let log = session.log.clone();
		let steer = kio::Producer::new(Steer::default());
		let subscription = Subscription {
			session: crate::driver::Transport::new(session.clone()),
			id: 0,
			track_name: "test".into(),
			priority: PriorityQueue::default(),
			publisher: 0,
			group: None,
			steer: steer.consume(),
			steer_seen: Steer::default(),
			version: Version::Lite06,
			stamp: Stamp::new(Version::Lite06, Some(crate::Timescale::MILLI)),
			runtime: crate::time::sim(),
			outcomes: Default::default(),
			ietf: false,
		};

		let track = track::Producer::new(
			Arc::new(broadcast::Info::default()),
			"test",
			crate::track::Info::timed(),
		);
		let mut subscriber = track.subscribe(None);
		let mut old = track.append_group().unwrap();
		old.write_frame(Timestamp::ZERO, b"old".as_slice()).unwrap();
		old.finish().unwrap();
		let group = subscriber.recv_group().await.unwrap().expect("old group");

		let handle = subscription.priority.insert(Priority::new(0, 0, 0));
		let mut serve = std::pin::pin!(subscription.serve_group(0, 0, handle, group));
		assert!(
			futures::poll!(serve.as_mut()).is_pending(),
			"transport write is blocked"
		);

		kio::clock::sim::advance(Duration::from_secs(1)).await;
		let mut edge = track.append_group().unwrap();
		edge.write_frame(Timestamp::from_millis(1000).unwrap(), b"edge".as_slice())
			.unwrap();
		edge.finish().unwrap();

		assert!(matches!(serve.await, Err(Error::Old)));
		assert_eq!(log.resets(), vec![crate::StreamError::Old.to_code()]);
	}

	/// A group parked at its tail when the budget gives up resets with `Old`. A FIN would
	/// tell the next hop the frames it already has are the whole group.
	#[kio::test]
	async fn a_group_parked_at_its_tail_expires_with_old() {
		let log = Log::default();
		let session = SinkSession::new(log.clone());
		let steer = kio::Producer::new(Steer::default());
		let subscription = Subscription {
			session: crate::driver::Transport::new(session.clone()),
			id: 0,
			track_name: "test".into(),
			priority: PriorityQueue::default(),
			publisher: 0,
			group: None,
			steer: steer.consume(),
			steer_seen: Steer::default(),
			version: Version::Lite06,
			stamp: Stamp::new(Version::Lite06, Some(crate::Timescale::MILLI)),
			runtime: crate::time::sim(),
			outcomes: Default::default(),
			ietf: false,
		};

		let track = track::Producer::new(
			Arc::new(broadcast::Info::default()),
			"test",
			crate::track::Info::timed(),
		);
		let mut subscriber = track.subscribe(None);
		let mut old = track.append_group().unwrap();
		old.write_frame(Timestamp::ZERO, b"old".as_slice()).unwrap();
		let group = subscriber.recv_group().await.unwrap().expect("old group");

		let handle = subscription.priority.insert(Priority::new(0, 0, 0));
		let mut serve = std::pin::pin!(subscription.serve_group(0, 0, handle, group));
		assert!(
			futures::poll!(serve.as_mut()).is_pending(),
			"parked waiting for the next frame"
		);

		kio::clock::sim::advance(Duration::from_secs(1)).await;
		let mut edge = track.append_group().unwrap();
		edge.write_frame(Timestamp::from_millis(1000).unwrap(), b"edge".as_slice())
			.unwrap();
		edge.finish().unwrap();

		assert!(matches!(serve.await, Err(Error::Old)));
		assert_eq!(log.resets(), vec![crate::StreamError::Old.to_code()]);
		old.abort(Error::Cancel).unwrap();
	}

	/// The final payload remains guarded after its frame has advanced the group cursor.
	#[kio::test]
	async fn blocked_final_transport_chunk_expires_with_the_group() {
		let gate = kio::Producer::new(true);
		let session = SinkSession::gated_uni(gate.consume());
		let log = session.log.clone();
		let steer = kio::Producer::new(Steer::default());
		let subscription = Subscription {
			session: crate::driver::Transport::new(session.clone()),
			id: 0,
			track_name: "test".into(),
			priority: PriorityQueue::default(),
			publisher: 0,
			group: None,
			steer: steer.consume(),
			steer_seen: Steer::default(),
			version: Version::Lite06,
			stamp: Stamp::new(Version::Lite06, Some(crate::Timescale::MILLI)),
			runtime: crate::time::sim(),
			outcomes: Default::default(),
			ietf: false,
		};

		let track = track::Producer::new(
			Arc::new(broadcast::Info::default()),
			"test",
			crate::track::Info::timed(),
		);
		let mut subscriber = track.subscribe(None);
		let old = track.append_group().unwrap();
		let mut frame = old
			.create_frame(frame::Info {
				timestamp: Some(Timestamp::ZERO),
				size: 2,
			})
			.unwrap();
		frame.write(b"a".as_slice()).unwrap();
		let group = subscriber.recv_group().await.unwrap().expect("old group");

		let handle = subscription.priority.insert(Priority::new(0, 0, 0));
		let mut serve = std::pin::pin!(subscription.serve_group(0, 0, handle, group));
		assert!(
			futures::poll!(serve.as_mut()).is_pending(),
			"waiting for the final byte"
		);

		let Ok(mut open) = gate.write() else {
			panic!("transport gate closed");
		};
		*open = false;
		drop(open);
		frame.write(b"b".as_slice()).unwrap();
		frame.finish().unwrap().finish().unwrap();
		assert!(
			futures::poll!(serve.as_mut()).is_pending(),
			"the final byte is transport-blocked"
		);

		kio::clock::sim::advance(Duration::from_secs(1)).await;
		let mut edge = track.append_group().unwrap();
		edge.write_frame(Timestamp::from_millis(1000).unwrap(), b"edge".as_slice())
			.unwrap();
		edge.finish().unwrap();

		assert!(matches!(serve.await, Err(Error::Old)));
		assert_eq!(log.resets(), vec![crate::StreamError::Old.to_code()]);
	}

	/// A subscription group keeps checking the max delay while transport stream credit is
	/// exhausted, so returning credit is reserved for content that is still live.
	#[kio::test]
	async fn blocked_transport_open_expires_with_the_group() {
		let gate = kio::Producer::new(false);
		let session = SinkSession::gated_open_uni(gate.consume());
		let steer = kio::Producer::new(Steer::default());
		let subscription = Subscription {
			session: crate::driver::Transport::new(session.clone()),
			id: 0,
			track_name: "test".into(),
			priority: PriorityQueue::default(),
			publisher: 0,
			group: None,
			steer: steer.consume(),
			steer_seen: Steer::default(),
			version: Version::Lite06,
			stamp: Stamp::new(Version::Lite06, Some(crate::Timescale::MILLI)),
			runtime: crate::time::sim(),
			outcomes: Default::default(),
			ietf: false,
		};

		let track = track::Producer::new(
			Arc::new(broadcast::Info::default()),
			"test",
			crate::track::Info::timed(),
		);
		let mut subscriber = track.subscribe(None);
		let mut old = track.append_group().unwrap();
		old.write_frame(Timestamp::ZERO, b"old".as_slice()).unwrap();
		old.finish().unwrap();
		let group = subscriber.recv_group().await.unwrap().expect("old group");

		let handle = subscription.priority.insert(Priority::new(0, 0, 0));
		let mut serve = std::pin::pin!(subscription.serve_group(0, 0, handle, group));
		assert!(
			futures::poll!(serve.as_mut()).is_pending(),
			"stream credit is exhausted"
		);

		kio::clock::sim::advance(Duration::from_secs(1)).await;
		let mut edge = track.append_group().unwrap();
		edge.write_frame(Timestamp::from_millis(1000).unwrap(), b"edge".as_slice())
			.unwrap();
		edge.finish().unwrap();

		assert!(matches!(serve.await, Err(Error::Old)));
	}

	/// Step `run` until it has nothing ready, yielding between steps so a long drain stays
	/// under the simulator's per-poll limit.
	async fn settle_run(run: &mut TrackRun, writer: &mut Writer<crate::transport::boxed::SendStream, Version>) {
		while run.poll_step(writer, &kio::Waiter::noop()).is_ready() {
			kio::clock::sim::yield_now().await;
		}
	}

	/// A peer withholding stream credit makes each subscription hold one group machine, however
	/// many tracks it subscribes to and groups they publish: the backlog waits in the cache.
	/// Granting credit then serves every group still worth sending.
	#[kio::test]
	async fn withheld_credit_holds_one_group_machine_per_subscription() {
		for (tracks, groups) in [(1, 1), (1, 64), (16, 1), (16, 64)] {
			let gate = kio::Producer::new(false);
			let session = SinkSession::gated_open_uni(gate.consume());
			let mut runs: Vec<_> = (0..tracks)
				.map(|_| {
					let track = track::Producer::new(
						Arc::new(broadcast::Info::default()),
						"test",
						crate::track::Info::timed(),
					);
					// Nothing goes stale, so only the credit bounds what is held.
					let subscriber =
						track.subscribe(track::Subscription::default().with_max_delay(Duration::from_secs(3600)));
					let (run, writer, _) = lite_run(Version::Lite07, session.clone(), subscriber);
					(track, run, writer)
				})
				.collect();

			for sequence in 0..groups {
				for (track, run, writer) in &mut runs {
					write_group(track, sequence, sequence);
					settle_run(run, writer).await;
				}
				let held: usize = runs.iter().map(|(_, run, _)| run.machines()).sum();
				assert_eq!(held, tracks, "{tracks} tracks after {} groups each", sequence + 1);
			}

			*gate.write().ok().unwrap() = true;
			for (_, run, writer) in &mut runs {
				settle_run(run, writer).await;
				assert_eq!(run.ctx.outcomes.opened.load(Ordering::Relaxed), groups);
				assert_eq!(run.machines(), 0, "every group was served");
			}
		}
	}

	/// A group waiting for stream credit that the cache evicts leaves at once, named with
	/// SUBSCRIBE_DROP, rather than spending the credit on a reset once it arrives. The next
	/// group takes its place.
	#[kio::test]
	async fn an_evicted_group_waiting_for_credit_leaves_at_once() {
		let version = Version::Lite07;
		let gate = kio::Producer::new(false);
		let mut track = track::Producer::new(
			Arc::new(broadcast::Info::default()),
			"test",
			crate::track::Info::timed(),
		);
		let subscriber = track.subscribe(track::Subscription::default().with_max_delay(Duration::from_secs(3600)));
		let (mut run, mut writer, log) = lite_run(version, SinkSession::gated_open_uni(gate.consume()), subscriber);

		let mut group = track.create_group(group::Info { sequence: 0 }).unwrap();
		group.write_frame(Timestamp::ZERO, b"x".as_slice()).unwrap();
		while run.poll_step(&mut writer, &kio::Waiter::noop()).is_ready() {}
		assert_eq!(run.machines(), 1, "group 0 waits for credit");

		group.abort(Error::Evicted).unwrap();
		while run.poll_step(&mut writer, &kio::Waiter::noop()).is_ready() {}
		assert_eq!(run.machines(), 0, "the evicted group leaves without credit");
		assert_eq!(drops(&mut writer, &log, version).await, [(0, 0)]);

		write_group(&mut track, 1, 1);
		while run.poll_step(&mut writer, &kio::Waiter::noop()).is_ready() {}
		assert_eq!(run.machines(), 1, "group 1 takes its place");

		*gate.write().ok().unwrap() = true;
		while run.poll_step(&mut writer, &kio::Waiter::noop()).is_ready() {}
		assert_eq!(
			run.ctx.outcomes.opened.load(Ordering::Relaxed),
			1,
			"the credit goes to group 1 alone"
		);
	}

	/// After a stall, the credit goes to the live edge: the group parked when credit ran out
	/// expires, and the cursor skips the backlog the budget gave up on while it waited.
	#[kio::test]
	async fn credit_after_a_stall_serves_the_live_edge() {
		let version = Version::Lite07;
		let gate = kio::Producer::new(false);
		let mut track = track::Producer::new(
			Arc::new(broadcast::Info::default()),
			"test",
			crate::track::Info::timed(),
		);
		let subscriber = track.subscribe(track::Subscription::default().with_max_delay(Duration::from_secs(1)));
		let (mut run, mut writer, log) = lite_run(version, SinkSession::gated_open_uni(gate.consume()), subscriber);
		for sequence in 0..10 {
			write_group(&mut track, sequence, sequence * 1000);
			while run.poll_step(&mut writer, &kio::Waiter::noop()).is_ready() {}
			assert!(run.machines() <= 1);
			kio::clock::sim::advance(Duration::from_secs(1)).await;
		}

		*gate.write().ok().unwrap() = true;
		while run.poll_step(&mut writer, &kio::Waiter::noop()).is_ready() {}
		assert_eq!(
			run.ctx.outcomes.opened.load(Ordering::Relaxed),
			1,
			"one stream, for the newest group"
		);
		let drops = drops(&mut writer, &log, version).await;
		assert_eq!(drops.first(), Some(&(0, 0)), "the parked group expired");
		assert!(
			drops.iter().all(|&(_, end)| end < 9),
			"the newest group is not dropped: {drops:?}"
		);
	}

	/// A stream parked on the transport resets once the cache evicts the part of its group
	/// it has not read: the rest can never be sent, so the buffered bytes are released
	/// rather than held until the peer grants flow control.
	#[kio::test]
	async fn blocked_transport_write_resets_once_the_group_is_evicted() {
		let gate = kio::Producer::new(false);
		let session = SinkSession::gated_uni(gate.consume());
		let log = session.log.clone();
		let steer = kio::Producer::new(Steer::default());
		let subscription = Subscription {
			session: crate::driver::Transport::new(session.clone()),
			id: 0,
			track_name: "test".into(),
			priority: PriorityQueue::default(),
			publisher: 0,
			group: None,
			steer: steer.consume(),
			steer_seen: Steer::default(),
			version: Version::Lite06,
			stamp: Stamp::new(Version::Lite06, Some(crate::Timescale::MILLI)),
			runtime: crate::time::sim(),
			outcomes: Default::default(),
			ietf: false,
		};

		let track = track::Producer::new(
			Arc::new(broadcast::Info::default()),
			"test",
			crate::track::Info::timed(),
		);
		let mut subscriber = track.subscribe(None);
		let mut producer = track.append_group().unwrap();
		producer.write_frame(Timestamp::ZERO, b"head".as_slice()).unwrap();
		let group = subscriber.recv_group().await.unwrap().expect("group");

		let handle = subscription.priority.insert(Priority::new(0, 0, 0));
		let mut serve = std::pin::pin!(subscription.serve_group(0, 0, handle, group));
		assert!(
			futures::poll!(serve.as_mut()).is_pending(),
			"transport write is blocked"
		);

		producer.abort(Error::Evicted).unwrap();
		assert!(matches!(
			futures::poll!(serve.as_mut()),
			Poll::Ready(Err(Error::Evicted))
		));
		assert_eq!(log.resets(), vec![crate::StreamError::Evicted.to_code()]);
	}

	/// Lite01/02 have no max delay field, so a SUBSCRIBE from one decodes as
	/// `Duration::ZERO`. Serving that as a real-time budget would hold every legacy
	/// peer to the live edge and discard backlog it never declined, so those versions
	/// get a non-dropping window and leave enforcement to the receiver.
	///
	/// These are the two most-preferred negotiated versions, so this is the common
	/// wire, not an edge case.
	#[test]
	fn a_version_without_the_field_serves_a_non_dropping_budget() {
		for version in [Version::Lite01, Version::Lite02] {
			let max_delay = serving_max_delay(version, Duration::ZERO);
			assert_eq!(max_delay, None, "{version:?} must not be served as real time");
		}

		// A version that does carry it is taken at its word, zero included.
		assert_eq!(serving_max_delay(Version::Lite05, Duration::ZERO), Some(Duration::ZERO));
		assert_eq!(
			serving_max_delay(Version::Lite05, Duration::from_secs(3)),
			Some(Duration::from_secs(3))
		);
	}

	/// A group that completes cleanly must not reset at all. The completion path
	/// releases the stream via `poll_close`; leaving the writer to drop after
	/// `finish()` would fire the Drop fallback and tack a spurious Cancel reset
	/// onto a stream the peer already acknowledged.
	#[kio::test]
	async fn completed_group_does_not_reset() {
		let log = Log::default();
		let session = SinkSession::new(log.clone());

		let steer = kio::Producer::new(Steer::default());
		let subscription = Subscription {
			session: crate::driver::Transport::new(session.clone()),
			id: 0,
			track_name: "test".into(),
			priority: PriorityQueue::default(),
			publisher: 0,
			group: None,
			steer: steer.consume(),
			steer_seen: Steer::default(),
			version: Version::Lite06,
			stamp: Stamp::new(Version::Lite06, Some(crate::Timescale::MILLI)),
			runtime: crate::time::sim(),
			outcomes: Default::default(),
			ietf: false,
		};

		let track = track::Producer::new(
			Arc::new(broadcast::Info::default()),
			"test",
			crate::track::Info::timed(),
		);
		let mut group = track.create_group(group::Info { sequence: 0 }).unwrap();
		group
			.write_frame(Timestamp::from_millis(0).unwrap(), b"hello".as_slice())
			.unwrap();
		let consumer = group.consume();
		group.finish().unwrap();

		let handle = subscription.priority.insert(Priority::new(0, 0, 0));
		subscription.serve_group(0, 0, handle, consumer).await.unwrap();

		assert_eq!(log.resets(), Vec::<u32>::new(), "clean completion must not reset");

		// The group held rank 0 (most urgent); the transport sends higher values
		// first, so every send order set on the stream must be the maximum.
		let priorities = log.priorities();
		assert!(!priorities.is_empty(), "the group stream must set a priority");
		assert!(
			priorities.iter().all(|&p| p == 255),
			"rank 0 must reach the transport as send order 255: {priorities:?}",
		);
	}

	/// A FIN holds the group's bytes until the peer acknowledges it, so a group that
	/// goes stale while waiting still expires instead of pinning them.
	#[kio::test]
	async fn unacknowledged_fin_expires_with_the_group() {
		let session = SinkSession::new(Log::default()).with_unacked_fin();
		let log = session.log.clone();
		let steer = kio::Producer::new(Steer::default());
		let subscription = Subscription {
			session: crate::driver::Transport::new(session.clone()),
			id: 0,
			track_name: "test".into(),
			priority: PriorityQueue::default(),
			publisher: 0,
			group: None,
			steer: steer.consume(),
			steer_seen: Steer::default(),
			version: Version::Lite06,
			stamp: Stamp::new(Version::Lite06, Some(crate::Timescale::MILLI)),
			runtime: crate::time::sim(),
			outcomes: Default::default(),
			ietf: false,
		};

		let track = track::Producer::new(
			Arc::new(broadcast::Info::default()),
			"test",
			crate::track::Info::timed(),
		);
		let mut subscriber = track.subscribe(None);
		let mut old = track.append_group().unwrap();
		old.write_frame(Timestamp::ZERO, b"old".as_slice()).unwrap();
		old.finish().unwrap();
		let group = subscriber.recv_group().await.unwrap().expect("old group");

		let handle = subscription.priority.insert(Priority::new(0, 0, 0));
		let mut serve = std::pin::pin!(subscription.serve_group(0, 0, handle, group));
		assert!(futures::poll!(serve.as_mut()).is_pending(), "the FIN is unacknowledged");
		assert!(log.resets().is_empty());

		kio::clock::sim::advance(Duration::from_secs(1)).await;
		let mut edge = track.append_group().unwrap();
		edge.write_frame(Timestamp::from_millis(1000).unwrap(), b"edge".as_slice())
			.unwrap();
		edge.finish().unwrap();

		assert!(matches!(futures::poll!(serve.as_mut()), Poll::Ready(Err(Error::Old))));
		assert_eq!(log.resets(), vec![crate::StreamError::Old.to_code()]);
	}

	/// The transport keeps scheduling a finished stream's retransmissions until the
	/// FIN is acknowledged, so queue and SUBSCRIBE_UPDATE reorders still reach it.
	#[kio::test]
	async fn unacknowledged_fin_follows_priority_changes() {
		let session = SinkSession::new(Log::default()).with_unacked_fin();
		let log = session.log.clone();
		let steer = kio::Producer::new(Steer::default());
		let subscription = Subscription {
			session: crate::driver::Transport::new(session.clone()),
			id: 0,
			track_name: "test".into(),
			priority: PriorityQueue::default(),
			publisher: 0,
			group: None,
			steer: steer.consume(),
			steer_seen: Steer::default(),
			version: Version::Lite06,
			stamp: Stamp::new(Version::Lite06, Some(crate::Timescale::MILLI)),
			runtime: crate::time::sim(),
			outcomes: Default::default(),
			ietf: false,
		};

		let track = track::Producer::new(
			Arc::new(broadcast::Info::default()),
			"test",
			crate::track::Info::timed(),
		);
		let mut group = track.create_group(group::Info { sequence: 0 }).unwrap();
		group.write_frame(Timestamp::ZERO, b"hello".as_slice()).unwrap();
		let consumer = group.consume();
		group.finish().unwrap();

		let handle = subscription.priority.insert(Priority::new(0, 0, 0));
		let queue = subscription.priority.clone();
		let mut serve = std::pin::pin!(subscription.serve_group(0, 0, handle, consumer));
		assert!(futures::poll!(serve.as_mut()).is_pending(), "the FIN is unacknowledged");
		assert_eq!(log.priorities().last(), Some(&255));

		// A newer group of the same subscription goes first.
		let _newer = queue.insert(Priority::new(0, 0, 1));
		assert!(futures::poll!(serve.as_mut()).is_pending());
		assert_eq!(log.priorities().last(), Some(&254));

		// A SUBSCRIBE_UPDATE raising this track reranks it above the newer group.
		steer.write().ok().expect("the group watches the priority").priority = 1;
		assert!(futures::poll!(serve.as_mut()).is_pending());
		assert_eq!(log.priorities().last(), Some(&255));
		assert!(log.resets().is_empty());
	}

	/// A lite-07 subscription's run loop, from group 0, and the log of its subscribe stream.
	fn lite07_run(
		session: SinkSession,
		track: track::Subscriber,
	) -> (TrackRun, Writer<crate::transport::boxed::SendStream, Version>, Log) {
		lite_run(Version::Lite07, session, track)
	}

	/// A subscription's run loop on `version`, from group 0, and the log of its subscribe stream.
	fn lite_run(
		version: Version,
		session: SinkSession,
		track: track::Subscriber,
	) -> (TrackRun, Writer<crate::transport::boxed::SendStream, Version>, Log) {
		let log = Log::default();
		let writer = Writer::new(
			crate::transport::boxed::SendStream::new(SinkSend::new(log.clone())),
			version,
		);
		let steer = kio::Producer::new(Steer::default());
		let ctx = Subscription {
			session: crate::driver::Transport::new(session.clone()),
			id: 0,
			track_name: "test".into(),
			priority: PriorityQueue::default(),
			publisher: 0,
			group: None,
			steer: steer.consume(),
			steer_seen: Steer::default(),
			version,
			stamp: Stamp::new(Version::Lite06, Some(crate::Timescale::MILLI)),
			runtime: crate::time::sim(),
			outcomes: Default::default(),
			ietf: false,
		};
		let bounds = Bounds::new(&[lite::Range::span(Some(0), 0, None, None)], None).unwrap();
		(TrackRun::new(ctx, track, bounds, 0, steer), writer, log)
	}

	#[kio::test]
	async fn wanted_updates_ignore_stale_groups_and_recheck_the_ratio() {
		let track = track::Producer::new(
			Arc::new(broadcast::Info::default()),
			"refresh",
			track::Info::default().with_refresh(1),
		);
		let subscriber = track.subscribe(None);
		let (mut run, _, _) = lite_run(Version::Lite07, SinkSession::new(Log::default()), subscriber);
		let mut group = track.append_group().unwrap();
		group.write_frame(None, &b"key"[..]).unwrap();
		group.write_frame(None, &b"more"[..]).unwrap();
		group.finish().unwrap();
		let update = |wanted_group| lite::SubscribeUpdate {
			wanted_group,
			priority: 0,
			max_delay: Duration::ZERO,
			ranges: vec![lite::Range::span(None, 0, None, None)],
			order: Order::Descending,
		};
		run.update(update(Some(0)));
		assert_eq!(track.demand().wanted(), Some(0), "stale request is not a join");
		run.update(update(Some(1)));
		assert_eq!(track.demand().wanted(), Some(1));
		let mut group = track.append_group().unwrap();
		group.write_frame(None, &b"key"[..]).unwrap();
		group.finish().unwrap();
		run.update(update(Some(1)));
		assert_eq!(track.demand().wanted(), Some(1), "answered request stays answered");
		run.update(update(Some(2)));
		assert_eq!(track.demand().wanted(), Some(1), "new hop judges its own ratio");
	}

	fn write_group(track: &mut track::Producer, sequence: u64, millis: u64) {
		let mut group = track.create_group(group::Info { sequence }).unwrap();
		group
			.write_frame(Timestamp::from_millis(millis).unwrap(), b"x".as_slice())
			.unwrap();
		group.finish().unwrap();
	}

	/// On a transport with send groups, a subscription opens one at its priority and its
	/// group streams join it, ranked newest first by sequence alone: the session-wide queue
	/// takes no part. A SUBSCRIBE_UPDATE moves the group once, and an order flip re-stamps
	/// the open streams.
	#[kio::test]
	async fn grouped_streams_rank_within_their_subscription() {
		let mut track = track::Producer::new(
			Arc::new(broadcast::Info::default()),
			"test",
			crate::track::Info::timed(),
		);
		// Old groups stay worth sending, so both are in flight at once.
		let subscriber = track.subscribe(track::Subscription::default().with_max_delay(Duration::from_secs(3600)));
		let session = SinkSession::new(Log::default()).with_groups().with_unacked_fin();
		let log = session.log.clone();
		let (mut run, mut writer, _) = lite_run(Version::Lite06, session.clone(), subscriber);
		run.ctx.group = crate::transport::poll::Session::send_group(&run.ctx.session, 3);

		write_group(&mut track, 0, 0);
		write_group(&mut track, 1, 1);
		{
			let mut poll = std::pin::pin!(kio::wait(|waiter| run.poll(&mut writer, waiter)));
			assert!(futures::poll!(poll.as_mut()).is_pending());
		}
		assert_eq!(log.groups(), [(0, 3)]);
		assert_eq!(
			log.joins(),
			[(0, 0), (1, 0)],
			"each stream joins the subscription's group"
		);
		assert_eq!(log.orders(), [(0, 1), (1, 3)], "newer groups first");
		assert!(log.priorities().is_empty(), "the session queue ranks no stream");

		run.update(lite::SubscribeUpdate {
			wanted_group: None,
			order: Order::Ascending,
			priority: 7,
			max_delay: Duration::ZERO,
			ranges: vec![lite::Range::span(None, 0, None, None)],
		});
		{
			let mut poll = std::pin::pin!(kio::wait(|waiter| run.poll(&mut writer, waiter)));
			assert!(futures::poll!(poll.as_mut()).is_pending());
		}
		assert_eq!(
			log.groups(),
			[(0, 3), (0, crate::priority::group(7, 0))],
			"one move for the subscription"
		);
		let mut flipped = log.orders()[2..].to_vec();
		flipped.sort();
		assert_eq!(flipped, [(0, 1), (1, -1)], "older groups first");
	}

	/// The encoded body must fit both the wire cap and the transport's smaller limit.
	#[kio::test]
	async fn datagrams_fit_the_wire_and_transport_limits() {
		for version in [Version::Lite05, Version::Lite06, Version::Lite07] {
			for transport in [900usize, 1200, 1400] {
				for size in [899usize, 900, 901, 1199, 1200, 1201] {
					let track = track::Producer::new(
						Arc::new(broadcast::Info::default()),
						"test",
						crate::track::Info::timed(),
					);
					let session = SinkSession::new(Log::default()).with_datagrams(transport);
					let (mut run, _, _) = lite_run(version, session, track.subscribe(None));
					// The subscribe ID, sequence, and timestamp each use one byte.
					let sent = run.ctx.serve_datagram(crate::model::datagram::Stored {
						datagram: crate::Datagram {
							sequence: 1,
							timestamp: Some(Timestamp::ZERO),
							payload: bytes::Bytes::from(vec![0; size - 3]),
						},
						wire: None,
						deflated: None,
					});
					assert_eq!(
						sent,
						size <= transport.min(1200),
						"{version}: body {size}, transport {transport}"
					);
				}
			}
		}
	}

	/// A datagram goes in its subscription's send group, ranked like the stream carrying
	/// its group from the start, so the transport merges it into the group order.
	#[kio::test]
	async fn datagrams_rank_within_their_subscription() {
		let mut track = track::Producer::new(
			Arc::new(broadcast::Info::default()),
			"test",
			crate::track::Info::timed(),
		);
		let subscriber = track.subscribe(track::Subscription::default().with_max_delay(Duration::from_secs(3600)));
		let session = SinkSession::new(Log::default())
			.with_groups()
			.with_datagrams(1200)
			.with_unacked_fin();
		let log = session.log.clone();
		let (mut run, mut writer, _) = lite_run(Version::Lite06, session.clone(), subscriber);
		run.ctx.group = crate::transport::poll::Session::send_group(&run.ctx.session, 3);

		write_group(&mut track, 0, 0);
		let sequence = track
			.append_datagram(Timestamp::from_millis(1).unwrap(), b"d".as_slice())
			.unwrap();
		{
			let mut poll = std::pin::pin!(kio::wait(|waiter| run.poll(&mut writer, waiter)));
			assert!(futures::poll!(poll.as_mut()).is_pending());
		}
		let order = crate::priority::in_group(sequence, 0, Order::Descending);
		assert_eq!(log.datagram_ranks(), [(Some(0), order)]);
		assert_eq!(log.orders(), [(0, 1)], "the stream for group 0 ranks below");
		assert!(order > 1);
	}

	/// With `ietf` negotiated, frame bounds cut datagrams by Object ID: a public datagram is frame 0, a
	/// relayed moq-transport one keeps its own, and both ends are inclusive.
	#[kio::test]
	async fn frame_bounds_cut_datagrams_by_object_id() {
		for (sequence, object_id, served) in [
			(5, None, false),
			(5, Some(1), false),
			(5, Some(2), true),
			(6, None, true),
			(7, Some(3), true),
			(7, Some(4), false),
		] {
			let mut track = track::Producer::new(
				Arc::new(broadcast::Info::default()),
				"test",
				crate::track::Info::timed(),
			);
			let subscriber = track.subscribe(track::Subscription::default().with_max_delay(Duration::from_secs(3600)));
			let session = SinkSession::new(Log::default()).with_groups().with_datagrams(1200);
			let log = session.log.clone();
			let (mut run, mut writer, _) = lite_run(Version::Lite07, session.clone(), subscriber);
			run.ctx.ietf = true;
			run.ctx.group = crate::transport::poll::Session::send_group(&run.ctx.session, 3);
			run.update(lite::SubscribeUpdate {
				wanted_group: None,
				order: Order::Descending,
				priority: 0,
				max_delay: Duration::from_secs(3600),
				ranges: vec![lite::Range::span(Some(5), 2, Some(7), Some(3))],
			});

			track
				.insert_wire_datagram(crate::model::datagram::Stored {
					datagram: crate::model::datagram::Datagram {
						sequence,
						timestamp: Some(Timestamp::from_millis(sequence).unwrap()),
						payload: bytes::Bytes::from_static(b"d"),
					},
					wire: object_id.map(|object_id| {
						Arc::new(crate::model::datagram::Wire {
							object_id: Some(object_id),
							priority: None,
							end_of_group: false,
							status: None,
							properties: Default::default(),
						})
					}),
					deflated: None,
				})
				.unwrap();
			{
				let mut poll = std::pin::pin!(kio::wait(|waiter| run.poll(&mut writer, waiter)));
				assert!(futures::poll!(poll.as_mut()).is_pending());
			}
			assert_eq!(
				log.datagram_ranks().len(),
				usize::from(served),
				"group {sequence} object {object_id:?}"
			);
		}
	}

	/// An object this hop cannot identify is never egress payload. An admitted object's
	/// existing best-effort accounting still counts a transport-size drop as an attempt.
	#[kio::test]
	async fn unidentified_datagram_objects_are_not_metered() {
		use crate::model::ProduceTest;

		for ietf in [false, true] {
			for objects in [&[1, 2][..], &[0, 1, 2]] {
				for max in [1, 1200] {
					let origin = crate::origin::Config::new(crate::Hop::new(2).unwrap()).produce();
					let broadcast = origin.publish("room", crate::origin::Route::default()).unwrap();
					let info = track::Info {
						properties: Some(Arc::new(crate::model::object::TrackProperties {
							version: crate::ietf::Version::Draft22,
							values: Vec::new(),
						})),
						..track::Info::timed()
					};
					let _track = broadcast.create_track("test", info).unwrap();
					let registry = crate::stats::Registry::new(Default::default());
					let tagged = origin.consume().with_stats(
						registry
							.tier(Default::default())
							.session(crate::Path::from_static("egress")),
					);
					let served = tagged.request("room", None).await.unwrap();
					let copy = served.track("test").unwrap();
					let subscriber = copy.subscribe(None).await.unwrap();
					let session = SinkSession::new(Log::default()).with_datagrams(max);
					let log = session.log.clone();
					let (mut run, _, _) = lite_run(Version::Lite07, session, subscriber);
					run.ctx.ietf = ietf;
					for &object in objects {
						run.serve_datagram(crate::model::datagram::Stored {
							datagram: crate::Datagram {
								sequence: 5,
								timestamp: Some(Timestamp::ZERO),
								payload: bytes::Bytes::from_static(b"payload"),
							},
							wire: Some(Arc::new(crate::model::datagram::Wire {
								object_id: Some(object),
								priority: None,
								end_of_group: false,
								status: None,
								properties: Default::default(),
							})),
							deflated: None,
						});
					}
					let sent = registry
						.snapshot()
						.traffic()
						.into_iter()
						.find(|(_, role, _)| *role == crate::stats::Role::Publisher)
						.map(|(_, _, traffic)| traffic)
						.unwrap_or_default();
					let admitted = objects.iter().filter(|&&id| ietf || id == 0).count() as u64;
					assert_eq!(
						(sent.datagrams, sent.groups, sent.frames, sent.bytes),
						(admitted, admitted, admitted, 7 * admitted),
						"ietf={ietf} ids={objects:?} max={max}"
					);
					assert_eq!(
						log.datagrams.lock().unwrap().len() as u64,
						if max == 1 { 0 } else { admitted }
					);
					// Status attempts have no payload and keep their separate accounting;
					// only a typed hop can transmit them.
					run.serve_datagram(crate::model::datagram::Stored {
						datagram: crate::Datagram {
							sequence: 6,
							timestamp: None,
							payload: bytes::Bytes::new(),
						},
						wire: Some(Arc::new(crate::model::datagram::Wire {
							object_id: Some(2),
							priority: None,
							end_of_group: false,
							status: Some(crate::ietf::object::END_OF_GROUP),
							properties: Default::default(),
						})),
						deflated: None,
					});
					let after_status = registry
						.snapshot()
						.traffic()
						.into_iter()
						.find(|(_, role, _)| *role == crate::stats::Role::Publisher)
						.map(|(_, _, traffic)| traffic)
						.unwrap();
					assert_eq!(after_status.datagrams, admitted + 1);
					assert_eq!(after_status.bytes, 7 * admitted);
					assert_eq!(
						log.datagrams.lock().unwrap().len() as u64,
						if max == 1 { 0 } else { admitted + u64::from(ietf) }
					);
				}
			}
		}
	}

	/// A SUBSCRIBE_UPDATE that flips the group order reranks the next datagram, even with no
	/// group served in between.
	#[kio::test]
	async fn datagrams_follow_an_updated_group_order() {
		let mut track = track::Producer::new(
			Arc::new(broadcast::Info::default()),
			"test",
			crate::track::Info::timed(),
		);
		let subscriber = track.subscribe(track::Subscription::default().with_max_delay(Duration::from_secs(3600)));
		let session = SinkSession::new(Log::default()).with_groups().with_datagrams(1200);
		let log = session.log.clone();
		let (mut run, mut writer, _) = lite_run(Version::Lite06, session.clone(), subscriber);
		run.ctx.group = crate::transport::poll::Session::send_group(&run.ctx.session, 3);

		let first = track
			.append_datagram(Timestamp::from_millis(1).unwrap(), b"a".as_slice())
			.unwrap();
		{
			let mut poll = std::pin::pin!(kio::wait(|waiter| run.poll(&mut writer, waiter)));
			assert!(futures::poll!(poll.as_mut()).is_pending());
		}

		run.update(lite::SubscribeUpdate {
			wanted_group: None,
			order: Order::Ascending,
			priority: 3,
			max_delay: Duration::from_secs(3600),
			ranges: vec![lite::Range::span(None, 0, None, None)],
		});
		let second = track
			.append_datagram(Timestamp::from_millis(2).unwrap(), b"b".as_slice())
			.unwrap();
		{
			let mut poll = std::pin::pin!(kio::wait(|waiter| run.poll(&mut writer, waiter)));
			assert!(futures::poll!(poll.as_mut()).is_pending());
		}

		let descending = crate::priority::in_group(first, 0, Order::Descending);
		let ascending = crate::priority::in_group(second, 0, Order::Ascending);
		assert_ne!(ascending, crate::priority::in_group(second, 0, Order::Descending));
		assert_eq!(log.datagram_ranks(), [(Some(0), descending), (Some(0), ascending)]);
	}

	/// A group with more frames ready than one poll's budget drains over several polls,
	/// so a sibling (the transport's driver, say) runs in between.
	#[kio::test]
	async fn a_long_group_drains_within_the_budget() {
		/// `GroupServe`'s passes per poll; each writes at most one frame.
		const BUDGET: usize = 32;
		const FRAMES: usize = 4_000;
		const PAYLOAD: usize = 100;

		type Task = Box<dyn FnMut(&kio::Waiter) -> Poll<()>>;

		let track = track::Producer::new(
			Arc::new(broadcast::Info::default()),
			"test",
			crate::track::Info::timed(),
		);
		let subscriber = track.subscribe(track::Subscription::default().with_max_delay(Duration::from_secs(3600)));
		let log = Log::default();
		let (mut run, mut writer, _) = lite07_run(SinkSession::new(log.clone()).with_unacked_fin(), subscriber);

		let mut group = track.create_group(group::Info { sequence: 0 }).unwrap();
		for millis in 0..FRAMES as u64 {
			group
				.write_frame(Timestamp::from_millis(millis).unwrap(), vec![0u8; PAYLOAD])
				.unwrap();
		}
		group.finish().unwrap();

		let mut tasks: kio::Tasks<Task> = kio::Tasks::new();
		let turns = Arc::new(AtomicU64::new(0));
		let counted = turns.clone();
		tasks.push(Box::new(move |waiter: &kio::Waiter| {
			counted.fetch_add(1, Ordering::Relaxed);
			waiter.waker().wake_by_ref();
			Poll::Pending
		}));
		tasks.push(Box::new(move |waiter: &kio::Waiter| {
			run.poll(&mut writer, waiter).map(|res| res.expect("serve"))
		}));

		// A frame's header takes well under a payload more, and a poll may also flush what
		// the last one buffered.
		let most = 2 * (BUDGET + 1) * 2 * PAYLOAD;
		let mut polls = 0;
		kio::wait(|owner| {
			if log.writes.lock().unwrap().len() >= FRAMES * PAYLOAD {
				return Poll::Ready(());
			}
			let before = log.writes.lock().unwrap().len();
			assert!(tasks.poll(owner).is_pending(), "the serve never ends");
			polls += 1;
			let wrote = log.writes.lock().unwrap().len() - before;
			assert!(wrote <= most, "one poll wrote {wrote} bytes");
			assert_eq!(turns.load(Ordering::Relaxed), polls, "the sibling missed a turn");
			assert!(polls <= FRAMES as u64, "the serve stalled");
			Poll::Pending
		})
		.await;
	}

	/// SUBSCRIBE_START tells the subscriber where each range's start landed, which a relative
	/// or time start leaves it no way to work out from the groups it receives.
	#[kio::test]
	async fn lite07_start_names_each_resolved_range_start() {
		use crate::track::{Anchor, Range};

		let mut track = track::Producer::new(
			Arc::new(broadcast::Info::default()),
			"test",
			crate::track::Info::timed(),
		);
		for sequence in 0..5 {
			write_group(&mut track, sequence, sequence * 1000);
		}
		let at = |millis| Anchor::Time(Timestamp::from_millis(millis).unwrap());
		let subscriber = track.subscribe(
			track::Subscription::default()
				.with_max_delay(Duration::from_secs(3600))
				.with_ranges([
					Range {
						start: Anchor::Relative(1),
						end: None,
					},
					Range {
						start: at(1500),
						end: Some(Anchor::Absolute(2)),
					},
					// Empty, so it starts nowhere.
					Range::groups(3..3),
					// Empty only once its start resolves to group 4.
					Range {
						start: Anchor::Relative(0),
						end: Some(Anchor::Absolute(2)),
					},
				]),
		);
		let (mut run, mut writer, log) = lite07_run(SinkSession::new(Log::default()), subscriber);
		let mut run = std::pin::pin!(kio::wait(move |waiter| run.poll(&mut writer, waiter)));
		assert!(futures::poll!(run.as_mut()).is_pending());

		let writes = log.writes.lock().unwrap().clone();
		match lite::SubscribeResponse::decode_slice(&writes, Version::Lite07)
			.unwrap()
			.0
		{
			lite::SubscribeResponse::Start(start) => assert_eq!(start.starts, [Some(3), Some(1), None, None]),
			other => panic!("expected SUBSCRIBE_START, got {other:?}"),
		}
	}

	/// A skipped sequence is accounted for by DROP rather than a stream count.
	#[kio::test]
	async fn lite07_end_drops_skipped_sequences() {
		let mut track = track::Producer::new(
			Arc::new(broadcast::Info::default()),
			"test",
			crate::track::Info::timed(),
		);
		let subscriber = track.subscribe(None);
		let (mut run, mut writer, log) = lite07_run(SinkSession::new(Log::default()), subscriber);
		let mut run = std::pin::pin!(kio::wait(move |waiter| run.poll(&mut writer, waiter)));

		write_group(&mut track, 0, 0);
		assert!(futures::poll!(run.as_mut()).is_pending());
		write_group(&mut track, 2, 2);
		track.finish().unwrap();
		run.await.unwrap();

		// SUBSCRIBE_START with the range starting at 0, a DROP for the skipped group 1
		// with NOT_FOUND (0x33), which the end gives up on at once, and SUBSCRIBE_END at 3.
		assert_eq!(
			*log.writes.lock().unwrap(),
			[0, 4, 1, 0, 1, 1, 2, 3, 1, 1, 0x33, 1, 2, 3, 0]
		);
	}

	/// A relay that holds only the first group of a backlog answers with the largest its
	/// source reported, so a live joiner downstream starts at the newest group rather than
	/// the oldest one the budget serves.
	#[kio::test]
	async fn lite07_start_reports_the_sources_largest() {
		let mut track = track::Producer::new(
			Arc::new(broadcast::Info::default()),
			"test",
			crate::track::Info::timed(),
		);
		// The source answered with group 4 as its largest; only group 0 has landed.
		track.report_largest(track::Position::group(4));
		let subscriber = track.subscribe(None);
		let (mut run, mut writer, log) = lite07_run(SinkSession::new(Log::default()), subscriber);
		write_group(&mut track, 0, 0);
		while run.start.is_none() {
			assert!(run.poll_step(&mut writer, &kio::Waiter::noop()).is_ready());
		}
		kio::wait(|waiter| writer.poll_flush(waiter)).await.unwrap();

		let writes = log.writes.lock().unwrap().clone();
		let (msg, _) = lite::SubscribeResponse::decode_slice(&writes, Version::Lite07).unwrap();
		let lite::SubscribeResponse::Start(start) = msg else {
			panic!("expected SUBSCRIBE_START, got {msg:?}");
		};
		assert_eq!(
			(start.starts, start.largest),
			(vec![Some(0)], Some(track::Position::group(4)))
		);
	}

	/// The SUBSCRIBE_DROP ranges a run loop's subscribe stream carried, once flushed.
	async fn drops(
		writer: &mut Writer<crate::transport::boxed::SendStream, Version>,
		log: &Log,
		version: Version,
	) -> Vec<(u64, u64)> {
		// The serve flushes what the run loop buffered before its FIN.
		kio::wait(|waiter| writer.poll_flush(waiter)).await.unwrap();
		let writes = log.writes.lock().unwrap().clone();
		let mut buf = writes.as_slice();
		let mut drops = Vec::new();
		while !buf.is_empty() {
			let (msg, size) = lite::SubscribeResponse::decode_slice(buf, version).unwrap();
			buf = &buf[size..];
			if let lite::SubscribeResponse::Drop(drop) = msg {
				drops.push((drop.start, drop.end));
			}
		}
		drops
	}

	/// A stale group already named in SUBSCRIBE_DROP stays dropped once SUBSCRIBE_UPDATE
	/// widens the max age past it: the subscriber may have settled it already.
	#[kio::test]
	async fn a_reported_stale_drop_survives_a_widened_max_age() {
		for version in [Version::Lite05, Version::Lite06, Version::Lite07] {
			let mut track = track::Producer::new(
				Arc::new(broadcast::Info::default()),
				"test",
				crate::track::Info::timed(),
			);
			let subscriber =
				track.subscribe(track::Subscription::default().with_max_delay(Duration::from_millis(1500)));
			let (mut run, mut writer, log) = lite_run(version, SinkSession::new(Log::default()), subscriber);
			write_group(&mut track, 0, 0);
			while run.poll_step(&mut writer, &kio::Waiter::noop()).is_ready() {}
			// Groups 1 and 2 reach 2s and 3s, at least 1.5s behind the 6s edge.
			for (sequence, millis) in [(1, 1000), (2, 2000), (3, 3000), (4, 6000)] {
				write_group(&mut track, sequence, millis);
			}
			while run.poll_step(&mut writer, &kio::Waiter::noop()).is_ready() {}
			assert_eq!(run.ctx.outcomes.opened.load(Ordering::Relaxed), 3, "{version:?}");
			assert_eq!(drops(&mut writer, &log, version).await, [(1, 2)], "{version:?}");

			run.update(lite::SubscribeUpdate {
				wanted_group: None,
				priority: 0,
				max_delay: Duration::from_secs(10),
				ranges: vec![lite::Range::span(None, 0, None, None)],
				order: Order::Descending,
			});
			while run.poll_step(&mut writer, &kio::Waiter::noop()).is_ready() {}
			assert_eq!(
				run.ctx.outcomes.opened.load(Ordering::Relaxed),
				3,
				"{version:?}: no stream for a dropped group"
			);
		}
	}

	/// Lite-05 and later name every sequence below the end that the subscription never
	/// got once the track ends: one skipped between groups, and one past the last.
	#[kio::test]
	async fn the_end_drops_the_sequences_never_served() {
		for version in [Version::Lite05, Version::Lite06] {
			let mut track = track::Producer::new(
				Arc::new(broadcast::Info::default()),
				"test",
				crate::track::Info::timed(),
			);
			let subscriber = track.subscribe(None);
			let (mut run, mut writer, log) = lite_run(version, SinkSession::new(Log::default()), subscriber);
			{
				let mut run = std::pin::pin!(kio::wait(|waiter| run.poll(&mut writer, waiter)));
				track.finish_at(track::Position::group(4), None).unwrap();
				write_group(&mut track, 0, 0);
				assert!(futures::poll!(run.as_mut()).is_pending());
				write_group(&mut track, 2, 2);
				// The last producer going ends the track with group 3 never produced.
				drop(track);
				run.await.unwrap();
			}
			assert_eq!(drops(&mut writer, &log, version).await, [(1, 1), (3, 3)], "{version:?}");
		}
	}

	/// An upstream DROP is forwarded while the track is still live, and the end sweep
	/// does not name it again.
	#[kio::test]
	async fn an_explicit_drop_is_forwarded_early_once() {
		for version in [Version::Lite05, Version::Lite06] {
			let mut track = track::Producer::new(
				Arc::new(broadcast::Info::default()),
				"test",
				crate::track::Info::timed(),
			);
			let subscriber = track.subscribe(None);
			let (mut run, mut writer, log) = lite_run(version, SinkSession::new(Log::default()), subscriber);
			write_group(&mut track, 0, 0);
			while run.start.is_none() {
				assert!(run.poll_step(&mut writer, &kio::Waiter::noop()).is_ready());
			}

			track.drop_groups(1..2, crate::StreamError::NotFound);
			assert!(run.poll_step(&mut writer, &kio::Waiter::noop()).is_ready());
			assert_eq!(drops(&mut writer, &log, version).await, [(1, 1)], "{version:?}");

			track.finish_at(track::Position::group(2), None).unwrap();
			drop(track);
			let Poll::Ready(result) = run.poll(&mut writer, &kio::Waiter::noop()) else {
				panic!("run did not finish");
			};
			result.unwrap();
			assert_eq!(drops(&mut writer, &log, version).await, [(1, 1)], "{version:?}");
		}
	}

	/// A group the publisher cuts is dropped once, while the track is live, and the end
	/// sweep does not name it again.
	#[kio::test]
	async fn a_cut_is_dropped_early_once() {
		let version = Version::Lite06;
		let mut track = track::Producer::new(
			Arc::new(broadcast::Info::default()),
			"test",
			crate::track::Info::timed(),
		);
		let subscriber = track.subscribe(None);
		let (mut run, mut writer, log) = lite_run(version, SinkSession::new(Log::default()), subscriber);
		write_group(&mut track, 0, 0);
		while run.poll_step(&mut writer, &kio::Waiter::noop()).is_ready() {}
		assert!(run.start.is_some());

		let group = track.create_group(group::Info { sequence: 1 }).unwrap();
		group.abort(Error::App(1)).unwrap();
		while run.poll_step(&mut writer, &kio::Waiter::noop()).is_ready() {}
		assert_eq!(drops(&mut writer, &log, version).await, [(1, 1)]);

		write_group(&mut track, 2, 2);
		track.finish_at(track::Position::group(3), None).unwrap();
		drop(track);
		kio::wait(|waiter| run.poll(&mut writer, waiter)).await.unwrap();
		assert_eq!(drops(&mut writer, &log, version).await, [(1, 1)]);
	}

	/// A wide upstream DROP is held and forwarded as a range, never one entry per sequence,
	/// and a wider one later forwards only what is new.
	#[kio::test]
	async fn a_wide_drop_is_forwarded_as_a_range() {
		const WIDE: u64 = 1 << 40;
		let version = Version::Lite06;
		let mut track = track::Producer::new(
			Arc::new(broadcast::Info::default()),
			"test",
			crate::track::Info::timed(),
		);
		let subscriber = track.subscribe(None);
		let (mut run, mut writer, log) = lite_run(version, SinkSession::new(Log::default()), subscriber);
		write_group(&mut track, 0, 0);
		while run.start.is_none() {
			assert!(run.poll_step(&mut writer, &kio::Waiter::noop()).is_ready());
		}

		track.drop_groups(2..WIDE, crate::StreamError::NotFound);
		assert!(run.poll_step(&mut writer, &kio::Waiter::noop()).is_ready());
		assert_eq!(drops(&mut writer, &log, version).await, [(2, WIDE - 1)]);

		track.drop_groups(1..WIDE + 2, crate::StreamError::NotFound);
		while run.poll_step(&mut writer, &kio::Waiter::noop()).is_ready() {}
		assert_eq!(
			drops(&mut writer, &log, version).await,
			[(2, WIDE - 1), (1, 1), (WIDE, WIDE + 1)]
		);
	}

	/// An upstream drop below the floor is forwarded once an update lowers the floor to it,
	/// not only at the end.
	#[kio::test]
	async fn a_lowered_start_forwards_a_drop_held_below_it() {
		let version = Version::Lite06;
		let mut track = track::Producer::new(
			Arc::new(broadcast::Info::default()),
			"test",
			crate::track::Info::timed(),
		);
		let subscription = track::Subscription::default()
			.with_start(5)
			.with_max_delay(Duration::from_secs(60));
		let subscriber = track.subscribe(subscription);
		let (mut run, mut writer, log) = lite_run(version, SinkSession::new(Log::default()), subscriber);
		write_group(&mut track, 5, 5);
		while run.poll_step(&mut writer, &kio::Waiter::noop()).is_ready() {}
		assert_eq!(run.start, Some(5));

		track.drop_groups(2..3, crate::StreamError::NotFound);
		while run.poll_step(&mut writer, &kio::Waiter::noop()).is_ready() {}
		assert_eq!(drops(&mut writer, &log, version).await, []);

		run.update(lite::SubscribeUpdate {
			wanted_group: None,
			order: crate::track::Order::Descending,
			priority: 0,
			max_delay: Duration::from_secs(60),
			ranges: vec![lite::Range::span(Some(0), 0, None, None)],
		});
		while run.poll_step(&mut writer, &kio::Waiter::noop()).is_ready() {}
		assert_eq!(drops(&mut writer, &log, version).await, [(2, 2)]);
	}

	/// A group the publisher cuts and then re-creates is served, not dropped.
	#[kio::test]
	async fn a_recreated_cut_is_not_dropped() {
		let version = Version::Lite06;
		let mut track = track::Producer::new(
			Arc::new(broadcast::Info::default()),
			"test",
			crate::track::Info::timed(),
		);
		let subscriber = track.subscribe(None);
		let (mut run, mut writer, log) = lite_run(version, SinkSession::new(Log::default()), subscriber);
		write_group(&mut track, 0, 0);
		while run.poll_step(&mut writer, &kio::Waiter::noop()).is_ready() {}

		// Recorded as a drop on the track, as a relay does for an upstream SUBSCRIBE_DROP,
		// before this run looks.
		track.drop_groups(1..2, crate::StreamError::NotFound);
		write_group(&mut track, 1, 1);
		track.finish_at(track::Position::group(2), None).unwrap();
		drop(track);
		kio::wait(|waiter| run.poll(&mut writer, waiter)).await.unwrap();
		assert_eq!(drops(&mut writer, &log, version).await, []);
	}

	/// A cut this run already dropped stays dropped: the subscriber accounted for it, so a
	/// group the publisher re-creates under it afterwards is not served.
	#[kio::test]
	async fn a_cut_dropped_then_recreated_is_not_served() {
		let version = Version::Lite06;
		let mut track = track::Producer::new(
			Arc::new(broadcast::Info::default()),
			"test",
			crate::track::Info::timed(),
		);
		let subscriber = track.subscribe(track::Subscription::default().with_max_delay(Duration::from_secs(60)));
		let (mut run, mut writer, log) = lite_run(version, SinkSession::new(Log::default()), subscriber);
		write_group(&mut track, 0, 0);
		while run.poll_step(&mut writer, &kio::Waiter::noop()).is_ready() {}
		assert!(run.start.is_some());

		let group = track.create_group(group::Info { sequence: 1 }).unwrap();
		group.abort(Error::App(1)).unwrap();
		while run.poll_step(&mut writer, &kio::Waiter::noop()).is_ready() {}
		assert_eq!(drops(&mut writer, &log, version).await, [(1, 1)]);

		write_group(&mut track, 1, 1);
		write_group(&mut track, 2, 2);
		track.finish_at(track::Position::group(3), None).unwrap();
		drop(track);
		kio::wait(|waiter| run.poll(&mut writer, waiter)).await.unwrap();
		assert_eq!(drops(&mut writer, &log, version).await, [(1, 1)]);
		assert_eq!(
			run.ctx.outcomes.opened.load(Ordering::Relaxed),
			2,
			"groups 0 and 2 only"
		);
	}

	/// A late group the cursor skips as stale is dropped at once, even when it is the only
	/// unread group and nothing else wakes the run.
	#[kio::test]
	async fn a_late_stale_group_is_dropped_early() {
		let version = Version::Lite06;
		let mut track = track::Producer::new(
			Arc::new(broadcast::Info::default()),
			"test",
			crate::track::Info::timed(),
		);
		let subscriber = track.subscribe(track::Subscription::default().with_max_delay(Duration::from_secs(1)));
		let (mut run, mut writer, log) = lite_run(version, SinkSession::new(Log::default()), subscriber);
		write_group(&mut track, 0, 0);
		write_group(&mut track, 2, 2);
		while run.poll_step(&mut writer, &kio::Waiter::noop()).is_ready() {}
		assert!(run.start.is_some());
		assert_eq!(drops(&mut writer, &log, version).await, []);

		// Its successor arrived a full budget ago, so group 1 is stale on the wall clock.
		kio::clock::sim::advance(Duration::from_secs(2)).await;
		write_group(&mut track, 1, 1);
		while run.poll_step(&mut writer, &kio::Waiter::noop()).is_ready() {}
		assert_eq!(
			run.ctx.outcomes.opened.load(Ordering::Relaxed),
			2,
			"group 1 is not served"
		);
		assert_eq!(drops(&mut writer, &log, version).await, [(1, 1)]);
	}

	/// A group skipped for a missing head after SUBSCRIBE_START is dropped at once, not
	/// left for the end sweep.
	#[kio::test]
	async fn a_missing_head_is_dropped_early() {
		let version = Version::Lite06;
		let mut track = track::Producer::new(
			Arc::new(broadcast::Info::default()),
			"test",
			crate::track::Info::timed(),
		);
		let subscriber = track.subscribe(track::Subscription::default().with_max_delay(Duration::from_secs(60)));
		let (mut run, mut writer, log) = lite_run(version, SinkSession::new(Log::default()), subscriber);
		write_group(&mut track, 0, 0);
		while run.poll_step(&mut writer, &kio::Waiter::noop()).is_ready() {}
		assert!(run.start.is_some());

		let mut headless = track.create_group(group::Info { sequence: 1 }).unwrap();
		headless.start_at(1).unwrap();
		headless
			.write_frame(Timestamp::from_millis(1).unwrap(), b"x".as_slice())
			.unwrap();
		while run.poll_step(&mut writer, &kio::Waiter::noop()).is_ready() {}
		assert_eq!(drops(&mut writer, &log, version).await, [(1, 1)]);

		// Given up and re-created whole after its DROP, it stays dropped.
		headless.abort(Error::App(1)).unwrap();
		write_group(&mut track, 1, 1);
		while run.poll_step(&mut writer, &kio::Waiter::noop()).is_ready() {}
		assert_eq!(
			run.ctx.outcomes.opened.load(Ordering::Relaxed),
			1,
			"only group 0 is served"
		);
		assert_eq!(drops(&mut writer, &log, version).await, [(1, 1)]);
	}

	/// A missing head at the model's largest sequence has no successor to name.
	#[kio::test]
	async fn a_missing_head_at_the_maximum_sequence_does_not_panic() {
		let track = track::Producer::new(
			Arc::new(broadcast::Info::default()),
			"test",
			crate::track::Info::timed(),
		);
		let subscriber = track.subscribe(track::Subscription::default().with_max_delay(Duration::from_secs(60)));
		let mut group = track.create_group(group::Info { sequence: u64::MAX }).unwrap();
		group.start_at(1).unwrap();
		group.write_frame(Timestamp::ZERO, b"x".as_slice()).unwrap();
		group.finish().unwrap();
		let (mut run, mut writer, _) = lite_run(Version::Lite07, SinkSession::new(Log::default()), subscriber);
		assert!(
			kio::wait(|waiter| run.poll(&mut writer, waiter))
				.now_or_never()
				.is_none()
		);
	}

	/// A long-budget reader warms the relay without changing a live reader's resolved start.
	#[kio::test]
	async fn cold_and_warm_relays_resolve_the_same_start() {
		fn drive<D: crate::time::Driver + Send + Unpin + 'static>(mut driver: D) {
			drop(kio::clock::sim::spawn(kio::clock::drive(move |now, waiter| {
				crate::time::Driver::poll(&mut driver, now, waiter)
			})));
		}

		fn origin(hop: u64) -> origin::Producer {
			let (origin, driver) = origin::Producer::new(origin::Config::new(crate::Hop::new(hop).unwrap()));
			drive(driver);
			origin
		}

		async fn connect(
			version: Version,
			publisher: &origin::Producer,
			subscriber: &origin::Producer,
		) -> (crate::Session, crate::Session) {
			let version = crate::Version::Lite(version);
			let config = crate::transport::memory::Config {
				protocol: Some(version.alpn().into()),
				..Default::default()
			};
			let (client_transport, server_transport) = crate::transport::memory::pair(config);
			let client = crate::Client::new()
				.with_versions(version.into())
				.with_subscriber(subscriber.clone());
			let server = crate::Server::new()
				.with_versions(version.into())
				.with_publisher(publisher);
			futures::join!(
				async {
					let (session, driver) = client.connect(kio::clock::now(), client_transport).await.unwrap();
					drive(driver);
					session
				},
				async {
					let (session, driver) = server.accept(kio::clock::now(), server_transport).await.unwrap();
					drive(driver);
					session
				}
			)
		}

		for version in [Version::Lite05, Version::Lite06, Version::Lite07] {
			let mut starts = Vec::new();
			for warm in [false, true] {
				let publisher = origin(1);
				let broadcast = publisher.create_broadcast("room").unwrap();
				let mut track = broadcast.create_track("test", crate::track::Info::timed()).unwrap();
				track.start_at(0).unwrap();
				broadcast.announce(Default::default()).unwrap();
				let relay = origin(2);
				let _upstream = connect(version, &publisher, &relay).await;
				let remote = relay.consume().follow("room", None).unwrap().available().await.unwrap();
				let mut keeper = match warm {
					true => Some(
						remote
							.track("test")
							.unwrap()
							.subscribe(
								track::Subscription::default()
									.with_range(0..)
									.with_max_delay(Duration::from_secs(60)),
							)
							.await
							.unwrap(),
					),
					false => None,
				};
				for sequence in 0..3 {
					write_group(&mut track, sequence, sequence * 10);
					if let Some(keeper) = &mut keeper {
						assert_eq!(keeper.recv_group().await.unwrap().unwrap().sequence, sequence);
					}
				}
				kio::clock::sim::advance(Duration::from_secs(1)).await;
				let subscriber = origin(3);
				let _downstream = connect(version, &relay, &subscriber).await;
				let remote = subscriber
					.consume()
					.follow("room", None)
					.unwrap()
					.available()
					.await
					.unwrap();
				let mut live = remote
					.track("test")
					.unwrap()
					.subscribe(track::Subscription::default().with_range(0..))
					.await
					.unwrap();
				assert_eq!(live.recv_group().await.unwrap().unwrap().sequence, 2);
				starts.push(kio::wait(|waiter| live.poll_start(waiter)).await);
			}
			assert_eq!(
				starts,
				[Some(2), Some(2)],
				"{version}: cold and warm relay starts differ"
			);
		}
	}

	/// A relay's retained stale groups must not change the start it answers downstream.
	#[kio::test]
	async fn the_start_names_past_the_stale_groups_it_skips() {
		for version in [Version::Lite05, Version::Lite06, Version::Lite07] {
			let mut starts = Vec::new();
			for warm in [false, true] {
				let mut track = track::Producer::new(
					Arc::new(broadcast::Info::default()),
					"test",
					crate::track::Info::timed(),
				);
				track.start_at(if warm { 0 } else { 2 }).unwrap();
				for sequence in (if warm { 0 } else { 2 })..3 {
					write_group(&mut track, sequence, sequence * 10);
				}
				kio::clock::sim::advance(Duration::from_secs(1)).await;
				let subscriber = track.subscribe(track::Subscription::default());
				let (mut run, mut writer, log) = lite_run(version, SinkSession::new(Log::default()), subscriber);
				assert!(
					kio::wait(|waiter| run.poll(&mut writer, waiter))
						.now_or_never()
						.is_none()
				);
				kio::wait(|waiter| writer.poll_flush(waiter)).await.unwrap();
				let writes = log.writes.lock().unwrap().clone();
				let (msg, _) = lite::SubscribeResponse::decode_slice(&writes, version).unwrap();
				let lite::SubscribeResponse::Start(start) = msg else {
					panic!("expected SUBSCRIBE_START, got {msg:?}");
				};
				starts.push(start.starts[0]);
			}
			assert_eq!(starts, [Some(2); 2], "{version}: cold and warm relay starts differ");
		}
	}

	/// Upstream drops received before the start exclude the entire range, even on lite-07.
	#[kio::test]
	async fn the_start_names_past_the_upstream_dropped_range() {
		for version in [Version::Lite05, Version::Lite06, Version::Lite07] {
			let mut track = track::Producer::new(
				Arc::new(broadcast::Info::default()),
				"test",
				crate::track::Info::timed(),
			);
			track.start_at(0).unwrap();
			let subscriber = track.subscribe(track::Subscription::default());
			let (mut run, mut writer, log) = lite_run(version, SinkSession::new(Log::default()), subscriber);
			track.drop_groups(0..1_000_000, crate::StreamError::NotFound);
			write_group(&mut track, 1_000_000, 0);
			assert!(
				kio::wait(|waiter| run.poll(&mut writer, waiter))
					.now_or_never()
					.is_none()
			);
			kio::wait(|waiter| writer.poll_flush(waiter)).await.unwrap();
			let writes = log.writes.lock().unwrap().clone();
			let (msg, _) = lite::SubscribeResponse::decode_slice(&writes, version).unwrap();
			let lite::SubscribeResponse::Start(start) = msg else {
				panic!("expected SUBSCRIBE_START, got {msg:?}");
			};
			assert_eq!(start.starts, [Some(1_000_000)], "{version}: named a dropped group");
		}
	}

	/// A range given up before the start resolves, but above it, is still named promptly.
	#[kio::test]
	async fn a_range_skipped_above_the_start_is_dropped() {
		for version in [Version::Lite05, Version::Lite06, Version::Lite07] {
			let mut track = track::Producer::new(
				Arc::new(broadcast::Info::default()),
				"test",
				crate::track::Info::timed(),
			);
			track.start_at(0).unwrap();
			let subscriber = track.subscribe(track::Subscription::default());
			let (mut run, mut writer, log) = lite_run(version, SinkSession::new(Log::default()), subscriber);
			// The newer group beats the older ones, so the start resolves at the source's 0.
			track.drop_groups(2..4, crate::StreamError::NotFound);
			write_group(&mut track, 5, 0);
			assert!(
				kio::wait(|waiter| run.poll(&mut writer, waiter))
					.now_or_never()
					.is_none()
			);
			assert_eq!(run.start, Some(0), "{version}");
			assert_eq!(drops(&mut writer, &log, version).await, [(2, 3)], "{version}");
		}
	}

	/// A SUBSCRIBE_UPDATE that lowers the start below SUBSCRIBE_START asks for those groups
	/// too, so the drops at the end count from there.
	#[kio::test]
	async fn the_end_drops_below_a_lowered_start() {
		let version = Version::Lite06;
		let mut track = track::Producer::new(
			Arc::new(broadcast::Info::default()),
			"test",
			crate::track::Info::timed(),
		);
		let subscription = track::Subscription::default()
			.with_start(5)
			.with_max_delay(Duration::from_secs(60));
		let subscriber = track.subscribe(subscription);
		let (mut run, mut writer, log) = lite_run(version, SinkSession::new(Log::default()), subscriber);
		write_group(&mut track, 2, 2);
		write_group(&mut track, 5, 5);
		track.finish_at(track::Position::group(7), None).unwrap();
		// Serve group 5, which resolves SUBSCRIBE_START.
		while run.start.is_none() {
			let step = run.poll_step(&mut writer, &kio::Waiter::noop());
			assert!(step.is_ready(), "the start never resolved");
		}
		assert_eq!(run.start, Some(5));

		run.update(lite::SubscribeUpdate {
			wanted_group: None,
			order: crate::track::Order::Descending,
			priority: 0,
			max_delay: Duration::from_secs(60),
			ranges: vec![lite::Range::span(Some(0), 0, None, None)],
		});
		{
			let mut run = std::pin::pin!(kio::wait(|waiter| run.poll(&mut writer, waiter)));
			assert!(futures::poll!(run.as_mut()).is_pending());
			drop(track);
			run.await.unwrap();
		}
		// Group 2 arrived before the subscription, but it is cached, so the lowered start
		// serves it and the drops name only what is missing around it.
		assert_eq!(drops(&mut writer, &log, version).await, [(0, 1), (3, 4), (6, 6)]);
	}

	/// A lowered start restarts the age of the gaps it newly asks for, as the subscriber does,
	/// so a gap below the old start isn't folded away on the time before the update.
	#[kio::test]
	async fn the_end_drops_below_a_lowered_start_after_the_grace() {
		let version = Version::Lite06;
		let mut track = track::Producer::new(
			Arc::new(broadcast::Info::default()),
			"test",
			crate::track::Info::timed(),
		);
		let subscription = track::Subscription::default()
			.with_start(5)
			.with_max_delay(Duration::from_secs(1));
		let subscriber = track.subscribe(subscription);
		let (mut run, mut writer, log) = lite_run(version, SinkSession::new(Log::default()), subscriber);
		write_group(&mut track, 5, 0);
		while run.start.is_none() {
			let step = run.poll_step(&mut writer, &kio::Waiter::noop());
			assert!(step.is_ready(), "the start never resolved");
		}

		kio::clock::sim::advance(Duration::from_secs(2)).await;
		run.update(lite::SubscribeUpdate {
			wanted_group: None,
			order: crate::track::Order::Descending,
			priority: 0,
			max_delay: Duration::from_secs(1),
			ranges: vec![lite::Range::span(Some(0), 0, None, None)],
		});
		{
			let mut run = std::pin::pin!(kio::wait(|waiter| run.poll(&mut writer, waiter)));
			// Group 2's successor arrives with it: group 5 arrived a full budget before
			// the update, so group 2 bounded by it would be stale on the wall clock. Group 4
			// is, and goes unserved.
			write_group(&mut track, 4, 0);
			write_group(&mut track, 2, 0);
			assert!(futures::poll!(run.as_mut()).is_pending());
			track.finish_at(track::Position::group(6), None).unwrap();
			drop(track);
			run.await.unwrap();
		}
		// The feed gave up on 0, 1 and 3 before the update, so they are misses named as the
		// cursor judges them, ahead of the stale group.
		assert_eq!(drops(&mut writer, &log, version).await, [(0, 1), (3, 3), (4, 4)]);
	}

	/// After a raised start, lowering it again restarts the gaps below the raised floor, not
	/// only those below the lowest start ever asked for.
	#[kio::test]
	async fn the_end_drops_below_a_start_raised_then_lowered() {
		let version = Version::Lite06;
		let mut track = track::Producer::new(
			Arc::new(broadcast::Info::default()),
			"test",
			crate::track::Info::timed(),
		);
		let subscription = track::Subscription::default()
			.with_start(0)
			.with_max_delay(Duration::from_secs(1));
		let subscriber = track.subscribe(subscription);
		let (mut run, mut writer, log) = lite_run(version, SinkSession::new(Log::default()), subscriber);
		write_group(&mut track, 0, 0);
		while run.start.is_none() {
			let step = run.poll_step(&mut writer, &kio::Waiter::noop());
			assert!(step.is_ready(), "the start never resolved");
		}

		let update = |start_group| lite::SubscribeUpdate {
			wanted_group: None,
			order: crate::track::Order::Descending,
			priority: 0,
			max_delay: Duration::from_secs(1),
			ranges: vec![lite::Range::span(Some(start_group), 0, None, None)],
		};
		run.update(update(5));
		{
			let mut run = std::pin::pin!(kio::wait(|waiter| run.poll(&mut writer, waiter)));
			write_group(&mut track, 5, 0);
			assert!(futures::poll!(run.as_mut()).is_pending());
		}

		kio::clock::sim::advance(Duration::from_millis(900)).await;
		run.update(update(2));
		kio::clock::sim::advance(Duration::from_millis(200)).await;
		track.finish_at(track::Position::group(6), None).unwrap();
		drop(track);
		kio::wait(|waiter| run.poll(&mut writer, waiter)).await.unwrap();
		// The feed gives up on 2..5 once its grace passes, a miss nothing fills.
		assert_eq!(drops(&mut writer, &log, version).await, [(2, 4), (1, 1)]);
	}

	/// A raised floor sends no early drop: a later update may lower it again, and the
	/// subscriber cannot take back a drop for the groups that reopens.
	#[kio::test]
	async fn a_raised_floor_is_not_dropped_early() {
		let version = Version::Lite06;
		let mut track = track::Producer::new(
			Arc::new(broadcast::Info::default()),
			"test",
			crate::track::Info::timed(),
		);
		let subscription = track::Subscription::default()
			.with_start(0)
			.with_max_delay(Duration::from_secs(10));
		let subscriber = track.subscribe(subscription);
		let (mut run, mut writer, log) = lite_run(version, SinkSession::new(Log::default()), subscriber);
		write_group(&mut track, 0, 0);
		while run.start.is_none() {
			assert!(run.poll_step(&mut writer, &kio::Waiter::noop()).is_ready());
		}

		let update = |start_group| lite::SubscribeUpdate {
			wanted_group: None,
			order: crate::track::Order::Ascending,
			priority: 0,
			max_delay: Duration::from_secs(10),
			ranges: vec![lite::Range::span(Some(start_group), 0, None, None)],
		};
		run.update(update(5));
		while run.poll_step(&mut writer, &kio::Waiter::noop()).is_ready() {}
		assert_eq!(drops(&mut writer, &log, version).await, []);

		run.update(update(0));
		for sequence in 1..5 {
			write_group(&mut track, sequence, 0);
		}
		track.finish_at(track::Position::group(5), None).unwrap();
		drop(track);
		kio::wait(|waiter| run.poll(&mut writer, waiter)).await.unwrap();
		assert_eq!(drops(&mut writer, &log, version).await, []);
	}

	/// Pre-06 an absent Group Start is the latest group, so an update that drops a named
	/// floor re-pins the cursor there: a group created below it afterwards is not served.
	#[kio::test]
	async fn a_pre06_update_dropping_the_floor_repins_to_the_latest_group() {
		let mut track = track::Producer::new(
			Arc::new(broadcast::Info::default()),
			"test",
			crate::track::Info::timed(),
		);
		let subscriber = track.subscribe(
			track::Subscription::default()
				.with_start(0)
				.with_max_delay(Duration::from_secs(60)),
		);
		let (mut run, mut writer, _log) = lite_run(Version::Lite05, SinkSession::new(Log::default()), subscriber);
		let mut settle = |run: &mut TrackRun| {
			let res = kio::wait(|waiter| run.poll(&mut writer, waiter)).now_or_never();
			assert!(res.is_none(), "the run ended");
			run.ctx.outcomes.opened.load(Ordering::Relaxed)
		};

		write_group(&mut track, 7, 7);
		assert_eq!(settle(&mut run), 1);

		run.update(lite::SubscribeUpdate {
			wanted_group: None,
			order: crate::track::Order::Descending,
			priority: 0,
			max_delay: Duration::from_secs(60),
			ranges: vec![lite::Range::span(None, 0, None, None)],
		});
		write_group(&mut track, 5, 5);
		assert_eq!(settle(&mut run), 1, "served a group below the latest");

		write_group(&mut track, 8, 8);
		assert_eq!(settle(&mut run), 2);
	}

	/// On an empty track the re-pin has no latest group to name, so the first group the
	/// track produces is served, even below the dropped floor.
	#[kio::test]
	async fn a_pre06_update_dropping_the_floor_on_an_empty_track_serves_the_next_group() {
		let mut track = track::Producer::new(
			Arc::new(broadcast::Info::default()),
			"test",
			crate::track::Info::timed(),
		);
		let subscriber = track.subscribe(
			track::Subscription::default()
				.with_start(5)
				.with_max_delay(Duration::from_secs(60)),
		);
		let (mut run, mut writer, _log) = lite_run(Version::Lite05, SinkSession::new(Log::default()), subscriber);
		run.update(lite::SubscribeUpdate {
			wanted_group: None,
			order: crate::track::Order::Descending,
			priority: 0,
			max_delay: Duration::from_secs(60),
			ranges: vec![lite::Range::span(None, 0, None, None)],
		});

		write_group(&mut track, 0, 0);
		let res = kio::wait(|waiter| run.poll(&mut writer, waiter)).now_or_never();
		assert!(res.is_none(), "the run ended");
		assert_eq!(
			run.ctx.outcomes.opened.load(Ordering::Relaxed),
			1,
			"group 0 was not served"
		);
	}

	/// A never-arrived gap remains owed after the publisher's grace: only the subscriber
	/// may age it out, and the end sweep must still name it before the FIN.
	#[kio::test]
	async fn the_end_drops_a_never_arrived_gap_past_the_grace() {
		let version = Version::Lite06;
		let mut track = track::Producer::new(
			Arc::new(broadcast::Info::default()),
			"test",
			crate::track::Info::timed(),
		);
		let subscriber = track.subscribe(track::Subscription::default().with_max_delay(Duration::from_secs(1)));
		let (mut run, mut writer, log) = lite_run(version, SinkSession::new(Log::default()), subscriber);
		{
			let mut run = std::pin::pin!(kio::wait(|waiter| run.poll(&mut writer, waiter)));
			write_group(&mut track, 0, 0);
			assert!(futures::poll!(run.as_mut()).is_pending());
			write_group(&mut track, 2, 0);
			assert!(futures::poll!(run.as_mut()).is_pending());

			kio::clock::sim::advance(Duration::from_secs(2)).await;
			write_group(&mut track, 4, 0);
			assert!(futures::poll!(run.as_mut()).is_pending());
			track.finish_at(track::Position::group(5), None).unwrap();
			drop(track);
			run.await.unwrap();
		}
		assert_eq!(drops(&mut writer, &log, version).await, [(1, 1), (3, 3)]);
	}

	/// A lite-07 run over a relay's track, whose upstream subscription still waits on the
	/// source's SUBSCRIBE_START, on a subscribe stream the test can push updates onto.
	struct RelayRun {
		run: TrackRun,
		writer: Writer<crate::transport::boxed::SendStream, Version>,
		session: ScriptedSession,
		outcomes: Arc<GroupOutcomes>,
	}

	impl RelayRun {
		/// Subscribe to `track` from `start_group`.
		fn new(track: &mut track::Producer, start_group: u64) -> Self {
			track.request_start(Some(0)).unwrap();
			let subscription = track::Subscription::default()
				.with_start(start_group)
				.with_max_delay(Duration::from_secs(30));
			let subscriber = track.subscribe(subscription);

			let session = ScriptedSession::new(Vec::new());
			let writer = Writer::new(
				crate::transport::boxed::SendStream::new(SinkSend::new(session.log.clone())),
				Version::Lite07,
			);
			let steer = kio::Producer::new(Steer::default());
			let outcomes = Arc::<GroupOutcomes>::default();
			let ctx = Subscription {
				session: crate::driver::Transport::new(session.clone()),
				id: 0,
				track_name: "test".into(),
				priority: PriorityQueue::default(),
				publisher: 0,
				group: None,
				steer: steer.consume(),
				steer_seen: Steer::default(),
				version: Version::Lite07,
				stamp: Stamp::new(Version::Lite07, Some(crate::Timescale::MILLI)),
				runtime: crate::time::sim(),
				outcomes: outcomes.clone(),
				ietf: false,
			};
			let bounds = Bounds::new(&[lite::Range::span(Some(start_group), 0, None, None)], None).unwrap();
			Self {
				run: TrackRun::new(ctx, subscriber, bounds, 0, steer),
				writer,
				session,
				outcomes,
			}
		}

		/// Drive the run until it parks, which it must.
		fn settle(&mut self) {
			let Self { run, writer, .. } = self;
			let res = kio::wait(|waiter| run.poll(writer, waiter)).now_or_never();
			assert!(res.is_none(), "the run ended");
		}

		/// How many group streams the run opened.
		fn opened(&self) -> u64 {
			self.outcomes.opened.load(Ordering::Relaxed)
		}

		/// Whether the first thing written was SUBSCRIBE_START at `group`.
		fn started_at(&self, group: u64) -> bool {
			use crate::coding::Decode;
			let writes = self.session.log.writes.lock().unwrap();
			matches!(
				lite::SubscribeResponse::decode_slice(&writes, Version::Lite07),
				Ok((lite::SubscribeResponse::Start(start), _)) if start.starts == [Some(group)]
			)
		}
	}

	/// A resume past the only finished group leaves that group below the cursor.
	/// Widening to no floor serves it: it is the live edge until a newer group
	/// exists, and a catalog never publishes the next one.
	#[kio::test]
	async fn a_widening_update_serves_the_finished_group_it_had_skipped() {
		let mut track = track::Producer::new(
			Arc::new(broadcast::Info::default()),
			"test",
			crate::track::Info::timed(),
		);
		write_group(&mut track, 0, 0);
		let mut relay = RelayRun::new(&mut track, 1);
		relay.settle();
		assert_eq!(relay.opened(), 0, "the resume is past the only group");

		relay.run.update(lite::SubscribeUpdate {
			wanted_group: None,
			order: crate::track::Order::Descending,
			priority: 0,
			max_delay: Duration::from_secs(30),
			ranges: vec![lite::Range::span(None, 0, None, None)],
		});
		relay.settle();
		assert_eq!(relay.opened(), 1, "the finished group is the live edge");
	}

	/// Widening the floor serves a cached group below the old one, even though it arrived
	/// before the groups already served, and serves those only once.
	#[kio::test]
	async fn a_widening_update_serves_a_cached_group_below_the_old_floor() {
		let mut track = track::Producer::new(
			Arc::new(broadcast::Info::default()),
			"test",
			crate::track::Info::timed(),
		);
		for sequence in 0..3 {
			write_group(&mut track, sequence, sequence);
		}
		let mut relay = RelayRun::new(&mut track, 1);
		relay.settle();
		track.start_at(0).unwrap();
		relay.settle();
		assert_eq!(relay.opened(), 2, "groups 1 and 2 are at or above the floor");

		relay.run.update(lite::SubscribeUpdate {
			wanted_group: None,
			order: crate::track::Order::Descending,
			priority: 0,
			max_delay: Duration::from_secs(30),
			ranges: vec![lite::Range::span(None, 0, None, None)],
		});
		relay.settle();
		assert_eq!(relay.opened(), 3, "group 0 is cached and now in range");
	}

	/// A SUBSCRIBE_UPDATE landing while the first group waits on the source's start keeps
	/// the floor it raised: resolving the start from the held group must not lower it.
	#[kio::test]
	async fn held_first_group_keeps_an_updated_floor() {
		let mut track = track::Producer::new(
			Arc::new(broadcast::Info::default()),
			"test",
			crate::track::Info::timed(),
		);
		let mut relay = RelayRun::new(&mut track, 0);

		write_group(&mut track, 5, 0);
		relay.settle();

		// The subscriber moves its start past the held group before the source resolves.
		let update = lite::SubscribeUpdate {
			wanted_group: None,
			order: crate::track::Order::Descending,
			priority: 0,
			max_delay: Duration::ZERO,
			ranges: vec![lite::Range::span(Some(7), 0, None, None)],
		};
		relay.run.update(update);
		relay.settle();

		track.start_at(0).unwrap();
		relay.settle();
		assert_eq!(relay.opened(), 1, "the held group is served");

		// Group 6 is below the updated floor.
		write_group(&mut track, 6, 6);
		relay.settle();
		assert_eq!(relay.opened(), 1, "served a group below the floor");
	}

	/// A source whose feed starts below the subscriber's floor serves the floor's group, so
	/// a newer group arriving first must not resolve the start past it.
	#[kio::test]
	async fn held_first_group_resolves_to_the_floor_under_the_source() {
		let mut track = track::Producer::new(
			Arc::new(broadcast::Info::default()),
			"test",
			crate::track::Info::timed(),
		);
		let mut relay = RelayRun::new(&mut track, 5);

		write_group(&mut track, 6, 6);
		relay.settle();
		track.start_at(0).unwrap();
		relay.settle();
		assert_eq!(relay.opened(), 1, "the held group is served");

		write_group(&mut track, 5, 5);
		relay.settle();
		assert_eq!(relay.opened(), 2, "dropped the floor's group");
		assert!(relay.started_at(5));
	}

	/// A group skipped for a missing head before the start resolves is never served, so
	/// SUBSCRIBE_START must not name it, even where the source's feed starts.
	#[kio::test]
	async fn held_first_group_starts_past_a_skipped_head() {
		let mut track = track::Producer::new(
			Arc::new(broadcast::Info::default()),
			"test",
			crate::track::Info::timed(),
		);
		let mut relay = RelayRun::new(&mut track, 5);

		// Group 5's first frame is gone.
		let mut headless = track.create_group(group::Info { sequence: 5 }).unwrap();
		headless.start_at(1).unwrap();
		headless
			.write_frame(Timestamp::from_millis(5).unwrap(), b"x".as_slice())
			.unwrap();
		headless.finish().unwrap();
		write_group(&mut track, 6, 6);
		relay.settle();

		track.start_at(5).unwrap();
		relay.settle();
		assert_eq!(relay.opened(), 1, "the held group is served");
		assert!(relay.started_at(6), "named the skipped group");
	}

	/// Adjacent ranges skipped with different errors before the start resolves are all
	/// passed over, so SUBSCRIBE_START names neither.
	#[kio::test]
	async fn held_first_group_starts_past_adjacent_skips() {
		let mut track = track::Producer::new(
			Arc::new(broadcast::Info::default()),
			"test",
			crate::track::Info::timed(),
		);
		let mut relay = RelayRun::new(&mut track, 4);

		track.drop_groups(4..5, StreamError::Old);
		// Group 5's first frame is gone.
		let mut headless = track.create_group(group::Info { sequence: 5 }).unwrap();
		headless.start_at(1).unwrap();
		headless
			.write_frame(Timestamp::from_millis(5).unwrap(), b"x".as_slice())
			.unwrap();
		headless.finish().unwrap();
		write_group(&mut track, 6, 6);
		relay.settle();

		track.start_at(4).unwrap();
		relay.settle();
		assert_eq!(relay.opened(), 1, "the held group is served");
		assert!(relay.started_at(6), "named a skipped group");
	}

	/// A track that ends without a group still ends the subscription, with no stream owed.
	#[kio::test]
	async fn lite07_empty_track_ends_without_a_start() {
		let track = track::Producer::new(
			Arc::new(broadcast::Info::default()),
			"test",
			crate::track::Info::timed(),
		);
		let subscriber = track.subscribe(None);
		track.finish().unwrap();

		let (mut run, mut writer, log) = lite07_run(SinkSession::new(Log::default()), subscriber);
		kio::wait(|waiter| run.poll(&mut writer, waiter)).await.unwrap();
		assert_eq!(*log.writes.lock().unwrap(), [1, 2, 0, 0]);
	}

	/// A declared end is sent before stream credit, and a group that expires first is dropped.
	#[kio::test]
	async fn lite07_end_precedes_blocked_streams_and_reports_expiry() {
		let gate = kio::Producer::new(false);
		let mut track = track::Producer::new(
			Arc::new(broadcast::Info::default()),
			"test",
			crate::track::Info::timed(),
		);
		let subscriber = track.subscribe(None);
		write_group(&mut track, 0, 0);

		let (mut run, mut writer, log) = lite07_run(SinkSession::gated_open_uni(gate.consume()), subscriber);
		let mut run = std::pin::pin!(kio::wait(move |waiter| run.poll(&mut writer, waiter)));
		assert!(futures::poll!(run.as_mut()).is_pending());

		// Group 1 lands a second later, expiring group 0 before it ever opened.
		kio::clock::sim::advance(Duration::from_secs(1)).await;
		write_group(&mut track, 1, 1000);
		track.finish().unwrap();
		assert!(futures::poll!(run.as_mut()).is_pending(), "group 1 is still opening");
		assert_eq!(
			*log.writes.lock().unwrap(),
			// The DROP carries OLD (0x34).
			[0, 4, 1, 0, 1, 1, 2, 3, 0, 0, 0x34, 1, 2, 2, 0],
			"the expired group is dropped and the end precedes stream credit"
		);

		let Ok(mut open) = gate.write() else {
			panic!("transport gate closed");
		};
		*open = true;
		drop(open);
		run.await.unwrap();
		assert_eq!(
			*log.writes.lock().unwrap(),
			[0, 4, 1, 0, 1, 1, 2, 3, 0, 0, 0x34, 1, 2, 2, 0]
		);
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::lite::test_transport::{Close, ScriptedSession, SinkSend, SinkSession};
	use crate::model::ProduceTest;
	use crate::{broadcast, coding::Encode};
	use futures::FutureExt;

	/// A peer that declares no origin in its SETUP is split-horizoned by the identity
	/// the caller assigned it, on the data plane and not just the announce filter.
	/// Otherwise it can subscribe its way back to content that already flowed through
	/// it, which is the loop the announce filter exists to prevent.
	#[kio::test]
	async fn serving_origin_falls_back_to_the_assigned_identity() {
		let assigned = crate::Hop::new(777).unwrap();
		let upstream = crate::Hop::new(778).unwrap();
		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();

		let mut echoed_hops = Hops::new();
		echoed_hops.push(crate::Hop::UNKNOWN).unwrap();
		let _echoed = origin
			.requests_route(
				"echoed",
				crate::origin::Route::default()
					.with_hops(echoed_hops)
					.with_via(assigned),
				None,
			)
			.unwrap();

		let mut local_hops = Hops::new();
		local_hops.push(upstream).unwrap();
		let _local = origin
			.requests_route("local", crate::origin::Route::default().with_hops(local_hops), None)
			.unwrap();

		// A SETUP that declares no origin of its own, so only the assigned one applies.
		let peer_setup = crate::lite::PeerSetup::new(false, crate::transport::Address::Setup);
		peer_setup.set(crate::lite::Setup::default());
		let (_, goaway) = crate::goaway::Handle::new(true);

		let publisher = Publisher::new(PublisherConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(SinkSession::new(Default::default())),
			origin: origin.consume(),
			version: Version::Lite06,
			peer_setup,
			goaway,
			peer_hop: Some(assigned),
			auth: crate::auth::Link::new(false),
			peer_grant: Default::default(),
			client: false,
			subscriptions: Default::default(),
			estimate: kio::Producer::default().weak(),
		});

		let serving = kio::wait(|waiter| publisher.shared.poll_serving_origin(waiter)).await;
		use futures::FutureExt;
		assert!(
			serving.request("echoed/x", None).now_or_never().unwrap().is_err(),
			"served the peer its own route"
		);
		assert!(
			serving.request("local/x", None).now_or_never().is_none(),
			"withheld an independent route"
		);
	}

	/// Lite01/02 send the initial active set as ANNOUNCE_INIT. It must apply the
	/// same per-peer route selection as the live loop: a broadcast whose only
	/// route flows through the excluded hop (here the peer's assigned identity,
	/// `Client::with_peer_hop`) is filtered from the initial set too.
	#[kio::test]
	async fn announce_init_applies_route_selection() {
		let assigned = crate::Hop::new(777).unwrap();
		let clean_publisher = crate::Hop::new(778).unwrap();
		let self_origin = crate::Hop::new(1).unwrap();
		let origin = crate::origin::Config::new(self_origin).produce();

		let mut tainted_hops = Hops::new();
		tainted_hops.push(crate::Hop::UNKNOWN).unwrap();
		let _tainted = origin
			.announce(
				"echoed",
				crate::origin::Route::default()
					.with_hops(tainted_hops)
					.with_via(assigned),
			)
			.unwrap();

		let mut clean_hops = Hops::new();
		clean_hops.push(clean_publisher).unwrap();
		let _clean = origin
			.announce("local", crate::origin::Route::default().with_hops(clean_hops))
			.unwrap();

		let gate = kio::Producer::new(true);
		let session = SinkSession::gated_bi(gate.consume());
		let log = session.log.clone();
		let mut stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), Version::Lite01)
			.await
			.unwrap();

		let consumer = origin.consume().excluding(assigned);
		let mut announced = consumer.announced();
		let mut run = std::pin::pin!(Publisher::run_announce(
			&mut stream,
			&consumer,
			&mut announced,
			self_origin,
			Version::Lite01,
			None,
		));
		assert!(futures::poll!(run.as_mut()).is_pending());

		let writes = log.writes.lock().unwrap();
		assert!(
			writes.windows(b"local".len()).any(|w| w == b"local"),
			"clean broadcast in ANNOUNCE_INIT"
		);
		assert!(
			!writes.windows(b"echoed".len()).any(|w| w == b"echoed"),
			"echoed broadcast filtered from ANNOUNCE_INIT"
		);
	}

	/// A broadcast sent in ANNOUNCE_INIT is live like any other, so its end reaches
	/// the peer instead of being treated as never advertised.
	#[kio::test]
	async fn announce_init_broadcast_ends() {
		let self_origin = crate::Hop::new(1).unwrap();
		let origin = crate::origin::Config::new(self_origin).produce();
		let cam = origin.announce("cam", crate::origin::Route::default()).unwrap();

		let gate = kio::Producer::new(true);
		let session = SinkSession::gated_bi(gate.consume());
		let log = session.log.clone();
		let mut stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), Version::Lite01)
			.await
			.unwrap();
		let consumer = origin.consume();
		let mut announced = consumer.announced();
		let mut run = std::pin::pin!(Publisher::run_announce(
			&mut stream,
			&consumer,
			&mut announced,
			self_origin,
			Version::Lite01,
			None,
		));
		assert!(futures::poll!(run.as_mut()).is_pending());
		drop(cam);
		kio::clock::timeout(std::time::Duration::from_secs(10), async {
			while log.writes.lock().unwrap().windows(3).filter(|w| *w == b"cam").count() < 2 {
				assert!(futures::poll!(run.as_mut()).is_pending());
				kio::clock::sim::yield_now().await;
			}
		})
		.await
		.expect("the initial broadcast's end was never sent");
	}

	/// A close that lands before the initial burst still sends it, then ends every
	/// broadcast, so the peer never sees a FIN in place of ANNOUNCE_INIT.
	#[kio::test]
	async fn withdraw_before_init_sends_the_initial_burst() {
		let self_origin = crate::Hop::new(1).unwrap();
		let origin = crate::origin::Config::new(self_origin).produce();
		let _cam = origin.announce("cam", crate::origin::Route::default()).unwrap();

		let gate = kio::Producer::new(true);
		let session = SinkSession::gated_bi(gate.consume());
		let log = session.log.clone();
		let mut stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), Version::Lite01)
			.await
			.unwrap();
		let consumer = origin.consume();
		let mut announced = consumer.announced();

		let mut run = AnnounceRun::new(crate::Path::default(), self_origin, Version::Lite01);
		run.withdraw(&mut stream, &consumer, &mut announced).unwrap();
		let _ = kio::wait(|waiter| Poll::Ready(run.poll(&mut stream, &consumer, &mut announced, waiter))).await;

		let writes = log.writes.lock().unwrap();
		assert_eq!(
			writes.windows(b"cam".len()).filter(|w| *w == b"cam").count(),
			2,
			"ANNOUNCE_INIT carries the broadcast, then its end follows"
		);
	}

	/// Decode the PROBE messages the publisher wrote. The publisher only replies on
	/// a stream the subscriber opened, so there is no leading ControlType here.
	fn decode_probes(bytes: &[u8], version: Version) -> Vec<lite::Probe> {
		use crate::coding::Decode as _;
		let mut slice = bytes;
		let mut out = Vec::new();
		while bytes::Buf::remaining(&slice) > 0 {
			out.push(crate::coding::decode_buf(&mut slice, version, |r, v| lite::Probe::decode(r, v)).unwrap());
		}
		out
	}

	/// Build the poll-based PROBE server around a stream opened by the test peer,
	/// following `estimate` as it would the supervisor's.
	fn probe_server(
		session: Transport,
		stream: Stream<Transport, Version>,
		version: Version,
		estimate: &kio::Producer<crate::session::Estimate>,
	) -> ProbeServe {
		let origin = Hop::random().produce();
		let (_, goaway) = crate::goaway::Handle::new(true);
		let publisher = Publisher::new(PublisherConfig {
			runtime: crate::time::sim(),
			session,
			origin: origin.consume(),
			version,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			goaway,
			peer_hop: None,
			auth: crate::auth::Link::new(false),
			peer_grant: Default::default(),
			client: false,
			subscriptions: Default::default(),
			estimate: estimate.weak(),
		});
		ProbeServe::new(publisher.shared.clone(), publisher.runtime.clone(), stream)
	}

	#[kio::test]
	async fn malformed_complete_goaway_closes_the_session() {
		use kio::Task;
		for version in [Version::Lite04, Version::Lite05, Version::Lite06, Version::Lite07] {
			// A complete two-byte body with an empty URI and one unread byte.
			let session = ScriptedSession::new(vec![2, 0, 0]);
			let log = session.log.clone();
			let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), version)
				.await
				.unwrap();
			let estimate = kio::Producer::default();
			let probe = probe_server(crate::driver::Transport::new(session), stream, version, &estimate);
			let stream = probe.stream.expect("probe stream");
			let mut control = Control {
				shared: probe.shared,
				runtime: crate::time::sim(),
				state: ControlState::Goaway { stream },
			};
			assert!(control.poll(&kio::Waiter::noop()).is_ready());
			assert_eq!(
				log.closes().first().map(|(code, _)| *code),
				Some(SessionError::ProtocolViolation.to_code()),
				"{version}"
			);
		}
	}

	#[kio::test]
	async fn unknown_control_types_reset_only_their_stream() {
		use kio::Task;
		// An unassigned type everywhere, then types a version predates: PROBE before lite-03,
		// GOAWAY before lite-04, FETCH and TRACK before lite-05, AUTH and TRACKS before lite-07.
		let cases = [
			(Version::Lite06, 9),
			(Version::Lite07, 9),
			(Version::Lite02, lite::ControlType::Probe as u8),
			(Version::Lite03, lite::ControlType::Goaway as u8),
			(Version::Lite04, lite::ControlType::Fetch as u8),
			(Version::Lite04, lite::ControlType::Track as u8),
			(Version::Lite06, lite::ControlType::Auth as u8),
			(Version::Lite06, lite::ControlType::Tracks as u8),
		];
		for (version, kind) in cases {
			let session = ScriptedSession::new(vec![kind]);
			let log = session.log.clone();
			let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), version)
				.await
				.unwrap();
			let estimate = kio::Producer::default();
			let probe = probe_server(crate::driver::Transport::new(session), stream, version, &estimate);
			let stream = probe.stream.expect("probe stream");
			let mut control = Control {
				shared: probe.shared,
				runtime: crate::time::sim(),
				state: ControlState::Start { stream },
			};
			assert!(control.poll(&kio::Waiter::noop()).is_ready());
			assert!(log.closes().is_empty(), "{version} type {kind}: session closed");
			assert!(!log.resets().is_empty(), "{version} type {kind}: stream not refused");
		}
	}

	/// A PROBE stream served against a transport reporting `stats`.
	struct Probing {
		estimate: kio::Producer<crate::session::Estimate>,
		log: crate::lite::test_transport::Log,
		server: ProbeServe,
		version: Version,
		_gate: kio::Producer<bool>,
	}

	impl Probing {
		async fn new(stats: crate::lite::test_transport::SinkStats, version: Version) -> Self {
			let gate = kio::Producer::new(true);
			let session = SinkSession::gated_bi(gate.consume()).with_stats(stats);
			let log = session.log.clone();
			let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), version)
				.await
				.unwrap();
			// The supervisor's construction-time snapshot.
			let estimate = kio::Producer::new(crate::session::Estimate::read(&session));
			let server = probe_server(crate::driver::Transport::new(session), stream, version, &estimate);
			Self {
				estimate,
				log,
				server,
				version,
				_gate: gate,
			}
		}

		/// Publish a new estimate, as the supervisor does when a sample moves.
		fn set(&self, send_rate: Option<u64>, rtt: Option<Duration>) {
			let Ok(mut estimate) = self.estimate.write() else {
				panic!("the estimate outlives the stream");
			};
			*estimate = crate::session::Estimate {
				send_rate: send_rate.map(crate::bandwidth::Rate::from_bps),
				rtt,
			};
		}

		/// Run until everything parks at `after` from now, returning every report so far.
		async fn reports(&mut self, after: Duration) -> Vec<lite::Probe> {
			let server = &mut self.server;
			let run = kio::wait(|waiter| server.poll_probe(waiter));
			assert!(kio::clock::timeout(after, run).await.is_err(), "the stream never ends");
			decode_probes(&self.log.writes.lock().unwrap(), self.version)
		}
	}

	/// Drive the PROBE state machine against a transport reporting `stats`, and
	/// return whatever it wrote before parking.
	async fn probe_writes(stats: crate::lite::test_transport::SinkStats) -> Vec<lite::Probe> {
		probe_writes_version(stats, Version::Lite05).await
	}

	/// As above, on a specific negotiated version.
	async fn probe_writes_version(stats: crate::lite::test_transport::SinkStats, version: Version) -> Vec<lite::Probe> {
		Probing::new(stats, version)
			.await
			.reports(Duration::from_millis(150))
			.await
	}

	/// A transport that exposes an RTT but no send-rate estimate must still report.
	///
	/// The two PROBE fields are independent, each using 0 for unknown, so discarding
	/// the whole message for want of a bitrate leaves a subscriber with no RTT at
	/// all. That is what pins a qmux viewer to its fallback jitter buffer.
	#[kio::test]
	async fn reports_rtt_without_a_bitrate() {
		let stats = crate::lite::test_transport::SinkStats::default().with_rtt(std::time::Duration::from_millis(40));
		let probes = probe_writes(stats).await;

		assert_eq!(probes.len(), 1, "expected exactly one report");
		assert_eq!(probes[0].rtt, Some(40));
		assert_eq!(probes[0].bitrate, None, "unknown bitrate, not a measured zero");
	}

	/// The mirror case: a send rate with no RTT still reports.
	#[kio::test]
	async fn reports_bitrate_without_an_rtt() {
		let stats = crate::lite::test_transport::SinkStats::default().with_send_rate(1_000_000);
		let probes = probe_writes(stats).await;

		assert_eq!(probes.len(), 1);
		assert_eq!(probes[0].bitrate, Some(1_000_000));
		assert_eq!(probes[0].rtt, None);
	}

	/// A transport measuring neither has nothing to say, and must not emit a report
	/// claiming two zeroes.
	#[kio::test]
	async fn reports_nothing_when_nothing_is_measurable() {
		let probes = probe_writes(crate::lite::test_transport::SinkStats::default()).await;
		assert!(probes.is_empty(), "expected no report, got {probes:?}");
	}

	/// Lite03's PROBE carries no RTT field, so an RTT-only report has nothing to
	/// say there. Sending one anyway would serialize as a bare "bitrate unknown"
	/// and, worse, fire again on every RTT movement.
	#[kio::test]
	async fn lite03_sends_nothing_for_an_rtt_only_report() {
		let stats = crate::lite::test_transport::SinkStats::default().with_rtt(std::time::Duration::from_millis(40));
		let probes = probe_writes_version(stats, Version::Lite03).await;
		assert!(probes.is_empty(), "expected no report on lite-03, got {probes:?}");
	}

	/// Lite03 still reports the half it can carry.
	#[kio::test]
	async fn lite03_reports_the_bitrate() {
		let stats = crate::lite::test_transport::SinkStats::default()
			.with_send_rate(1_000_000)
			.with_rtt(std::time::Duration::from_millis(40));
		let probes = probe_writes_version(stats, Version::Lite03).await;

		assert_eq!(probes.len(), 1);
		assert_eq!(probes[0].bitrate, Some(1_000_000));
		assert_eq!(probes[0].rtt, None, "lite-03 carries no RTT field");
	}

	/// A bitrate that becomes unknown is worth one report: the peer is still
	/// holding the last value we sent. But only one, however long the stream runs.
	#[kio::test]
	async fn a_bitrate_going_unknown_is_retracted_once() {
		let stats = crate::lite::test_transport::SinkStats::default().with_send_rate(1_000_000);
		let mut probing = Probing::new(stats, Version::Lite05).await;
		assert_eq!(probing.reports(Duration::from_millis(150)).await.len(), 1);

		// The transport stops measuring. Everything after this is unknown.
		probing.set(None, None);

		// Well past PROBE_MAX_AGE, so a stale-report timer would have fired repeatedly.
		let probes = probing.reports(Duration::from_secs(33)).await;
		assert_eq!(
			probes.len(),
			2,
			"the measurement then one retraction, not a repeating 'unknown': {probes:?}"
		);
		assert_eq!(probes[0].bitrate, Some(1_000_000));
		assert_eq!(probes[1].bitrate, None);
	}

	/// An estimate that never moves is refreshed once per PROBE_MAX_AGE, and costs
	/// nothing in between.
	#[kio::test]
	async fn an_unchanged_estimate_refreshes_at_max_age() {
		let stats = crate::lite::test_transport::SinkStats::default().with_send_rate(1_000_000);
		let mut probing = Probing::new(stats, Version::Lite05).await;
		assert_eq!(probing.reports(Duration::from_millis(9_900)).await.len(), 1);
		assert_eq!(probing.reports(Duration::from_millis(200)).await.len(), 2);
		assert_eq!(probing.reports(Duration::from_millis(9_800)).await.len(), 2);
		assert_eq!(probing.reports(Duration::from_millis(200)).await.len(), 3);
	}

	/// A move past PROBE_MAX_DELTA is reported as soon as the estimate changes.
	#[kio::test]
	async fn a_large_move_reports_at_once() {
		let stats = crate::lite::test_transport::SinkStats::default().with_send_rate(1_000_000);
		let mut probing = Probing::new(stats, Version::Lite05).await;
		assert_eq!(probing.reports(Duration::from_secs(1)).await.len(), 1);

		probing.set(Some(2_000_000), None);
		let probes = probing.reports(Duration::from_millis(1)).await;
		assert_eq!(probes.len(), 2);
		assert_eq!(probes[1].bitrate, Some(2_000_000));
	}

	/// A smaller move waits for the threshold to decay to it, even though the
	/// estimate does not change again.
	#[kio::test]
	async fn a_small_move_waits_for_the_threshold() {
		let stats = crate::lite::test_transport::SinkStats::default().with_send_rate(1_000_000);
		let mut probing = Probing::new(stats, Version::Lite05).await;
		assert_eq!(probing.reports(Duration::from_secs(1)).await.len(), 1);

		// 10% clears the threshold once it decays to 0.1: 10s - 0.4 * 9.9s = 6.04s
		// after the first report.
		probing.set(Some(1_100_000), None);
		assert_eq!(probing.reports(Duration::from_millis(4_990)).await.len(), 1);
		let probes = probing.reports(Duration::from_millis(100)).await;
		assert_eq!(probes.len(), 2);
		assert_eq!(probes[1].bitrate, Some(1_100_000));
	}

	/// Where a request waits when its requester leaves.
	#[derive(Clone, Copy, Debug)]
	enum Wait {
		/// On a route that would serve the broadcast on demand, but never does.
		Broadcast,
		/// On a broadcast whose track is requested but never answered.
		Track,
	}

	/// Drive `serve` until it ends, or everything parks: whether it ended.
	async fn drive<R: Request>(serve: &mut RequestServe<R>) -> bool {
		kio::clock::timeout(Duration::from_millis(1), kio::wait(|waiter| serve.poll(waiter)))
			.await
			.is_ok()
	}

	/// Serve an `R` request whose broadcast or track never resolves, then have the
	/// requester close its send side.
	async fn leave_request<R: Request>(version: Version, request: &impl Encode<Version>, wait: Wait, close: Close) {
		let case = format!("{} over {version:?}, waiting on the {wait:?}, {close:?}", R::KIND);
		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		// Held, never answered.
		let mut _route = None;
		let mut held = None;
		match wait {
			Wait::Broadcast => {
				_route = Some(
					origin
						.requests_route("room", crate::origin::Route::default(), None)
						.unwrap(),
				)
			}
			Wait::Track => {
				let broadcast = origin.create_broadcast("room").unwrap();
				let dynamic = broadcast.requests();
				broadcast.announce(Default::default()).unwrap();
				held = Some((broadcast, dynamic));
			}
		}

		let peer_setup = crate::lite::PeerSetup::new(false, crate::transport::Address::Setup);
		peer_setup.set(crate::lite::Setup::default());
		let (_, goaway) = crate::goaway::Handle::new(true);
		let publisher = Publisher::new(PublisherConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(ScriptedSession::new(Vec::new())),
			origin: origin.consume(),
			version,
			peer_setup,
			goaway,
			peer_hop: None,
			auth: crate::auth::Link::new(false),
			peer_grant: Default::default(),
			client: false,
			subscriptions: Default::default(),
			estimate: kio::Producer::default().weak(),
		});

		let mut script = Vec::new();
		request
			.encode(&mut crate::coding::Encoder::new(&mut script, version.into()), version)
			.unwrap();
		let session = ScriptedSession::new(script);
		let (send, recv) = kio::wait(|waiter| {
			<ScriptedSession as crate::transport::poll::Session>::poll_open_bi(&session, waiter, Default::default())
		})
		.await
		.unwrap();
		let stream = Stream::<Transport, Version> {
			writer: Writer::new(crate::transport::boxed::SendStream::new(send), version),
			reader: crate::coding::Reader::new(crate::transport::boxed::RecvStream::new(recv), version),
		};
		let mut serve = RequestServe::<R>::new(publisher.shared.clone(), stream);
		assert!(!drive(&mut serve).await, "{case}: nothing to answer with");

		// The lookup reached the track, which stays wanted while the requester waits.
		let track = held.as_mut().map(|(_, dynamic)| {
			let track = dynamic
				.requested_track()
				.now_or_never()
				.expect("track requested")
				.unwrap();
			assert!(track.demand().wanted().is_some(), "{case}: nobody wants the track");
			track
		});

		session.close(close);
		assert!(drive(&mut serve).await, "{case}: still serving");
		drop(serve);
		if let Some(track) = track {
			let mut demand = track.demand();
			let unused = kio::clock::timeout(
				Duration::from_millis(1),
				kio::wait(|waiter| crate::util::poll_unwanted(&mut demand, waiter)),
			);
			assert!(unused.await.is_ok(), "{case}: the track is still wanted");
		}

		// A subscription cut short is still a whole one, so it ends with our FIN. Any
		// other reply would read as whole if it ended short, so it resets.
		let graceful = R::GRACEFUL && matches!(close, Close::Fin);
		assert_eq!(
			session.log.resets().is_empty(),
			graceful,
			"{case}: {:?}",
			session.log.resets()
		);
	}

	/// A requester that closes its send direction, by FIN or reset, has terminated the
	/// transaction (moq-lite: closing the send direction ends the stream), however the
	/// answer is waiting. The lookup ends then, rather than pinning an upstream request
	/// nobody will read.
	#[kio::test]
	async fn a_request_ends_when_the_requester_leaves() {
		let broadcast = crate::Path::from_static("room");
		for version in [Version::Lite05, Version::Lite06, Version::Lite07] {
			for wait in [Wait::Broadcast, Wait::Track] {
				for close in [Close::Fin, Close::Reset] {
					let track = lite::Track {
						epoch: None,
						broadcast: broadcast.clone(),
						track: "video".into(),
					};
					leave_request::<TrackInfoServe>(version, &track, wait, close).await;

					let subscribe = lite::Subscribe {
						order: crate::track::Order::Descending,
						epoch: None,
						id: 0,
						broadcast: broadcast.clone(),
						track: "video".into(),
						priority: 0,
						max_delay: Duration::ZERO,
						ranges: vec![lite::Range::span(None, 0, None, None)],
					};
					leave_request::<SubscribeServe>(version, &subscribe, wait, close).await;

					let fetch = lite::Fetch {
						epoch: None,
						broadcast: broadcast.clone(),
						track: "video".into(),
						priority: 0,
						max_delay: None,
						group: 0,
						start_frame: 0,
						end_frame: None,
					};
					leave_request::<FetchServe>(version, &fetch, wait, close).await;
				}
			}
		}
	}

	/// A broadcast that overflows `Path::MAX_BYTES` under a deep root is refused on its own
	/// stream, never by closing the session.
	#[kio::test]
	async fn a_broadcast_too_long_under_the_root_is_refused_per_request() {
		for version in [Version::Lite05, Version::Lite06, Version::Lite07] {
			let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
			let root = "r".repeat(crate::Path::MAX_BYTES - 2);
			let scoped = origin
				.consume()
				.scope(root.as_str(), &crate::Patterns::from(crate::Pattern::all()))
				.unwrap();

			let peer_setup = crate::lite::PeerSetup::new(false, crate::transport::Address::Setup);
			peer_setup.set(crate::lite::Setup::default());
			let (_, goaway) = crate::goaway::Handle::new(true);
			let publisher = Publisher::new(PublisherConfig {
				estimate: kio::Producer::default().weak(),
				runtime: crate::time::sim(),
				session: crate::driver::Transport::new(ScriptedSession::new(Vec::new())),
				origin: scoped,
				version,
				peer_setup,
				goaway,
				peer_hop: None,
				subscriptions: Default::default(),
				auth: crate::auth::Link::new(false),
				peer_grant: Default::default(),
				client: false,
			});

			let mut script = Vec::new();
			lite::Subscribe {
				order: crate::track::Order::Descending,
				epoch: None,
				id: 0,
				broadcast: crate::Path::from_static("room"),
				track: "video".into(),
				priority: 0,
				max_delay: Duration::ZERO,
				ranges: vec![lite::Range::span(None, 0, None, None)],
			}
			.encode(&mut crate::coding::Encoder::new(&mut script, version.into()), version)
			.unwrap();
			let session = ScriptedSession::new(script);
			let (send, recv) = kio::wait(|waiter| {
				<ScriptedSession as crate::transport::poll::Session>::poll_open_bi(&session, waiter, Default::default())
			})
			.await
			.unwrap();
			let stream = Stream::<Transport, Version> {
				writer: Writer::new(crate::transport::boxed::SendStream::new(send), version),
				reader: crate::coding::Reader::new(crate::transport::boxed::RecvStream::new(recv), version),
			};
			let mut serve = RequestServe::<SubscribeServe>::new(publisher.shared.clone(), stream);
			let served = kio::clock::timeout(Duration::from_millis(1), kio::wait(|waiter| serve.poll(waiter)))
				.await
				.expect("the request was left waiting");
			assert!(served.is_ok(), "{version:?}: the session closed: {served:?}");
			assert!(
				!session.log.resets().is_empty(),
				"{version:?}: the request was not refused"
			);
		}
	}

	/// An answered TRACK stream holds its track until the requester closes its side, by
	/// FIN or reset, and the hold owes nothing a draining close would wait for. A requester
	/// that leaves while the reply is still blocked on flow control lets the track go too.
	#[kio::test]
	async fn an_answered_track_stream_holds_the_track_until_the_requester_closes() {
		for version in [Version::Lite05, Version::Lite06, Version::Lite07] {
			for close in [Close::Fin, Close::Reset] {
				for blocked in [false, true] {
					let case = format!("{version:?}, {close:?}, blocked {blocked}");
					let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
					let broadcast = origin.create_broadcast("room").unwrap();
					let track = broadcast.create_track("video", crate::track::Info::timed()).unwrap();
					broadcast.announce(Default::default()).unwrap();

					let peer_setup = crate::lite::PeerSetup::new(false, crate::transport::Address::Setup);
					peer_setup.set(crate::lite::Setup::default());
					let (_, goaway) = crate::goaway::Handle::new(true);
					let publisher = Publisher::new(PublisherConfig {
						estimate: kio::Producer::default().weak(),
						runtime: crate::time::sim(),
						session: crate::driver::Transport::new(ScriptedSession::new(Vec::new())),
						origin: origin.consume(),
						version,
						peer_setup,
						goaway,
						peer_hop: None,
						subscriptions: Default::default(),
						auth: crate::auth::Link::new(false),
						peer_grant: Default::default(),
						client: false,
					});

					let mut script = Vec::new();
					lite::Track {
						epoch: None,
						broadcast: crate::Path::from_static("room"),
						track: "video".into(),
					}
					.encode(&mut crate::coding::Encoder::new(&mut script, version.into()), version)
					.unwrap();
					let session = ScriptedSession::new(script);
					let (_, recv) = kio::wait(|waiter| {
						<ScriptedSession as crate::transport::poll::Session>::poll_open_bi(
							&session,
							waiter,
							Default::default(),
						)
					})
					.await
					.unwrap();
					// A closed gate is a reply blocked on flow control, never reopened.
					let gate = kio::Producer::new(!blocked);
					let stream = Stream::<Transport, Version> {
						writer: Writer::new(
							crate::transport::boxed::SendStream::new(SinkSend::gated(
								session.log.clone(),
								gate.consume(),
							)),
							version,
						),
						reader: crate::coding::Reader::new(crate::transport::boxed::RecvStream::new(recv), version),
					};
					let mut serve = RequestServe::<TrackInfoServe>::new(publisher.shared.clone(), stream);

					assert!(!drive(&mut serve).await, "{case}: ended before the requester closed");
					assert!(
						matches!(serve.state, RequestState::Hold { finished, .. } if finished != blocked),
						"{case}: TRACK_INFO was not answered as expected"
					);
					assert!(track.demand().wanted().is_some(), "{case}: the track was let go");
					assert_eq!(
						publisher.shared.owed.load(Ordering::Relaxed),
						usize::from(blocked),
						"{case}: only an undelivered reply owes"
					);

					session.close(close);
					assert!(drive(&mut serve).await, "{case}: still holding");
					let mut demand = track.demand();
					kio::clock::timeout(
						Duration::from_millis(1),
						kio::wait(|waiter| crate::util::poll_unwanted(&mut demand, waiter)),
					)
					.await
					.unwrap_or_else(|_| panic!("{case}: the track is still wanted"));
					// The incomplete reply is cancelled; a delivered one is not.
					assert_eq!(
						session.log.resets().is_empty(),
						!blocked,
						"{case}: {:?}",
						session.log.resets()
					);
				}
			}
		}
	}

	/// SUBSCRIBE_UPDATE is not a close: an update arriving while the subscription
	/// resolves keeps it waiting, and the latest applies once it starts. Its wanted group
	/// is state each update restates: past the largest it joins, and once that group
	/// exists the same value asks for nothing more.
	#[kio::test]
	async fn a_subscribe_update_is_not_a_close() {
		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let broadcast = origin.create_broadcast("room").unwrap();
		let mut dynamic = broadcast.requests();
		broadcast.announce(Default::default()).unwrap();

		let peer_setup = crate::lite::PeerSetup::new(false, crate::transport::Address::Setup);
		peer_setup.set(crate::lite::Setup::default());
		let (_, goaway) = crate::goaway::Handle::new(true);
		let version = Version::Lite07;
		let publisher = Publisher::new(PublisherConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(ScriptedSession::new(Vec::new())),
			origin: origin.consume(),
			version,
			peer_setup,
			goaway,
			peer_hop: None,
			auth: crate::auth::Link::new(false),
			peer_grant: Default::default(),
			client: false,
			subscriptions: Default::default(),
			estimate: kio::Producer::default().weak(),
		});

		let mut script = Vec::new();
		lite::Subscribe {
			order: crate::track::Order::Descending,
			epoch: None,
			id: 0,
			broadcast: crate::Path::from_static("room"),
			track: "video".into(),
			priority: 1,
			max_delay: Duration::ZERO,
			// A relay asks from its cached group, so the SUBSCRIBE is not a live-edge join.
			ranges: vec![lite::Range::span(Some(1), 0, None, None)],
		}
		.encode(&mut crate::coding::Encoder::new(&mut script, version.into()), version)
		.unwrap();
		let session = ScriptedSession::new(script);
		let (send, recv) = kio::wait(|waiter| {
			<ScriptedSession as crate::transport::poll::Session>::poll_open_bi(&session, waiter, Default::default())
		})
		.await
		.unwrap();
		let stream = Stream::<Transport, Version> {
			writer: Writer::new(crate::transport::boxed::SendStream::new(send), version),
			reader: crate::coding::Reader::new(crate::transport::boxed::RecvStream::new(recv), version),
		};
		let mut serve = RequestServe::<SubscribeServe>::new(publisher.shared.clone(), stream);
		assert!(!drive(&mut serve).await);

		let update = |wanted_group, priority| lite::SubscribeUpdate {
			wanted_group,
			order: crate::track::Order::Descending,
			priority,
			max_delay: Duration::ZERO,
			ranges: vec![lite::Range::span(Some(1), 0, None, None)],
		};
		session.push(&update(Some(2), 3).encode_bytes(version).unwrap());
		session.push(&update(Some(2), 7).encode_bytes(version).unwrap());
		assert!(!drive(&mut serve).await, "an update ended the subscription");

		// Resolve the track with a group over the ratio: the run starts at the latest
		// priority, and its restated wanted group joins.
		let request = dynamic
			.requested_track()
			.now_or_never()
			.expect("track requested")
			.unwrap();
		let track = request.accept(crate::track::Info::timed().with_refresh(1));
		let mut group = track.create_group(group::Info { sequence: 1 }).unwrap();
		for frame in [&b"key"[..], b"abc", b"d"] {
			group.write_frame(crate::Timestamp::ZERO, frame).unwrap();
		}
		group.finish().unwrap();
		assert!(!drive(&mut serve).await, "the subscription ended");
		let RequestState::Serve(SubscribeServe::Run(run)) = &serve.state else {
			panic!("the subscription is not running");
		};
		assert_eq!(run.steer_tx.read().priority, 7, "the update was lost");
		assert_eq!(track.demand().wanted(), Some(2), "the refresh request was lost");

		// The asked-for group arrives, over the ratio too. A restated wanted group is now
		// stale, so it is not another join.
		let mut group = track.create_group(group::Info { sequence: 2 }).unwrap();
		for frame in [&b"key"[..], b"abc", b"d"] {
			group.write_frame(crate::Timestamp::ZERO, frame).unwrap();
		}
		group.finish().unwrap();
		session.push(&update(Some(2), 5).encode_bytes(version).unwrap());
		assert!(!drive(&mut serve).await, "the subscription ended");
		let RequestState::Serve(SubscribeServe::Run(run)) = &serve.state else {
			panic!("the subscription is not running");
		};
		assert_eq!(run.steer_tx.read().priority, 5, "the update was lost");
		assert_eq!(track.demand().wanted(), Some(2), "a stale wanted group joined");
	}

	/// A lite-07 SUBSCRIBE to `room/video`, served until it runs.
	struct Running {
		serve: RequestServe<SubscribeServe>,
		session: ScriptedSession,
		track: track::Producer,
		_broadcast: crate::origin::Publication,
		_origin: crate::origin::Producer,
	}

	/// The SUBSCRIBE [`running`] sends, which an update restates to change nothing.
	fn running_update() -> lite::SubscribeUpdate {
		lite::SubscribeUpdate {
			wanted_group: None,
			order: crate::track::Order::Descending,
			priority: 0,
			max_delay: Duration::from_secs(3600),
			ranges: vec![lite::Range::span(None, 0, None, None)],
		}
	}

	async fn running() -> Running {
		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let broadcast = origin.create_broadcast("room").unwrap();
		let mut dynamic = broadcast.requests();
		broadcast.announce(Default::default()).unwrap();

		let peer_setup = crate::lite::PeerSetup::new(false, crate::transport::Address::Setup);
		peer_setup.set(crate::lite::Setup::default());
		let (_, goaway) = crate::goaway::Handle::new(true);
		let version = Version::Lite07;
		let publisher = Publisher::new(PublisherConfig {
			estimate: kio::Producer::default().weak(),
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(ScriptedSession::new(Vec::new())),
			origin: origin.consume(),
			version,
			peer_setup,
			goaway,
			peer_hop: None,
			subscriptions: Default::default(),
			auth: crate::auth::Link::new(false),
			peer_grant: Default::default(),
			client: false,
		});

		let mut script = Vec::new();
		lite::Subscribe {
			order: crate::track::Order::Descending,
			epoch: None,
			id: 0,
			broadcast: crate::Path::from_static("room"),
			track: "video".into(),
			priority: 0,
			max_delay: Duration::from_secs(3600),
			ranges: vec![lite::Range::span(None, 0, None, None)],
		}
		.encode(&mut crate::coding::Encoder::new(&mut script, version.into()), version)
		.unwrap();
		let session = ScriptedSession::new(script);
		let (send, recv) = kio::wait(|waiter| {
			<ScriptedSession as crate::transport::poll::Session>::poll_open_bi(&session, waiter, Default::default())
		})
		.await
		.unwrap();
		let stream = Stream::<Transport, Version> {
			writer: Writer::new(crate::transport::boxed::SendStream::new(send), version),
			reader: crate::coding::Reader::new(crate::transport::boxed::RecvStream::new(recv), version),
		};
		let mut serve = RequestServe::<SubscribeServe>::new(publisher.shared.clone(), stream);
		assert!(!drive(&mut serve).await);
		let track = dynamic
			.requested_track()
			.now_or_never()
			.expect("track requested")
			.unwrap()
			.accept(crate::track::Info::timed());
		assert!(!drive(&mut serve).await, "the subscription ended");
		assert!(
			matches!(serve.state, RequestState::Serve(SubscribeServe::Run(_))),
			"the subscription is not running"
		);
		Running {
			serve,
			session,
			track,
			_broadcast: broadcast,
			_origin: origin,
		}
	}

	/// The steer of a running subscription.
	fn steer(serve: &RequestServe<SubscribeServe>) -> &kio::Producer<Steer> {
		let RequestState::Serve(SubscribeServe::Run(run)) = &serve.state else {
			panic!("the subscription is not running");
		};
		&run.steer_tx
	}

	/// A subscription that always has another group ready yields within its budget, so a
	/// sibling (the transport's driver, say) runs between its polls. Without it, one poll
	/// served every group the producer kept appending and starved the rest of the runtime
	/// (a go publisher's 2.5 ms audio groups timed its session out).
	#[kio::test]
	async fn a_busy_subscription_yields_to_its_siblings() {
		/// `RequestServe`'s passes per poll; each serves at most one group.
		const BUDGET: u64 = 32;
		const GROUPS: u64 = 10_000;
		const BATCH: u64 = 1_000;

		type Task = Box<dyn FnMut(&kio::Waiter) -> Poll<()>>;

		let Running {
			mut serve,
			session,
			track,
			_broadcast,
			_origin,
		} = running().await;
		let RequestState::Serve(SubscribeServe::Run(run)) = &serve.state else {
			unreachable!()
		};
		let outcomes = run.ctx.outcomes.clone();

		let mut tasks: kio::Tasks<Task> = kio::Tasks::new();

		// The producer appends a batch of ready groups on each of its turns.
		// Keep the track open after the producer task finishes its last batch.
		let producer = track.clone();
		let mut appended = 0;
		tasks.push(Box::new(move |waiter: &kio::Waiter| {
			for _ in 0..BATCH.min(GROUPS - appended) {
				let mut group = producer.create_group(group::Info { sequence: appended }).unwrap();
				group
					.write_frame(crate::Timestamp::from_millis(appended).unwrap(), b"x".as_slice())
					.unwrap();
				group.finish().unwrap();
				appended += 1;
			}
			if appended == GROUPS {
				return Poll::Ready(());
			}
			waiter.waker().wake_by_ref();
			Poll::Pending
		}));

		// Stands in for the transport's driver: it only needs a turn.
		let turns = Arc::new(AtomicU64::new(0));
		let counted = turns.clone();
		tasks.push(Box::new(move |waiter: &kio::Waiter| {
			counted.fetch_add(1, Ordering::Relaxed);
			waiter.waker().wake_by_ref();
			Poll::Pending
		}));

		tasks.push(Box::new(move |waiter: &kio::Waiter| {
			serve.poll(waiter).map(|res| res.expect("serve"))
		}));

		let mut polls = 0;
		kio::wait(|owner| {
			if outcomes.opened.load(Ordering::Relaxed) >= GROUPS {
				return Poll::Ready(());
			}
			let before = outcomes.opened.load(Ordering::Relaxed);
			assert!(tasks.poll(owner).is_pending(), "the serve never ends");
			polls += 1;
			let served = outcomes.opened.load(Ordering::Relaxed) - before;
			assert!(served <= BUDGET, "one poll served {served} groups");
			assert_eq!(turns.load(Ordering::Relaxed), polls, "the sibling missed a turn");
			assert!(polls <= 2 * GROUPS, "the serve stalled");
			Poll::Pending
		})
		.await;
		assert!(
			session.log.resets().is_empty(),
			"a group was reset: {:?}",
			session.log.resets()
		);
	}

	/// A requester streaming SUBSCRIBE_UPDATEs gets at most a budget of them applied per
	/// poll, so its siblings run between them, yet every update still applies in order.
	#[kio::test]
	async fn a_flood_of_updates_yields_to_its_siblings() {
		/// `RequestServe`'s passes per poll; each applies at most one update.
		const BUDGET: usize = 32;
		const UPDATES: usize = 10_000;

		let Running {
			mut serve,
			session,
			_broadcast,
			_origin,
			track: _track,
		} = running().await;
		// Each differs from the one before, so none is skipped as a repeat, and only the
		// last carries `i8::MAX`, so seeing it means every update has applied.
		let priority = |i: usize| if i == UPDATES - 1 { i8::MAX } else { (i % 100) as i8 + 1 };
		let mut bytes = Vec::new();
		for i in 0..UPDATES {
			let update = lite::SubscribeUpdate {
				priority: priority(i),
				..running_update()
			};
			update
				.encode(
					&mut crate::coding::Encoder::new(&mut bytes, Version::Lite07.into()),
					Version::Lite07,
				)
				.unwrap();
		}
		session.push(&bytes);

		let mut polls = 0;
		kio::wait(|waiter| {
			assert!(serve.poll(waiter).is_pending(), "the subscription ended");
			polls += 1;
			if steer(&serve).read().priority == priority(UPDATES - 1) {
				return Poll::Ready(());
			}
			assert!(polls <= UPDATES, "the updates stalled");
			Poll::Pending
		})
		.await;
		assert!(polls * BUDGET >= UPDATES, "{UPDATES} updates applied in {polls} polls");
	}

	/// An update that restates the subscription wakes none of its groups.
	#[kio::test]
	async fn a_repeated_update_wakes_no_group() {
		#[derive(Default)]
		struct Count(std::sync::atomic::AtomicUsize);
		impl std::task::Wake for Count {
			fn wake(self: Arc<Self>) {
				self.0.fetch_add(1, Ordering::Relaxed);
			}
		}

		let Running {
			mut serve,
			session,
			_broadcast,
			_origin,
			track: _track,
		} = running().await;
		let steered = steer(&serve).consume();
		let wakes = Arc::new(Count::default());
		let waiter = kio::Waiter::new(std::task::Waker::from(wakes.clone()));
		assert!(steered.poll(&waiter, |_| Poll::<()>::Pending).is_pending());

		for _ in 0..100 {
			session.push(&running_update().encode_bytes(Version::Lite07).unwrap());
		}
		assert!(!drive(&mut serve).await, "the subscription ended");
		assert_eq!(wakes.0.load(Ordering::Relaxed), 0, "a repeated update woke the groups");

		let update = lite::SubscribeUpdate {
			priority: 1,
			..running_update()
		};
		session.push(&update.encode_bytes(Version::Lite07).unwrap());
		assert!(!drive(&mut serve).await, "the subscription ended");
		assert_eq!(
			wakes.0.load(Ordering::Relaxed),
			1,
			"a new priority did not reach the groups"
		);
	}

	/// A cold relay fills a bounded request from the group's head, so its cache does not
	/// store an undecodable tail merely because that was the downstream reader's floor.
	#[kio::test]
	async fn a_cold_bounded_fetch_requests_the_prefix() {
		let origin = crate::origin::Config::new(Hop::new(1).unwrap()).produce();
		let broadcast = origin.create_broadcast("room").unwrap();
		let track = broadcast.create_track("video", track::Info::timed()).unwrap();
		let dynamic = track.requests();
		let peer_setup = crate::lite::PeerSetup::new(false, crate::transport::Address::Setup);
		peer_setup.set(crate::lite::Setup::default());
		let (_, goaway) = crate::goaway::Handle::new(true);
		let publisher = Publisher::new(PublisherConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(ScriptedSession::new(Vec::new())),
			origin: origin.consume(),
			version: Version::Lite07,
			peer_setup,
			goaway,
			peer_hop: None,
			auth: crate::auth::Link::new(false),
			peer_grant: Default::default(),
			client: false,
			subscriptions: Default::default(),
			estimate: kio::Producer::default().weak(),
		});
		let run = FetchServe::start(
			&publisher.shared,
			lite::Fetch {
				epoch: None,
				broadcast: crate::Path::new("room").unwrap(),
				track: "video".into(),
				priority: 0,
				max_delay: None,
				group: 0,
				start_frame: 2,
				end_frame: Some(3),
			},
			broadcast.consume(),
		)
		.unwrap();
		let request = dynamic.requested_fill_group().await.unwrap();
		assert_eq!(request.frame_start(), 0, "a cold relay keeps the prefix");
		drop(run);
	}

	/// A FETCH whose own budget gives up on its group partway resets as too old: a FIN
	/// would tell the requester the prefix it got is the whole group.
	#[kio::test]
	async fn an_expired_fetch_resets_rather_than_finishing() {
		const VERSION: Version = Version::Lite07;
		let origin = crate::origin::Config::new(Hop::new(1).unwrap()).produce();
		let broadcast = origin.create_broadcast("room").unwrap();
		let track = broadcast.create_track("video", track::Info::timed()).unwrap();
		broadcast.announce(Default::default()).unwrap();
		let mut open = track.append_group().unwrap();
		open.write_frame(crate::Timestamp::ZERO, b"a".to_vec()).unwrap();

		let peer_setup = crate::lite::PeerSetup::new(false, crate::transport::Address::Setup);
		peer_setup.set(crate::lite::Setup::default());
		let (_, goaway) = crate::goaway::Handle::new(true);
		let publisher = Publisher::new(PublisherConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(ScriptedSession::new(Vec::new())),
			origin: origin.consume(),
			version: VERSION,
			peer_setup,
			goaway,
			peer_hop: None,
			auth: crate::auth::Link::new(false),
			peer_grant: Default::default(),
			client: false,
			subscriptions: Default::default(),
			estimate: kio::Producer::default().weak(),
		});
		let mut script = Vec::new();
		lite::Fetch {
			epoch: None,
			broadcast: crate::Path::from_static("room"),
			track: "video".into(),
			priority: 0,
			max_delay: Some(Duration::from_millis(500)),
			group: 0,
			start_frame: 0,
			end_frame: None,
		}
		.encode(&mut crate::coding::Encoder::new(&mut script, VERSION.into()), VERSION)
		.unwrap();
		let session = ScriptedSession::new(script);
		let (send, recv) = kio::wait(|waiter| {
			<ScriptedSession as crate::transport::poll::Session>::poll_open_bi(&session, waiter, Default::default())
		})
		.await
		.unwrap();
		let stream = Stream::<Transport, Version> {
			writer: Writer::new(crate::transport::boxed::SendStream::new(send), VERSION),
			reader: crate::coding::Reader::new(crate::transport::boxed::RecvStream::new(recv), VERSION),
		};
		let mut serve = RequestServe::<FetchServe>::new(publisher.shared.clone(), stream);
		assert!(!drive(&mut serve).await, "the open group holds the response");

		// The track runs a full second past the group's successor.
		for ms in [1000, 2000] {
			let mut group = track.append_group().unwrap();
			group
				.write_frame(crate::Timestamp::from_millis(ms).unwrap(), b"x".to_vec())
				.unwrap();
			group.finish().unwrap();
		}
		assert!(drive(&mut serve).await, "the budget ends the response");
		assert_eq!(session.log.resets(), vec![crate::StreamError::Old.to_code()]);
	}

	#[kio::test]
	async fn report_consumes_probe_targets_until_fin() {
		for version in [
			Version::Lite03,
			Version::Lite04,
			Version::Lite05,
			Version::Lite06,
			Version::Lite07,
		] {
			let target = lite::Probe {
				bitrate: Some(4_000_000),
				rtt: Some(20),
			};
			let session = ScriptedSession::new(target.encode_bytes(version).unwrap().to_vec())
				.with_stats(crate::lite::test_transport::SinkStats::default().with_send_rate(1_000_000));
			let log = session.log.clone();
			let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), version)
				.await
				.unwrap();
			let estimate = kio::Producer::default();
			let mut serve = probe_server(
				crate::driver::Transport::new(session.clone()),
				stream,
				version,
				&estimate,
			);
			assert!(
				serve.poll_probe(&kio::Waiter::noop()).is_pending(),
				"{version}: target ended Report"
			);
			assert!(log.resets().is_empty(), "{version}: valid target was reset");
			assert_eq!(
				decode_probes(&log.writes.lock().unwrap(), version)[0].bitrate,
				Some(1_000_000),
				"{version}: Report used the target as its estimate"
			);
			session.close(Close::Fin);
			assert!(
				matches!(serve.poll_probe(&kio::Waiter::noop()), Poll::Ready(Ok(()))),
				"{version}: buffered target hid FIN"
			);
		}
	}

	#[kio::test]
	async fn draining_probe_targets_yields() {
		const TARGETS: usize = 100;
		let version = Version::Lite07;
		let target = lite::Probe {
			bitrate: Some(4_000_000),
			rtt: Some(20),
		};
		let script = target.encode_bytes(version).unwrap().repeat(TARGETS);
		let session = ScriptedSession::new(script)
			.with_stats(crate::lite::test_transport::SinkStats::default().with_send_rate(1_000_000));
		let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), version)
			.await
			.unwrap();
		session.close(Close::Fin);
		let estimate = kio::Producer::default();
		let mut serve = probe_server(
			crate::driver::Transport::new(session.clone()),
			stream,
			version,
			&estimate,
		);
		// Without a budget the first poll drains every target and reaches the FIN.
		assert!(
			serve.poll_probe(&kio::Waiter::noop()).is_pending(),
			"one poll drained every buffered target"
		);
		assert!(
			(0..TARGETS).any(|_| serve.poll_probe(&kio::Waiter::noop()).is_ready()),
			"the FIN was never reached"
		);
	}

	#[kio::test]
	async fn malformed_subscribe_update_closes_after_request_setup() {
		use kio::Task;
		for version in [Version::Lite05, Version::Lite06, Version::Lite07] {
			let origin = crate::origin::Config::new(Hop::random()).produce();
			let broadcast = origin.create_broadcast("room").unwrap();
			let _dynamic = broadcast.requests();
			broadcast.announce(Default::default()).unwrap();
			let request = lite::Subscribe {
				order: crate::track::Order::Descending,
				epoch: None,
				id: 0,
				broadcast: crate::Path::from_static("room"),
				track: "video".into(),
				priority: 0,
				max_delay: Duration::ZERO,
				ranges: vec![lite::Range::span(None, 0, None, None)],
			};
			let session = ScriptedSession::new(request.encode_bytes(version).unwrap().to_vec());
			let log = session.log.clone();
			let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), version)
				.await
				.unwrap();
			let estimate = kio::Producer::default();
			let probe = probe_server(
				crate::driver::Transport::new(session.clone()),
				stream,
				version,
				&estimate,
			);
			probe.shared.peer_setup.set(lite::Setup::default());
			let mut shared = probe.shared;
			Arc::get_mut(&mut shared).expect("one owner").origin = origin.consume();
			let mut control = Control {
				shared: shared.clone(),
				runtime: crate::time::sim(),
				state: ControlState::Subscribe(RequestServe::new(shared, probe.stream.unwrap())),
			};
			assert!(
				control.poll(&kio::Waiter::noop()).is_pending(),
				"{version}: request did not wait for its track"
			);
			session.push(&[0]);
			assert!(
				control.poll(&kio::Waiter::noop()).is_ready(),
				"{version}: malformed update did not end control"
			);
			assert!(
				log.closes()
					.iter()
					.any(|(code, _)| *code == SessionError::ProtocolViolation.to_code()),
				"{version}: late decode error was swallowed"
			);
		}
	}

	/// A second listing of a broadcast on one session replaces the first, so a peer cannot
	/// multiply the listed set's memory and fanout by listing it again.
	#[kio::test]
	async fn a_newer_listing_of_a_broadcast_replaces_the_older() {
		use kio::Task;
		let version = Version::Lite07;
		let origin = crate::origin::Config::new(Hop::random()).produce();
		let broadcast = origin.create_broadcast("room").unwrap();
		broadcast.announce(Default::default()).unwrap();
		let request = lite::Tracks {
			broadcast: crate::Path::from_static("room"),
			epoch: None,
		};
		let request = request.encode_bytes(version).unwrap().to_vec();
		let session = ScriptedSession::per_stream(vec![request.clone(), request]);
		let log = session.log.clone();
		let older = Stream::open(&mut crate::driver::Transport::new(session.clone()), version)
			.await
			.unwrap();
		let newer = Stream::open(&mut crate::driver::Transport::new(session.clone()), version)
			.await
			.unwrap();
		let estimate = kio::Producer::default();
		let probe = probe_server(
			crate::driver::Transport::new(session.clone()),
			older,
			version,
			&estimate,
		);
		probe.shared.peer_setup.set(lite::Setup::default());
		let mut shared = probe.shared;
		Arc::get_mut(&mut shared).expect("one owner").origin = origin.consume();
		let control = |stream| Control {
			shared: shared.clone(),
			runtime: crate::time::sim(),
			state: ControlState::Tracks(RequestServe::new(shared.clone(), stream)),
		};

		// Whether the control is still serving after the runtime settles.
		async fn serving(control: &mut Control) -> bool {
			kio::clock::timeout(Duration::from_millis(1), kio::wait(|waiter| control.poll(waiter)))
				.await
				.is_err()
		}

		let mut older = control(probe.stream.unwrap());
		assert!(serving(&mut older).await, "the listing ended early");
		let mut newer = control(newer);
		assert!(serving(&mut newer).await, "the newer listing ended early");
		assert!(!serving(&mut older).await, "the older listing outlived its replacement");
		assert_eq!(log.resets(), vec![crate::StreamError::Cancel.to_code()]);
		assert!(log.closes().is_empty(), "a repeated listing closed the session");
		assert!(serving(&mut newer).await, "the newer listing ended");
	}

	#[kio::test]
	async fn a_producer_decode_error_stays_on_its_request() {
		use kio::Task;
		for version in [Version::Lite05, Version::Lite06, Version::Lite07] {
			let origin = crate::origin::Config::new(Hop::random()).produce();
			let broadcast = origin.create_broadcast("room").unwrap();
			let mut dynamic = broadcast.requests();
			broadcast.announce(Default::default()).unwrap();
			let request = lite::Subscribe {
				order: crate::track::Order::Descending,
				epoch: None,
				id: 0,
				broadcast: crate::Path::from_static("room"),
				track: "video".into(),
				priority: 0,
				max_delay: Duration::ZERO,
				ranges: vec![lite::Range::span(None, 0, None, None)],
			};
			let session = ScriptedSession::new(request.encode_bytes(version).unwrap().to_vec());
			let log = session.log.clone();
			let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), version)
				.await
				.unwrap();
			let estimate = kio::Producer::default();
			let probe = probe_server(
				crate::driver::Transport::new(session.clone()),
				stream,
				version,
				&estimate,
			);
			probe.shared.peer_setup.set(lite::Setup::default());
			let mut shared = probe.shared;
			Arc::get_mut(&mut shared).expect("one owner").origin = origin.consume();
			let mut control = Control {
				shared: shared.clone(),
				runtime: crate::time::sim(),
				state: ControlState::Subscribe(RequestServe::new(shared, probe.stream.unwrap())),
			};
			let pending = kio::clock::timeout(Duration::from_millis(1), kio::wait(|waiter| control.poll(waiter))).await;
			assert!(pending.is_err(), "{version}: request did not wait for its track");
			// The local producer fails with the same error type a malformed peer message
			// would; it is still not the peer's fault.
			let track = dynamic
				.requested_track()
				.now_or_never()
				.expect("track requested")
				.unwrap();
			track.reject(Error::Decode(crate::DecodeError::InvalidValue));
			let ended = kio::clock::timeout(Duration::from_millis(1), kio::wait(|waiter| control.poll(waiter))).await;
			assert!(ended.is_ok(), "{version}: the request did not end");
			assert!(
				log.closes().is_empty(),
				"{version}: a producer error closed the session"
			);
			assert!(!log.resets().is_empty(), "{version}: the request was not refused");
		}
	}

	#[kio::test]
	async fn goaway_roles_limits_and_transport_termination() {
		use kio::Task;
		for version in [Version::Lite04, Version::Lite05, Version::Lite06, Version::Lite07] {
			for uri in ["https://other.example/".to_owned(), "a".repeat(8193)] {
				let msg = lite::Goaway {
					uri: std::borrow::Cow::Owned(uri),
				};
				let session = ScriptedSession::new(msg.encode_bytes(version).unwrap().to_vec());
				let log = session.log.clone();
				let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), version)
					.await
					.unwrap();
				let estimate = kio::Producer::default();
				let probe = probe_server(crate::driver::Transport::new(session), stream, version, &estimate);
				let mut control = Control {
					shared: probe.shared,
					runtime: crate::time::sim(),
					state: ControlState::Goaway {
						stream: probe.stream.unwrap(),
					},
				};
				assert!(control.poll(&kio::Waiter::noop()).is_ready());
				assert!(
					log.closes()
						.iter()
						.any(|(code, _)| *code == SessionError::ProtocolViolation.to_code()),
					"{version}"
				);
			}
			for end in [Close::Fin, Close::Reset] {
				let session = ScriptedSession::new(Vec::new());
				session.close(end);
				let log = session.log.clone();
				let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), version)
					.await
					.unwrap();
				let estimate = kio::Producer::default();
				let probe = probe_server(crate::driver::Transport::new(session), stream, version, &estimate);
				let mut control = Control {
					shared: probe.shared,
					runtime: crate::time::sim(),
					state: ControlState::Goaway {
						stream: probe.stream.unwrap(),
					},
				};
				assert!(control.poll(&kio::Waiter::noop()).is_ready());
				assert!(log.closes().is_empty(), "{version}: {end:?} became a violation");
			}
		}
	}

	/// A publisher on a session with send groups.
	fn grouped_publisher(origin: origin::Consumer) -> (Publisher, crate::lite::test_transport::Log) {
		let peer_setup = crate::lite::PeerSetup::new(false, crate::transport::Address::Setup);
		peer_setup.set(crate::lite::Setup::default());
		let (_, goaway) = crate::goaway::Handle::new(true);
		let session = ScriptedSession::new(Vec::new()).with_groups();
		let log = session.log.clone();
		let publisher = Publisher::new(PublisherConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(session),
			origin,
			version: Version::Lite07,
			peer_setup,
			goaway,
			peer_hop: None,
			auth: crate::auth::Link::new(false),
			peer_grant: Default::default(),
			client: false,
			subscriptions: Default::default(),
			estimate: kio::Producer::default().weak(),
		});
		(publisher, log)
	}

	/// Subscribe to `broadcast`'s `track` at the subscriber `priority`, resolving the track
	/// with `info`, until the subscription runs.
	async fn subscribe_at(
		publisher: &Publisher,
		dynamic: &mut broadcast::Requests,
		broadcast: &'static str,
		track: &'static str,
		priority: i8,
		info: track::Info,
	) -> (RequestServe<SubscribeServe>, track::Producer) {
		let version = Version::Lite07;
		let msg = lite::Subscribe {
			order: crate::track::Order::Descending,
			epoch: None,
			id: 0,
			broadcast: crate::Path::new(broadcast).unwrap(),
			track: track.into(),
			priority,
			max_delay: Duration::ZERO,
			ranges: vec![lite::Range::span(None, 0, None, None)],
		};
		let mut script = Vec::new();
		msg.encode(&mut crate::coding::Encoder::new(&mut script, version.into()), version)
			.unwrap();
		let session = ScriptedSession::new(script);
		let (send, recv) = kio::wait(|waiter| {
			<ScriptedSession as crate::transport::poll::Session>::poll_open_bi(&session, waiter, Default::default())
		})
		.await
		.unwrap();
		let stream = Stream::<Transport, Version> {
			writer: Writer::new(crate::transport::boxed::SendStream::new(send), version),
			reader: crate::coding::Reader::new(crate::transport::boxed::RecvStream::new(recv), version),
		};
		let mut serve = RequestServe::<SubscribeServe>::new(publisher.shared.clone(), stream);
		assert!(!drive(&mut serve).await);
		let track = dynamic
			.requested_track()
			.now_or_never()
			.expect("track requested")
			.unwrap()
			.accept(info);
		assert!(!drive(&mut serve).await, "the subscription ended");
		assert!(
			matches!(serve.state, RequestState::Serve(SubscribeServe::Run(_))),
			"the subscription is not running"
		);
		(serve, track)
	}

	/// A subscription's send group ranks by the peer's subscriber priority first, and the
	/// publisher's track priority breaks a tie between two at one subscriber priority.
	#[kio::test]
	async fn send_group_ranks_by_the_subscriber_then_the_publisher() {
		let origin = crate::origin::Config::new(Hop::new(1).unwrap()).produce();
		let broadcast = origin.create_broadcast("a").unwrap();
		let mut dynamic = broadcast.requests();
		broadcast.announce(Default::default()).unwrap();
		let (publisher, log) = grouped_publisher(origin.consume());

		let video = track::Info::timed().with_priority(60);
		let audio = track::Info::timed().with_priority(80);
		let _video = subscribe_at(&publisher, &mut dynamic, "a", "video", 0, video).await;
		let _audio = subscribe_at(&publisher, &mut dynamic, "a", "audio", 0, audio).await;
		let _urgent = subscribe_at(&publisher, &mut dynamic, "a", "chat", 1, track::Info::timed()).await;

		let groups = log.groups();
		assert_eq!(groups.len(), 3);
		assert!(groups[1].1 > groups[0].1, "the publisher's priority breaks the tie");
		assert!(groups[2].1 > groups[1].1, "the subscriber's priority dominates");
	}
}
