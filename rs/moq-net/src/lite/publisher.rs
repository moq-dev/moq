use crate::{SessionError, announce, frame, group, origin, track};
use std::{
	collections::HashMap,
	sync::Arc,
	task::{Context, Poll, ready},
	time::Duration,
};

use web_transport_trait::Stats;

use crate::{
	AsPath, Error, Hop, Hops,
	coding::{Encode, Stream, Writer},
	lite::{
		self,
		priority::{Priority, PriorityHandle, PriorityQueue},
	},
};

use super::Version;

pub(super) struct PublisherConfig<S: crate::transport::poll::Session, R: crate::runtime::Runtime> {
	/// The runtime that arms the publisher's timers.
	pub runtime: R,
	pub session: S,
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
}

/// Context shared by every control-stream child.
struct Shared<S: crate::transport::poll::Session> {
	session: S,
	origin: origin::Consumer,
	self_origin: Hop,
	// The peer's SETUP, read for the origin id it declared. Used to serve the
	// peer from a source whose chain excludes them, keeping the data plane on
	// the same split-horizon rule as the announces we send them.
	peer_setup: super::PeerSetup,
	// The identity assigned to the peer by the caller (`Client::with_peer_hop`, or
	// the per-session default a server hands every request), standing in wherever the
	// peer declines to declare one. Backs both the announce filter and the serving
	// origin, so a peer that names itself nowhere on the wire is still split-horizoned.
	peer_hop: Option<Hop>,
	// The excluded origin handle, resolved once: the peer sends exactly one
	// SETUP, so its declared id never changes for the session.
	serving: std::sync::OnceLock<origin::Consumer>,
	priority: PriorityQueue,
	version: Version,
	goaway: crate::goaway::Protocol,
}

/// Largest millisecond duration every implementation can carry losslessly.
const MAX_SAFE_AGE_MS: u64 = (1_u64 << 53) - 1;

/// The budget to serve a peer with, given what its wire could tell us.
///
/// A version without the field decodes as [`Duration::ZERO`], which is
/// indistinguishable from a peer genuinely asking for the live edge. Serving that
/// as real time would discard backlog a legacy subscriber never declined, so fall
/// back to a window wide enough not to drop and leave enforcement to the receiver,
/// exactly as the IETF path does for the same reason.
fn serving_max_age(version: Version, requested: Duration) -> Duration {
	match version.carries_max_age() {
		true => requested,
		false => Duration::from_millis(MAX_SAFE_AGE_MS),
	}
}

/// Position a subscription's read cursor for the wire serving it.
///
/// On lite-06 there is nothing to do: `Consumer::subscribe` resolves the cursor from the
/// subscription itself, at the oldest group its own Max Age still considers fresh, floored
/// at the group it named.
///
/// Pre-06 wires are the exception: their drafts define an absent `Group Start` as the
/// latest group, so say so explicitly rather than letting the budget reach back. Lite03-05
/// carry a Max Age, but there it is a staleness tolerance only; Lite01/02 additionally get
/// an unbounded budget so nothing is dropped under them (see [`serving_max_age`]), which
/// must not read as a request to replay the whole cache on join.
fn position_cursor(track: &mut track::Subscriber, version: Version, start_group: Option<u64>) {
	if version.resolves_start() || start_group.is_some() {
		return;
	}

	if let Some(latest) = track.latest() {
		track.start_at(latest);
	}
}

impl<S: crate::transport::poll::Session> Shared<S> {
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
pub(super) struct Publisher<S: crate::transport::poll::Session, R: crate::runtime::Runtime> {
	shared: Arc<Shared<S>>,
	// Cloned into each control-stream child that arms timers (PROBE, announce linger).
	runtime: R,
	// A dedicated accept handle: the poll interface takes `&mut self`, and the
	// shared context stays behind the Arc for the per-stream children.
	accept: S,
	children: kio::Tasks<Control<S, R>>,
}

impl<S: crate::transport::poll::Session, R: crate::runtime::Runtime> Publisher<S, R> {
	pub fn new(config: PublisherConfig<S, R>) -> Self {
		// Identity stamped onto outbound announce hops. Derived from the
		// origin we're consuming so it matches the local relay identity
		// across every session, required for cross-session loop detection.
		let self_origin = *config.origin;
		let accept = config.session.clone();
		Self {
			shared: Arc::new(Shared {
				session: config.session,
				origin: config.origin,
				self_origin,
				peer_setup: config.peer_setup,
				peer_hop: config.peer_hop,
				serving: std::sync::OnceLock::new(),
				priority: Default::default(),
				version: config.version,
				goaway: config.goaway,
			}),
			runtime: config.runtime,
			accept,
			children: kio::Tasks::new(),
		}
	}
}

impl<S, R> Publisher<S, R>
where
	S: crate::transport::poll::Session,
	R: crate::runtime::Runtime,
{
	pub fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		let _ = self.children.poll(waiter);

		let mut cx = Context::from_waker(waiter.waker());
		loop {
			match Stream::poll_accept(&mut self.accept, self.shared.version, &mut cx) {
				Poll::Ready(Ok(stream)) => {
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
}

#[cfg(test)]
impl<S: crate::transport::poll::Session, R: crate::runtime::Runtime> Publisher<S, R> {
	/// Test shim: drive one announce-interest stream like the old `run_announce`.
	async fn run_announce(
		stream: &mut Stream<S, Version>,
		origin: &origin::Consumer,
		announced: &mut announce::Consumer,
		prefix: impl AsPath,
		self_origin: Hop,
		version: Version,
	) -> Result<(), Error> {
		let mut run = AnnounceRun::new(prefix.as_path().to_owned(), self_origin, version);
		kio::wait(|waiter| run.poll(stream, origin, announced, waiter)).await
	}
}

/// One accepted control stream, dispatched on its first varint.
struct Control<S: crate::transport::poll::Session, R: crate::runtime::Runtime> {
	shared: Arc<Shared<S>>,
	// Handed to the children that arm timers (PROBE, announce linger).
	runtime: R,
	state: ControlState<S, R>,
}

// A state machine's enum is its storage: one transient instance per stream, so the
// big variant is the working state, not padding held in bulk.
#[allow(clippy::large_enum_variant)]
enum ControlState<S: crate::transport::poll::Session, R: crate::runtime::Runtime> {
	/// Reading the stream's type.
	Start {
		stream: Stream<S, Version>,
	},
	Announce(AnnounceServe<S>),
	Subscribe(SubscribeServe<S>),
	Fetch(FetchServe<S>),
	TrackInfo(TrackInfoServe<S>),
	Probe(ProbeServe<S, R>),
	/// Decoding the peer's GOAWAY, surfaced through [`crate::Session::draining`].
	Goaway {
		stream: Stream<S, Version>,
	},
	Done,
}

impl<S: crate::transport::poll::Session, R: crate::runtime::Runtime> kio::Task for Control<S, R> {
	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		if let Err(err) = ready!(self.poll_serve(waiter)) {
			tracing::warn!(%err, "control stream error");
		}
		Poll::Ready(())
	}
}

impl<S: crate::transport::poll::Session, R: crate::runtime::Runtime> Control<S, R> {
	fn poll_serve(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		loop {
			match &mut self.state {
				ControlState::Start { stream } => {
					let mut cx = Context::from_waker(waiter.waker());
					let kind = ready!(stream.reader.poll_decode::<lite::ControlType>(&mut cx))?;

					let ControlState::Start { stream } = std::mem::replace(&mut self.state, ControlState::Done) else {
						unreachable!()
					};
					self.state = match kind {
						lite::ControlType::Announce => {
							ControlState::Announce(AnnounceServe::new(self.shared.clone(), stream))
						}
						lite::ControlType::Subscribe => {
							ControlState::Subscribe(SubscribeServe::new(self.shared.clone(), stream))
						}
						lite::ControlType::Fetch => ControlState::Fetch(FetchServe::new(self.shared.clone(), stream)?),
						lite::ControlType::Track => {
							ControlState::TrackInfo(TrackInfoServe::new(self.shared.clone(), stream)?)
						}
						lite::ControlType::Probe => {
							ControlState::Probe(ProbeServe::new(self.shared.clone(), self.runtime.clone(), stream))
						}
						lite::ControlType::Goaway => ControlState::Goaway { stream },
						lite::ControlType::Session => return Poll::Ready(Err(Error::UnexpectedStream)),
					};
				}
				ControlState::Announce(serve) => return serve.poll(waiter),
				ControlState::Subscribe(serve) => return serve.poll(waiter),
				ControlState::Fetch(serve) => return serve.poll(waiter),
				ControlState::TrackInfo(serve) => return serve.poll(waiter),
				ControlState::Probe(serve) => return serve.poll(waiter),
				ControlState::Goaway { stream } => {
					// A decode error propagates to the caller, which logs and continues: a
					// malformed GOAWAY must not tear down the session it is trying to drain.
					let mut cx = Context::from_waker(waiter.waker());
					let msg = ready!(stream.reader.poll_decode::<lite::Goaway>(&mut cx))?;
					tracing::info!(uri = %msg.uri, "received goaway");

					let uri = msg.uri.into_owned();
					let goaway = crate::goaway::Goaway {
						uri: uri.clone(),
						// moq-lite has no timeout field on the wire.
						timeout: None,
					};

					if let Err(err) = self.shared.goaway.record(goaway) {
						// A second Goaway stream is a protocol violation. The control loop only
						// logs per-stream errors, so close the session here rather than letting a
						// peer silently replace a redirect an observer may already be acting on.
						tracing::warn!(%uri, "duplicate GOAWAY received; closing session");
						self.shared
							.session
							.clone()
							.close(SessionError::from(&err).to_code(), &err.to_string());
						return Poll::Ready(Err(err));
					}
					return Poll::Ready(Ok(()));
				}
				ControlState::Done => return Poll::Ready(Ok(())),
			}
		}
	}
}

/// Serves one PROBE stream: periodic bandwidth estimates until the peer closes
/// its side.
struct ProbeServe<S: crate::transport::poll::Session, R: crate::runtime::Runtime> {
	shared: Arc<Shared<S>>,
	runtime: R,
	stream: Option<Stream<S, Version>>,
	last_sent: Option<(lite::Probe, crate::runtime::Instant)>,
	next_probe: crate::runtime::Deadline<R>,
}

impl<S: crate::transport::poll::Session, R: crate::runtime::Runtime> ProbeServe<S, R> {
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

	/// The bitrate change worth reporting, decaying to zero as the last report
	/// ages: a stale estimate is worth refreshing for a smaller move.
	fn bitrate_threshold(elapsed: Duration) -> f64 {
		let t = elapsed
			.as_secs_f64()
			.clamp(Self::PROBE_INTERVAL.as_secs_f64(), Self::PROBE_MAX_AGE.as_secs_f64());
		let range = Self::PROBE_MAX_AGE.as_secs_f64() - Self::PROBE_INTERVAL.as_secs_f64();
		Self::PROBE_MAX_DELTA * (Self::PROBE_MAX_AGE.as_secs_f64() - t) / range
	}

	fn new(shared: Arc<Shared<S>>, runtime: R, stream: Stream<S, Version>) -> Self {
		Self {
			shared,
			stream: Some(stream),
			last_sent: None,
			// Send the first probe immediately, then keep an anchored cadence.
			next_probe: crate::runtime::Deadline::at(&runtime, runtime.now()),
			runtime,
		}
	}

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		match ready!(self.poll_probe(waiter)) {
			Ok(()) => tracing::debug!("probe stream closed"),
			Err(err) => {
				tracing::warn!(%err, "probe stream error");
				self.stream.take().expect("stream present").writer.abort(&err);
			}
		}
		Poll::Ready(Ok(()))
	}

	fn poll_probe(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		let stream = self.stream.as_mut().expect("stream present");
		let mut cx = Context::from_waker(waiter.waker());

		loop {
			// Deliver the previous estimate before ticking out the next one.
			ready!(stream.writer.poll_flush(&mut cx))?;

			// Tick the probe interval, bailing as soon as the peer closes its side.
			if let Poll::Ready(res) = stream.reader.poll_closed(&mut cx) {
				return Poll::Ready(res);
			}
			ready!(self.next_probe.poll(waiter));
			let next = self
				.next_probe
				.deadline()
				.and_then(|at| at.checked_add(Self::PROBE_INTERVAL));
			self.next_probe.set(next);

			// The two fields are independent on the wire, each using 0 for unknown,
			// so a transport that exposes only one still has something to report.
			// Anything this version can't carry is dropped here rather than by the
			// encoder, so it reads as unknown to every check below.
			// Scoped so the borrowed stats handle is dropped before mutating the
			// stream below.
			let report = {
				let stats = self.shared.session.stats();
				lite::Probe {
					bitrate: stats.estimated_send_rate(),
					rtt: self
						.shared
						.version
						.has_probe_rtt()
						.then(|| stats.rtt().map(|d| d.as_millis() as u64))
						.flatten(),
				}
			};

			// Nothing left to report. Say so once if it retracts a value the peer is
			// still holding, then stay quiet rather than repeating "unknown" every
			// time the max age comes around.
			if report.bitrate.is_none() && report.rtt.is_none() {
				let retracts = self
					.last_sent
					.as_ref()
					.is_some_and(|(prev, _)| prev.bitrate.is_some() || prev.rtt.is_some());
				if !retracts {
					continue;
				}
			}

			let should_send = match &self.last_sent {
				None => true,
				Some((prev, at)) => {
					let elapsed = self.runtime.now().duration_since(*at);
					elapsed >= Self::PROBE_MAX_AGE
						|| Self::moved(prev.bitrate, report.bitrate, Self::bitrate_threshold(elapsed))
						|| Self::moved(prev.rtt, report.rtt, Self::PROBE_RTT_DELTA)
				}
			};

			if should_send {
				stream.writer.buffer(&report)?;
				self.last_sent = Some((report, self.runtime.now()));
			}
		}
	}
}

/// Serves one announce-interest stream: the initial set, then updates as routes,
/// demand, and the origin change.
struct AnnounceServe<S: crate::transport::poll::Session> {
	shared: Arc<Shared<S>>,
	stream: Option<Stream<S, Version>>,
	state: AnnounceState,
}

// A state machine's enum is its storage: one transient instance per stream, so the
// big variant is the working state, not padding held in bulk.
#[allow(clippy::large_enum_variant)]
enum AnnounceState {
	/// Reading the ANNOUNCE_REQUEST.
	Decode,
	/// Waiting on the peer's SETUP for the session-wide excluded origin
	/// (lite-05, whose wire carries no per-stream exclude_hop).
	ExcludeHop { prefix: crate::PathOwned },
	/// Streaming announce updates.
	Run {
		origin: origin::Consumer,
		announced: announce::Consumer,
		run: AnnounceRun,
	},
}

impl<S: crate::transport::poll::Session> AnnounceServe<S> {
	fn new(shared: Arc<Shared<S>>, stream: Stream<S, Version>) -> Self {
		Self {
			shared,
			stream: Some(stream),
			state: AnnounceState::Decode,
		}
	}

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		loop {
			match &mut self.state {
				AnnounceState::Decode => {
					let stream = self.stream.as_mut().expect("stream present");
					let mut cx = Context::from_waker(waiter.waker());
					let interest = ready!(stream.reader.poll_decode::<lite::AnnounceRequest>(&mut cx))?;
					let prefix = interest.prefix.to_owned();

					// The identity whose routes we filter out. Lite-04/05 carry it per
					// announce stream; lite-06+ reads the session-wide SETUP Hop
					// parameter, the same identity the subscribe path excludes. A peer that
					// declares nothing falls back to the identity the caller assigned it
					// (`with_peer_hop`), if any.
					let assigned = self.shared.peer_hop.map(|origin| origin.id()).unwrap_or(0);
					if self.shared.version.has_exclude_hop() {
						let exclude_hop = match interest.exclude_hop {
							0 => assigned,
							id => id,
						};
						self.start(prefix, exclude_hop);
					} else if self.shared.version.has_setup_stream() {
						self.state = AnnounceState::ExcludeHop { prefix };
					} else {
						self.start(prefix, assigned);
					}
				}
				AnnounceState::ExcludeHop { prefix } => {
					let assigned = self.shared.peer_hop.map(|origin| origin.id()).unwrap_or(0);
					let exclude_hop = ready!(self.shared.peer_setup.poll_hop(waiter))
						.map(|origin| origin.id())
						.unwrap_or(assigned);
					let prefix = prefix.clone();
					self.start(prefix, exclude_hop);
				}
				AnnounceState::Run { origin, announced, run } => {
					let stream = self.stream.as_mut().expect("stream present");
					let res = ready!(run.poll(stream, origin, announced, waiter));
					if let Err(err) = res {
						match &err {
							Error::Cancel | Error::Transport(_) => {
								tracing::debug!(prefix = %origin.absolute(&run.prefix), "announcing cancelled");
							}
							err => {
								tracing::warn!(%err, prefix = %origin.absolute(&run.prefix), "announcing error");
							}
						}
						self.stream.take().expect("stream present").writer.abort(&err);
					}
					return Poll::Ready(Ok(()));
				}
			}
		}
	}

	fn start(&mut self, prefix: crate::PathOwned, exclude_hop: u64) {
		// If the requested prefix is outside our scope (an empty origin, or a token
		// that doesn't grant it), we simply have nothing to announce. Respond with an
		// empty set and keep the stream open (the subscriber treats a FIN here as a
		// fatal stream close), rather than erroring, which would reset the stream.
		let origin = self
			.shared
			.origin
			.scope(&[prefix.as_path()])
			.unwrap_or_else(|| self.shared.origin.empty());
		// Register the split-horizon peer on the announce cursor too. The origin
		// model uses this exposure to park a reflected copy before it can replace
		// the source we are currently advertising to that peer.
		let origin = match Hop::new(exclude_hop) {
			Ok(peer) => origin.excluding(peer),
			Err(_) => origin,
		};
		let announced = origin.announced();
		let run = AnnounceRun::new(prefix, self.shared.self_origin, self.shared.version);
		self.state = AnnounceState::Run { origin, announced, run };
	}
}

/// The announce loop's state, minus the handles it borrows per poll so the test
/// shim can supply its own.
struct AnnounceRun {
	prefix: crate::PathOwned,
	self_origin: Hop,
	version: Version,
	// Lite06+: announce ids. Every `active` we send implicitly assigns the next
	// per-stream ordinal, and `ended` references the id instead of repeating the
	// path. Only announces that actually hit the wire get an id (filtered ones
	// were never seen by the peer).
	next_announce_id: u64,
	// The routes the peer currently holds, keyed by suffix. The value is the
	// announce id on versions that assign them.
	live: HashMap<crate::PathOwned, Option<u64>>,
	phase: AnnouncePhase,
}

enum AnnouncePhase {
	/// The version-specific initial burst has not been sent yet.
	Init,
	Running,
	/// The origin ended: FIN sent, waiting for the acknowledgement.
	Closing,
}

impl AnnounceRun {
	fn new(prefix: crate::PathOwned, self_origin: Hop, version: Version) -> Self {
		Self {
			prefix,
			self_origin,
			version,
			next_announce_id: 0,
			live: HashMap::new(),
			phase: AnnouncePhase::Init,
		}
	}

	/// The suffix an update travels under on this stream.
	fn suffix(&self, prefix: &crate::origin::Prefix) -> crate::PathOwned {
		prefix
			.as_path()
			.strip_prefix(&self.prefix)
			.expect("origin returned invalid prefix")
			.to_owned()
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
			true => route.cost.clamped(),
			false => crate::origin::Cost::UNKNOWN,
		};
		Some((hops, cost))
	}

	/// The next announce id, on versions that assign them.
	fn assign_id(&mut self) -> Option<u64> {
		if !self.version.has_announce_id() {
			return None;
		}
		let id = self.next_announce_id;
		self.next_announce_id += 1;
		Some(id)
	}

	/// Retract the peer's advertisement for `suffix`, if it holds one.
	fn retract<S: crate::transport::poll::Session>(
		&mut self,
		stream: &mut Stream<S, Version>,
		suffix: crate::PathOwned,
		absolute: &crate::Path,
	) -> Result<(), Error> {
		let Some(id) = self.live.remove(&suffix) else {
			// Filtered on the way out; the peer never saw it.
			return Ok(());
		};
		tracing::debug!(route = %absolute, "unannounce");
		match id {
			Some(id) => stream.writer.buffer(&lite::AnnounceBroadcast::EndedId { id })?,
			// An ended announce doesn't need hops; the receiver matches on path only.
			None => stream.writer.buffer(&lite::AnnounceBroadcast::Ended {
				suffix: suffix.as_path(),
				hops: Hops::new(),
			})?,
		}
		Ok(())
	}

	/// Buffer the version-specific initial burst: ANNOUNCE_INIT (Lite01/02) or
	/// ANNOUNCE_OK plus the initial actives (lite-05+).
	fn init<S: crate::transport::poll::Session>(
		&mut self,
		stream: &mut Stream<S, Version>,
		origin: &origin::Consumer,
		announced: &mut announce::Consumer,
	) -> Result<(), Error> {
		match self.version {
			Version::Lite01 | Version::Lite02 => {
				let mut init = Vec::new();

				// Send ANNOUNCE_INIT as the first message with all currently active routes.
				// We use `try_next()` to synchronously get the initial updates.
				while let Some(update) = announced.try_next() {
					let suffix = self.suffix(&update.prefix);
					let absolute = origin.absolute(update.prefix.as_path()).to_owned();

					if update.active {
						if self.outgoing(&update.route, &absolute.as_path()).is_none() {
							continue;
						}
						tracing::debug!(route = %absolute, "announce");
						if !init.contains(&suffix) {
							init.push(suffix);
						}
					} else {
						// A potential race: a just-announced route already retracted.
						tracing::debug!(route = %absolute, "unannounce");
						init.retain(|p| p != &suffix);
					}
				}

				let announce_init = lite::AnnounceInit { suffixes: init };
				stream.writer.buffer(&announce_init)?;
			}
			_ if self.version.has_announce_ok() => {
				// Drain the current active set synchronously (like the Lite01/02 path),
				// stashing suffix+hops so we can both COUNT them for AnnounceOk and re-send
				// them afterward. The receiver stamps our origin onto each hop chain, so we
				// forward the stored chain as-is (no self push here).
				let mut initial: Vec<(crate::PathOwned, Hops, crate::origin::Cost)> = Vec::new();
				while let Some(update) = announced.try_next() {
					let suffix = self.suffix(&update.prefix);
					let absolute = origin.absolute(update.prefix.as_path()).to_owned();

					if update.active {
						let Some((hops, cost)) = self.outgoing(&update.route, &absolute.as_path()) else {
							continue;
						};
						tracing::debug!(route = %absolute, "announce");
						initial.retain(|(s, ..)| s != &suffix);
						initial.push((suffix, hops, cost));
					} else {
						// A potential race: a just-announced route already retracted.
						tracing::debug!(route = %absolute, "unannounce");
						initial.retain(|(s, ..)| s != &suffix);
					}
				}

				// Report our origin id (stamped onto hops by the receiver, not us)
				// and the count of initial announces that follow immediately.
				let ok = lite::AnnounceOk {
					origin: self.self_origin,
					active: initial.len() as u64,
				};
				stream.writer.buffer(&ok)?;
				for (suffix, hops, cost) in initial {
					let id = self.assign_id();
					self.live.insert(suffix.clone(), id);
					stream.writer.buffer(&lite::AnnounceBroadcast::Active {
						suffix: suffix.as_path(),
						hops,
						cost,
					})?;
				}
			}
			_ => {
				// Lite03/Lite04: no announce init, no AnnounceOk.
			}
		}

		Ok(())
	}

	/// Stream updates as they arrive. Closure wins the race so a dead peer can't
	/// stall on a busy announce feed.
	fn poll<S: crate::transport::poll::Session>(
		&mut self,
		stream: &mut Stream<S, Version>,
		origin: &origin::Consumer,
		announced: &mut announce::Consumer,
		waiter: &kio::Waiter,
	) -> Poll<Result<(), Error>> {
		let mut cx = Context::from_waker(waiter.waker());

		if matches!(self.phase, AnnouncePhase::Init) {
			self.init(stream, origin, announced)?;
			self.phase = AnnouncePhase::Running;
		}

		loop {
			// Deliver the buffered updates before selecting more work.
			ready!(stream.writer.poll_flush(&mut cx))?;

			if matches!(self.phase, AnnouncePhase::Closing) {
				return stream.writer.poll_closed(&mut cx);
			}

			if let Poll::Ready(res) = stream.reader.poll_closed(&mut cx) {
				return Poll::Ready(res);
			}
			let Poll::Ready(next) = announced.poll_next(waiter) else {
				return Poll::Pending;
			};

			let Some(update) = next else {
				// The buffer is empty (flushed at the loop top), so FIN now and
				// wait for the acknowledgement.
				stream.writer.finish()?;
				self.phase = AnnouncePhase::Closing;
				continue;
			};

			let suffix = self.suffix(&update.prefix);
			let absolute = origin.absolute(update.prefix.as_path()).to_owned();

			if !update.active {
				self.retract(stream, suffix, &absolute.as_path())?;
				continue;
			}

			match self.outgoing(&update.route, &absolute.as_path()) {
				Some((hops, cost)) => match self.live.get(&suffix) {
					// A metadata update on a live advertisement: restart it in
					// place (lite-05 restarts via a duplicate ANNOUNCE).
					Some(&id) if lite::restart_supported(self.version) => {
						tracing::debug!(route = %absolute, "reannounce");
						match id {
							Some(id) => stream
								.writer
								.buffer(&lite::AnnounceBroadcast::Restart { id, hops, cost })?,
							None => stream.writer.buffer(&lite::AnnounceBroadcast::Active {
								suffix: suffix.as_path(),
								hops,
								cost,
							})?,
						}
					}
					// Pre-restart versions have no way to update a live
					// advertisement; the peer keeps the original chain.
					Some(_) => {}
					None => {
						tracing::debug!(route = %absolute, "announce");
						let id = self.assign_id();
						self.live.insert(suffix.clone(), id);
						stream.writer.buffer(&lite::AnnounceBroadcast::Active {
							suffix: suffix.as_path(),
							hops,
							cost,
						})?;
					}
				},
				// The chain must not be forwarded (reflected, or full): retract
				// whatever the peer holds.
				None => self.retract(stream, suffix, &absolute.as_path())?,
			}
		}
	}
}

/// Serves one TRACK stream: resolve the track, answer with its TRACK_INFO, FIN.
struct TrackInfoServe<S: crate::transport::poll::Session> {
	shared: Arc<Shared<S>>,
	stream: Option<Stream<S, Version>>,
	state: TrackInfoState,
	// Log context, filled in after the decode.
	absolute: crate::PathOwned,
	track: String,
}

enum TrackInfoState {
	Decode,
	/// Resolving the split-horizon origin (waits on the peer's SETUP).
	Hop {
		msg: lite::Track<'static>,
	},
	/// Resolving the broadcast (may wait on a dynamic handler).
	Request {
		msg: lite::Track<'static>,
		requesting: origin::Requesting,
	},
	/// Waiting for the track's info.
	Query {
		querying: track::Querying,
	},
	/// TRACK_INFO buffered: flush, FIN, and wait for the acknowledgement.
	Finish {
		finished: bool,
	},
}

impl<S: crate::transport::poll::Session> TrackInfoServe<S> {
	fn new(shared: Arc<Shared<S>>, stream: Stream<S, Version>) -> Result<Self, Error> {
		// The Track Stream is lite-05+ only.
		if !shared.version.has_track_stream() {
			return Err(Error::UnexpectedStream);
		}
		Ok(Self {
			shared,
			stream: Some(stream),
			state: TrackInfoState::Decode,
			absolute: Default::default(),
			track: Default::default(),
		})
	}

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		match ready!(self.poll_serve(waiter)) {
			Ok(()) => Poll::Ready(Ok(())),
			// A decode error propagates so the control loop logs it; anything past
			// the decode is answered on the stream instead.
			Err(err) if matches!(self.state, TrackInfoState::Decode) => Poll::Ready(Err(err)),
			Err(err) => {
				match &err {
					Error::Cancel | Error::Transport(_) => {
						tracing::debug!(broadcast = %self.absolute, track = %self.track, "track info cancelled")
					}
					err => {
						tracing::warn!(broadcast = %self.absolute, track = %self.track, %err, "track info error")
					}
				}
				self.stream.take().expect("stream present").writer.abort(&err);
				Poll::Ready(Ok(()))
			}
		}
	}

	fn poll_serve(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		loop {
			match &mut self.state {
				TrackInfoState::Decode => {
					let stream = self.stream.as_mut().expect("stream present");
					let mut cx = Context::from_waker(waiter.waker());
					let msg = ready!(stream.reader.poll_decode::<lite::Track>(&mut cx))?;
					self.absolute = self.shared.origin.absolute(&msg.broadcast).to_owned();
					self.track = msg.track.to_string();
					tracing::debug!(broadcast = %self.absolute, track = %self.track, "track info requested");
					self.state = TrackInfoState::Hop { msg };
				}
				TrackInfoState::Hop { .. } => {
					// The peer requested this exact path, so it has already seen an
					// announcement for it. `request_broadcast` resolves it immediately, or
					// falls back to an `origin::Dynamic` handler (as in SubscribeServe).
					let origin = ready!(self.shared.poll_serving_origin(waiter));
					let TrackInfoState::Hop { msg } = std::mem::replace(&mut self.state, TrackInfoState::Decode) else {
						unreachable!()
					};
					let requesting = origin.request_broadcast(&msg.broadcast).into_inner();
					self.state = TrackInfoState::Request { msg, requesting };
				}
				TrackInfoState::Request { msg, requesting } => {
					let broadcast = ready!(requesting.poll_ok(waiter))?;
					let querying = broadcast.track(&msg.track)?.info().into_inner();
					self.state = TrackInfoState::Query { querying };
				}
				TrackInfoState::Query { querying } => {
					let info = ready!(querying.poll_ok(waiter))?;

					// TRACK_INFO only flows on Lite05+ (the encode errors otherwise), where every
					// track is timed, so the model's timescale and retention bound go on the wire
					// verbatim.
					let stream = self.stream.as_mut().expect("stream present");
					stream.writer.buffer(&lite::TrackInfo {
						priority: info.priority,
						max_age: info.max_age,
						timescale: info.timescale,
					})?;
					self.state = TrackInfoState::Finish { finished: false };
				}
				TrackInfoState::Finish { finished } => {
					let stream = self.stream.as_mut().expect("stream present");
					let mut cx = Context::from_waker(waiter.waker());
					if !*finished {
						ready!(stream.writer.poll_flush(&mut cx))?;
						stream.writer.finish()?;
						*finished = true;
					}
					return stream.writer.poll_closed(&mut cx);
				}
			}
		}
	}
}

/// Serves one SUBSCRIBE stream: resolve the track, then stream its groups (and
/// best-effort datagrams) until the peer FINs or the track ends.
struct SubscribeServe<S: crate::transport::poll::Session> {
	shared: Arc<Shared<S>>,
	stream: Option<Stream<S, Version>>,
	state: SubscribeState<S>,
	// Log context, filled in after the decode.
	id: u64,
	absolute: crate::PathOwned,
	track: String,
}

enum SubscribeState<S: crate::transport::poll::Session> {
	Decode,
	/// Resolving the split-horizon origin (waits on the peer's SETUP).
	Hop {
		msg: lite::Subscribe<'static>,
	},
	/// Resolving the broadcast (may wait on a dynamic handler).
	Request {
		msg: lite::Subscribe<'static>,
		requesting: origin::Requesting,
	},
	/// Waiting for the model subscription to be confirmed.
	Confirm {
		msg: lite::Subscribe<'static>,
		subscribing: track::Subscribing,
	},
	/// Streaming groups and datagrams. Boxed: by far the largest state, and the enum
	/// is moved on every transition.
	Run(Box<TrackRun<S>>),
	/// The track finished: draining the in-flight group streams before the FIN.
	Drain {
		children: kio::Tasks<GroupServe<S>>,
	},
	/// FIN and wait for the acknowledgement.
	Finish {
		finished: bool,
	},
}

impl<S: crate::transport::poll::Session> SubscribeServe<S> {
	fn new(shared: Arc<Shared<S>>, stream: Stream<S, Version>) -> Self {
		Self {
			shared,
			stream: Some(stream),
			state: SubscribeState::Decode,
			id: 0,
			absolute: Default::default(),
			track: Default::default(),
		}
	}

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		match ready!(self.poll_serve(waiter)) {
			Ok(()) => {
				tracing::info!(id = self.id, broadcast = %self.absolute, track = %self.track, "subscribed complete");
				Poll::Ready(Ok(()))
			}
			// A decode error propagates so the control loop logs it.
			Err(err) if matches!(self.state, SubscribeState::Decode) => Poll::Ready(Err(err)),
			Err(err) => {
				match &err {
					// TODO better classify WebTransport errors.
					Error::Cancel | Error::Transport(_) => {
						tracing::info!(id = self.id, broadcast = %self.absolute, track = %self.track, "subscribed cancelled")
					}
					err => {
						tracing::warn!(id = self.id, broadcast = %self.absolute, track = %self.track, %err, "subscribed error")
					}
				}
				self.stream.take().expect("stream present").writer.abort(&err);
				Poll::Ready(Ok(()))
			}
		}
	}

	fn poll_serve(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		loop {
			match &mut self.state {
				SubscribeState::Decode => {
					let stream = self.stream.as_mut().expect("stream present");
					let mut cx = Context::from_waker(waiter.waker());
					let msg = ready!(stream.reader.poll_decode::<lite::Subscribe>(&mut cx))?;

					self.id = msg.id;
					self.absolute = self.shared.origin.absolute(&msg.broadcast).to_owned();
					self.track = msg.track.to_string();
					tracing::info!(id = self.id, broadcast = %self.absolute, track = %self.track, "subscribed started");

					self.state = SubscribeState::Hop { msg };
				}
				SubscribeState::Hop { .. } => {
					// We just received a subscribe for this exact path, so by definition the
					// peer has already seen an announcement for it. `request_broadcast`
					// resolves an announced broadcast immediately; if it isn't announced it
					// falls back to an `origin::Dynamic` handler (or resolves to an error
					// when there is none).
					let origin = ready!(self.shared.poll_serving_origin(waiter));
					let SubscribeState::Hop { msg } = std::mem::replace(&mut self.state, SubscribeState::Decode) else {
						unreachable!()
					};
					let requesting = origin.request_broadcast(&msg.broadcast).into_inner();
					self.state = SubscribeState::Request { msg, requesting };
				}
				SubscribeState::Request { requesting, .. } => {
					// Waits for the dynamic fallback if the broadcast wasn't announced;
					// resolves immediately otherwise (including an unroutable/dropped error).
					let broadcast = ready!(requesting.poll_ok(waiter))?;
					let SubscribeState::Request { msg, .. } =
						std::mem::replace(&mut self.state, SubscribeState::Decode)
					else {
						unreachable!()
					};

					let subscription = crate::track::Subscription {
						priority: msg.priority,
						max_age: serving_max_age(self.shared.version, msg.max_age),
						..Bounds::from(&msg).positions()
					};

					// One subscriber for the whole subscription: the run loop polls its
					// groups and its best-effort datagrams from this single cursor, so a
					// group-only or datagram-only track opens exactly one subscription (no
					// duplicate demand).
					let track_consumer = broadcast.track(&msg.track)?;
					let subscribing = track_consumer.subscribe(subscription).into_inner();
					self.state = SubscribeState::Confirm { msg, subscribing };
				}
				SubscribeState::Confirm { subscribing, .. } => {
					let track = ready!(subscribing.poll_ok(waiter))?;
					let SubscribeState::Confirm { msg, .. } =
						std::mem::replace(&mut self.state, SubscribeState::Decode)
					else {
						unreachable!()
					};
					let stream = self.stream.as_mut().expect("stream present");

					// Per-frame timestamps require a wire format that carries them. Lite05+
					// prefixes every frame with a zigzag-delta timestamp at the track's
					// timescale; older drafts have no wire field, so `None` here means
					// "don't emit the prefix" (the frames still carry timestamps in the
					// model, just not on this wire).
					let timescale = if self.shared.version.has_track_stream() {
						Some(track.info().timescale)
					} else {
						None
					};

					// Lite05+ accepts implicitly: no SUBSCRIBE_OK, the immutable properties
					// live in TRACK_INFO, and the resolved range arrives as
					// SUBSCRIBE_START/END from the run loop. Older drafts still acknowledge
					// with SUBSCRIBE_OK here.
					if !self.shared.version.has_track_stream() {
						let info = lite::SubscribeOk {
							priority: msg.priority,
							max_age: Duration::ZERO,
							start_group: None,
							end_group: None,
						};
						stream.writer.buffer(&lite::SubscribeResponse::Ok(info))?;
					}

					// Track-level subscriber priority. SUBSCRIBE_UPDATE messages broadcast
					// new values to the run loop (so future groups inherit the new priority)
					// and the in-flight group machines (so they update via
					// PriorityHandle::set_track).
					let track_priority_tx = kio::Producer::new(msg.priority);

					let sub = Subscription {
						session: self.shared.session.clone(),
						id: msg.id,
						track_name: Arc::from(track.name()),
						priority: self.shared.priority.clone(),
						track_priority: track_priority_tx.consume(),
						track_priority_seen: msg.priority,
						version: self.shared.version,
						timescale,
					};

					let run = TrackRun::new(sub, track, Bounds::from(&msg), track_priority_tx);
					self.state = SubscribeState::Run(Box::new(run));
				}
				SubscribeState::Run(run) => {
					let stream = self.stream.as_mut().expect("stream present");
					match ready!(run.poll(stream, waiter))? {
						// Peer FIN'd: they're done with this subscription. Drop any
						// in-flight group machines (don't drain) so half-sent groups get
						// cancelled rather than completed pointlessly.
						TrackEnd::PeerFin => self.state = SubscribeState::Finish { finished: false },
						// The live edge reached the boundary; SUBSCRIBE_END was already
						// sent (or the version predates the track stream). Drain in-flight
						// group machines, then FIN.
						TrackEnd::Finished => {
							let SubscribeState::Run(run) = std::mem::replace(&mut self.state, SubscribeState::Decode)
							else {
								unreachable!()
							};
							self.state = SubscribeState::Drain { children: run.children };
						}
					}
				}
				SubscribeState::Drain { children } => {
					ready!(children.poll(waiter));
					self.state = SubscribeState::Finish { finished: false };
				}
				SubscribeState::Finish { finished } => {
					let stream = self.stream.as_mut().expect("stream present");
					let mut cx = Context::from_waker(waiter.waker());
					if !*finished {
						ready!(stream.writer.poll_flush(&mut cx))?;
						stream.writer.finish()?;
						*finished = true;
					}
					return stream.writer.poll_closed(&mut cx);
				}
			}
		}
	}
}

/// Serves one FETCH stream: a single cached group, streamed in order, then FIN.
struct FetchServe<S: crate::transport::poll::Session> {
	shared: Arc<Shared<S>>,
	stream: Option<Stream<S, Version>>,
	state: FetchState,
	// Log context, filled in after the decode.
	absolute: crate::PathOwned,
	track: String,
	group: u64,
}

// A state machine's enum is its storage: one transient instance per stream, so the
// big variant is the working state, not padding held in bulk.
#[allow(clippy::large_enum_variant)]
enum FetchState {
	Decode,
	/// Resolving the split-horizon origin (waits on the peer's SETUP).
	Hop {
		msg: lite::Fetch<'static>,
	},
	/// Resolving the broadcast (may wait on a dynamic handler).
	Request {
		msg: lite::Fetch<'static>,
		requesting: origin::Requesting,
	},
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
		timescale: Option<crate::Timescale>,
		prev_ts: u64,
		frame: Option<frame::Consumer>,
		chunk: Option<bytes::Bytes>,
		batch: Box<frame::Buffer>,
		batch_pos: usize,
	},
	/// FIN and wait for the acknowledgement.
	Finish {
		finished: bool,
	},
}

impl<S: crate::transport::poll::Session> FetchServe<S> {
	fn new(shared: Arc<Shared<S>>, stream: Stream<S, Version>) -> Result<Self, Error> {
		// FETCH is lite-05+ only; older drafts have no dedicated FETCH stream.
		if !shared.version.has_track_stream() {
			return Err(Error::UnexpectedStream);
		}
		Ok(Self {
			shared,
			stream: Some(stream),
			state: FetchState::Decode,
			absolute: Default::default(),
			track: Default::default(),
			group: 0,
		})
	}

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		match ready!(self.poll_serve(waiter)) {
			Ok(()) => {
				tracing::info!(broadcast = %self.absolute, track = %self.track, group = %self.group, "fetch complete");
				Poll::Ready(Ok(()))
			}
			// A decode error propagates so the control loop logs it.
			Err(err) if matches!(self.state, FetchState::Decode) => Poll::Ready(Err(err)),
			Err(err) => {
				match &err {
					Error::Cancel | Error::Transport(_) => {
						tracing::info!(broadcast = %self.absolute, track = %self.track, group = %self.group, "fetch cancelled")
					}
					err => {
						tracing::warn!(broadcast = %self.absolute, track = %self.track, group = %self.group, %err, "fetch error")
					}
				}
				self.stream.take().expect("stream present").writer.abort(&err);
				Poll::Ready(Ok(()))
			}
		}
	}

	fn poll_serve(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		loop {
			match &mut self.state {
				FetchState::Decode => {
					let stream = self.stream.as_mut().expect("stream present");
					let mut cx = Context::from_waker(waiter.waker());
					let msg = ready!(stream.reader.poll_decode::<lite::Fetch>(&mut cx))?;

					self.absolute = self.shared.origin.absolute(&msg.broadcast).to_owned();
					self.track = msg.track.to_string();
					self.group = msg.group;
					tracing::info!(broadcast = %self.absolute, track = %self.track, group = %self.group, "fetch started");

					self.state = FetchState::Hop { msg };
				}
				FetchState::Hop { .. } => {
					// The peer fetched this exact path, so it has already seen an
					// announcement for it. `request_broadcast` resolves it immediately, or
					// falls back to an `origin::Dynamic` handler (as in SubscribeServe).
					let origin = ready!(self.shared.poll_serving_origin(waiter));
					let FetchState::Hop { msg } = std::mem::replace(&mut self.state, FetchState::Decode) else {
						unreachable!()
					};
					let requesting = origin.request_broadcast(&msg.broadcast).into_inner();
					self.state = FetchState::Request { msg, requesting };
				}
				FetchState::Request { requesting, .. } => {
					let broadcast = ready!(requesting.poll_ok(waiter))?;
					let FetchState::Request { msg, .. } = std::mem::replace(&mut self.state, FetchState::Decode) else {
						unreachable!()
					};
					let track = broadcast.track(&msg.track)?;
					let fetching = track
						.fetch_group(
							msg.group,
							group::Fetch {
								priority: msg.priority,
								frame_start: msg.start_frame,
								..Default::default()
							},
						)
						.into_inner();
					self.state = FetchState::Fetch { msg, fetching };
				}
				FetchState::Fetch { msg, fetching } => {
					let mut group = ready!(kio::Pollable::poll(fetching, waiter))?;

					// The response carries no header, so a short run is indistinguishable
					// from one that started elsewhere: only serve a range we can cover
					// exactly. `fetch_group` already positions the consumer, so this is a
					// belt-and-braces check on a promise the wire can't restate.
					if group.index() != msg.start_frame {
						return Poll::Ready(Err(Error::Lagged));
					}
					// The end is a serving cap only: the cached group runs to the end of
					// the group so it stays usable for anyone else (see
					// `group::Fetch::frame_start`).
					group.end_at(msg.end_frame);

					// FETCH is gated to lite-05+, which learned the track timescale via
					// TRACK_INFO.
					let timescale = if self.shared.version.has_track_stream() {
						Some(group.timescale())
					} else {
						None
					};

					self.state = FetchState::Serve {
						group,
						timescale,
						prev_ts: 0,
						frame: None,
						chunk: None,
						batch: Box::new(frame::Buffer::new()),
						batch_pos: 0,
					};
				}
				FetchState::Serve {
					group,
					timescale,
					prev_ts,
					frame,
					chunk,
					batch,
					batch_pos,
				} => {
					let stream = self.stream.as_mut().expect("stream present");
					let mut cx = Context::from_waker(waiter.waker());
					loop {
						ready!(stream.writer.poll_flush(&mut cx))?;
						if let Some(pending) = chunk {
							ready!(stream.writer.poll_write(&mut cx, pending))?;
							if !bytes::Buf::has_remaining(pending) {
								*chunk = None;
							}
						} else if let Some(pending) = frame {
							match ready!(pending.poll_read_chunk(waiter))? {
								Some(next) => *chunk = Some(next),
								None => *frame = None,
							}
						} else if *batch_pos < batch.len() {
							let batched = &mut batch.filled_mut()[*batch_pos];
							buffer_frame_info(
								&mut stream.writer,
								batched.timestamp,
								batched.payload.len() as u64,
								*timescale,
								prev_ts,
							)?;
							let payload = std::mem::take(&mut batched.payload);
							if !payload.is_empty() {
								*chunk = Some(payload);
							}
							*batch_pos += 1;
							group.keep_alive();
						} else {
							match group.poll_read_frames(waiter, batch) {
								Poll::Ready(Ok(count)) if count > 0 => {
									*batch_pos = 0;
									continue;
								}
								Poll::Ready(Ok(_)) => break,
								Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
								Poll::Pending => {}
							}
							match ready!(group.poll_next_frame(waiter))? {
								Some(next) => {
									buffer_frame_info(
										&mut stream.writer,
										next.timestamp,
										next.size,
										*timescale,
										prev_ts,
									)?;
									*frame = Some(next);
								}
								None => break,
							}
						}
					}
					self.state = FetchState::Finish { finished: false };
				}
				FetchState::Finish { finished } => {
					let stream = self.stream.as_mut().expect("stream present");
					let mut cx = Context::from_waker(waiter.waker());
					if !*finished {
						ready!(stream.writer.poll_flush(&mut cx))?;
						stream.writer.finish()?;
						*finished = true;
					}
					return stream.writer.poll_closed(&mut cx);
				}
			}
		}
	}
}

#[cfg(test)]
mod test {
	use super::*;
	use crate::{Timestamp, broadcast};

	fn track_producer(name: impl Into<Arc<str>>) -> track::Producer {
		track::Producer::new(Arc::new(broadcast::Info::default()), name, None)
	}

	/// A pre-06 wire is served from the live edge when it names no start: those drafts
	/// define an absent `Group Start` as the latest group, and Lite01/02 additionally get
	/// an unbounded budget (so nothing is dropped under them) that must not read as a
	/// request to replay the whole cache.
	#[test]
	fn a_pre06_wire_is_pinned_to_the_live_edge() {
		use futures::FutureExt;

		let mut producer = track_producer("test");
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
				track::Subscription::default().with_max_age(serving_max_age(version, std::time::Duration::ZERO)),
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

		let mut legacy = served(Version::Lite01);
		position_cursor(&mut legacy, Version::Lite01, None);
		assert_eq!(drain(&mut legacy), vec![2]);

		// Lite03-05 declare a budget, but their drafts define it as a staleness tolerance
		// and an absent start as the latest group, so they are pinned all the same.
		let mut tolerant =
			producer.subscribe(track::Subscription::default().with_max_age(std::time::Duration::from_secs(5)));
		position_cursor(&mut tolerant, Version::Lite05, None);
		assert_eq!(drain(&mut tolerant), vec![2]);

		// On lite-06 the declared budget is what resolves the start, so it stands.
		let mut declared =
			producer.subscribe(track::Subscription::default().with_max_age(std::time::Duration::from_secs(5)));
		position_cursor(&mut declared, Version::Lite06Wip, None);
		assert_eq!(drain(&mut declared), vec![0, 1, 2]);
	}

	/// The run_track contract behind SUBSCRIBE_OK's implicit drop: once the first
	/// served group resolves the start, the cursor floor rises to it, so a lower
	/// group arriving late (unordered delivery) is never served after the range
	/// was declared dropped.
	#[tokio::test]
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
				subscriber.start_at(group.sequence);
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

	#[tokio::test]
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

	#[tokio::test]
	async fn recv_next_reports_future_boundary_before_finished() {
		let mut producer = track_producer("test");
		let mut subscriber = producer.subscribe(None);

		// The last group is 6 (exclusive 7), but only group 5 has been produced so far.
		producer.create_group(group::Info { sequence: 5 }).unwrap();
		producer.finish_at(7).unwrap();

		// Group 5 is delivered first.
		match recv_next(&mut subscriber, false, true).await.unwrap() {
			Recv::Group(group) => assert_eq!(group.sequence, 5),
			_ => panic!("expected group 5"),
		}

		// With no more groups ready yet, the declared boundary surfaces even though the
		// track isn't finished (group 6 is still outstanding).
		match recv_next(&mut subscriber, false, true).await.unwrap() {
			Recv::Boundary(group) => assert_eq!(group, 7),
			_ => panic!("expected the future boundary"),
		}

		// The caller stops requesting the boundary once sent. The trailing group arrives,
		// then the track finishes.
		producer.create_group(group::Info { sequence: 6 }).unwrap();
		match recv_next(&mut subscriber, false, false).await.unwrap() {
			Recv::Group(group) => assert_eq!(group.sequence, 6),
			_ => panic!("expected group 6"),
		}
		match recv_next(&mut subscriber, false, false).await.unwrap() {
			Recv::Finished => {}
			_ => panic!("expected finished once the boundary is reached"),
		}
	}

	/// A relay can ingest back-to-back groups micro-reordered (the upstream leg
	/// sends newest-first). The older group is cached and in demand, so serving
	/// must still deliver it; a sequence cursor would skip it permanently.
	#[tokio::test]
	async fn recv_next_serves_late_arrival_after_newer_group() {
		use futures::FutureExt;

		let mut producer = track_producer("test");
		let mut subscriber = producer.subscribe(track::Subscription::default().with_max_age(Duration::from_secs(5)));

		producer.create_group(group::Info { sequence: 2 }).unwrap();
		match recv_next(&mut subscriber, false, false).await.unwrap() {
			Recv::Group(group) => assert_eq!(group.sequence, 2),
			_ => panic!("expected group 2"),
		}

		// Group 1 lands after group 2 was already served.
		producer.create_group(group::Info { sequence: 1 }).unwrap();
		match recv_next(&mut subscriber, false, false).now_or_never() {
			Some(Ok(Recv::Group(group))) => assert_eq!(group.sequence, 1),
			Some(_) => panic!("expected the late-arriving group"),
			None => panic!("the late-arriving group was skipped"),
		}

		// Staleness is the max age window's job, not arrival order's: the track
		// still finishes normally afterward.
		producer.finish_at(3).unwrap();
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

	/// The tokio-backed test runtime, matching the fake transport.
	type TestRuntime = crate::runtime::tokio_test::Tokio<SinkSession>;
	type TestPublisher = Publisher<SinkSession, TestRuntime>;

	const VERSION: Version = Version::Lite06Wip;

	/// The hops stamped on every harness route.
	fn pub_hops() -> Hops {
		Hops::try_from(vec![Hop::new(9).unwrap()]).unwrap()
	}

	/// A cursor over the captured announce-stream bytes, decoding messages
	/// incrementally so each test step asserts exactly what it caused.
	struct Wire {
		writes: Arc<Mutex<Vec<u8>>>,
		cursor: usize,
	}

	impl Wire {
		fn pending(&self) -> Vec<u8> {
			self.writes.lock().unwrap()[self.cursor..].to_vec()
		}

		/// Decode the AnnounceOk that opens the stream.
		fn take_ok(&mut self) -> lite::AnnounceOk {
			let buf = self.pending();
			let mut slice = &buf[..];
			let ok = lite::AnnounceOk::decode(&mut slice, VERSION).expect("announce ok");
			self.cursor += buf.len() - slice.len();
			ok
		}

		/// Decode every announce message written since the last call.
		fn take_announces(&mut self) -> Vec<lite::AnnounceBroadcast<'static>> {
			let buf = self.pending();
			let mut slice = &buf[..];
			let mut msgs = Vec::new();
			while !slice.is_empty() {
				msgs.push(own(
					lite::AnnounceBroadcast::decode(&mut slice, VERSION).expect("announce message")
				));
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

	/// Re-own a decoded message so it can outlive the decode buffer.
	fn own(msg: lite::AnnounceBroadcast<'_>) -> lite::AnnounceBroadcast<'static> {
		match msg {
			lite::AnnounceBroadcast::Active { suffix, hops, cost } => lite::AnnounceBroadcast::Active {
				suffix: suffix.to_owned(),
				hops,
				cost,
			},
			lite::AnnounceBroadcast::Ended { suffix, hops } => lite::AnnounceBroadcast::Ended {
				suffix: suffix.to_owned(),
				hops,
			},
			lite::AnnounceBroadcast::EndedId { id } => lite::AnnounceBroadcast::EndedId { id },
			lite::AnnounceBroadcast::Restart { id, hops, cost } => lite::AnnounceBroadcast::Restart { id, hops, cost },
		}
	}

	struct Harness {
		/// Held for the whole test: dropping the origin producer retracts every
		/// route under it, which would end the announce loop.
		origin: origin::Producer,
		/// The initial announcement; drop to retract, update to restart.
		announcement: crate::announce::Producer,
		wire: Wire,
		task: tokio::task::JoinHandle<Result<(), Error>>,
	}

	impl Harness {
		/// Assert the loop is quiet *and* still alive. A panicked announce task
		/// also writes nothing, so silence alone would pass for the wrong reason
		/// (`tokio::spawn` parks the panic in the handle until it's joined).
		fn assert_idle(&self) {
			self.wire.assert_quiet();
			assert!(!self.task.is_finished(), "the announce loop ended unexpectedly");
		}
	}

	async fn settle() {
		tokio::time::sleep(Duration::from_millis(1)).await;
	}

	/// Announce one route with cost 7 and run the announce loop against it.
	async fn harness() -> Harness {
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
		let mut stream = Stream::<SinkSession, Version> {
			writer: Writer::new(SinkSend::new(log), VERSION),
			reader: Reader::new(PendingRecv, VERSION),
		};
		let task = tokio::spawn(async move {
			let mut announced = consumer.announced();
			let self_origin = *consumer;
			TestPublisher::run_announce(&mut stream, &consumer, &mut announced, "", self_origin, VERSION).await
		});
		settle().await;

		let mut wire = Wire { writes, cursor: 0 };
		assert_eq!(wire.take_ok().active, 1, "expected one initial announce");
		match wire.take_announces().as_slice() {
			[lite::AnnounceBroadcast::Active { suffix, hops, cost }] => {
				assert_eq!(suffix.as_str(), "cam");
				assert_eq!(hops, &pub_hops());
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
	#[tokio::test(start_paused = true)]
	async fn announce_and_retract() {
		let mut h = harness().await;

		let late = h
			.origin
			.announce("mic", crate::origin::Route::default().with_hops(pub_hops()))
			.unwrap();
		settle().await;
		match h.wire.take_announces().as_slice() {
			[lite::AnnounceBroadcast::Active { suffix, .. }] => assert_eq!(suffix.as_str(), "mic"),
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
	#[tokio::test(start_paused = true)]
	async fn update_restarts_in_place() {
		let mut h = harness().await;

		h.announcement
			.update(crate::origin::Route::default().with_hops(pub_hops()).with_cost(3))
			.unwrap();
		settle().await;
		match h.wire.take_announces().as_slice() {
			[lite::AnnounceBroadcast::Restart { id: 0, hops, cost }] => {
				assert_eq!(hops, &pub_hops());
				assert_eq!(*cost, crate::origin::Cost::new(3));
			}
			other => panic!("expected a restart, got {other:?}"),
		}
		h.assert_idle();
	}

	/// An identical update never reaches the wire: the origin coalesces it away.
	#[tokio::test(start_paused = true)]
	async fn identical_update_is_quiet() {
		let h = harness().await;
		h.announcement
			.update(crate::origin::Route::default().with_hops(pub_hops()).with_cost(7))
			.unwrap();
		settle().await;
		h.assert_idle();
	}

	/// A route whose chain contains the excluded peer is invisible to that peer's
	/// announce stream (control-plane split horizon via the cursor).
	#[tokio::test(start_paused = true)]
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
		let mut stream = Stream::<SinkSession, Version> {
			writer: Writer::new(SinkSend::new(log), VERSION),
			reader: Reader::new(PendingRecv, VERSION),
		};
		let task = tokio::spawn(async move {
			let mut announced = consumer.announced();
			let self_origin = *consumer;
			TestPublisher::run_announce(&mut stream, &consumer, &mut announced, "", self_origin, VERSION).await
		});
		settle().await;

		let mut wire = Wire { writes, cursor: 0 };
		assert_eq!(wire.take_ok().active, 1, "only the clean route is announced");
		match wire.take_announces().as_slice() {
			[lite::AnnounceBroadcast::Active { suffix, .. }] => assert_eq!(suffix.as_str(), "local"),
			other => panic!("expected the clean announce, got {other:?}"),
		}
		task.abort();
	}

	/// A cost past the wire ceiling is clamped rather than rejected.
	#[tokio::test(start_paused = true)]
	async fn cost_clamps_to_the_wire_ceiling() {
		let mut h = harness().await;
		h.announcement
			.update(
				crate::origin::Route::default()
					.with_hops(pub_hops())
					.with_cost(u64::MAX),
			)
			.unwrap();
		settle().await;
		match h.wire.take_announces().as_slice() {
			[lite::AnnounceBroadcast::Restart { cost, .. }] => {
				assert_eq!(*cost, crate::origin::Cost::new(crate::origin::MAX_COST));
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
/// model layer (`group::Producer::create_frame`) already converted the timestamp
/// into the track timescale, so its raw value goes straight onto the wire. Mirrors
/// the decode in the subscriber's `run_group`.
fn buffer_frame_info<W: crate::transport::poll::SendStream>(
	writer: &mut Writer<W, Version>,
	timestamp: crate::Timestamp,
	size: u64,
	timescale: Option<crate::Timescale>,
	prev_ts: &mut u64,
) -> Result<(), Error> {
	if timescale.is_some() {
		buffer_zigzag_delta(writer, timestamp.value(), prev_ts)?;
	}
	writer.buffer(&size)?;
	Ok(())
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
	let zz = crate::coding::VarInt::from_zigzag(delta).map_err(crate::coding::EncodeError::from)?;
	writer.buffer(&zz)?;
	*prev = curr;
	Ok(())
}

/// What [`recv_next`] pulled from the one subscriber: the next group to serve, the next
/// best-effort datagram to forward, the track declaring its exclusive final sequence, or
/// the track finishing (the live edge having reached that boundary).
// A `group::Consumer` carries an inline frame prefetch, so the `Group` variant dwarfs the
// others. This is a transient, one-at-a-time return value, so the padding is never held in
// bulk; boxing would only add a per-group allocation.
#[allow(clippy::large_enum_variant)]
enum Recv {
	Group(group::Consumer),
	Datagram(crate::Datagram),
	Boundary(u64),
	Finished,
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
/// When `emit_boundary` is set, a declared-but-not-yet-reached final sequence surfaces as
/// [`Recv::Boundary`] in an idle moment (after groups and datagrams), so the caller can send
/// SUBSCRIBE_END as soon as the ending is known rather than waiting for the live edge to reach
/// it. The caller clears `emit_boundary` after the first boundary so it fires once.
fn poll_recv_next(
	track: &mut track::Subscriber,
	datagrams: bool,
	emit_boundary: bool,
	waiter: &kio::Waiter,
) -> Poll<Result<Recv, Error>> {
	{
		let mut groups_finished = false;
		match track.poll_recv_group(waiter) {
			Poll::Ready(Ok(Some(group))) => return Poll::Ready(Ok(Recv::Group(group))),
			Poll::Ready(Ok(None)) => groups_finished = true,
			Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
			Poll::Pending => {}
		}
		if datagrams {
			match track.poll_recv_datagram(waiter) {
				Poll::Ready(Ok(Some(datagram))) => return Poll::Ready(Ok(Recv::Datagram(datagram))),
				// Datagram side finished but groups are still paused/pending: keep waiting on groups.
				Poll::Ready(Ok(None)) => {}
				Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
				Poll::Pending => {}
			}
		}
		// No live data ready: report the boundary (if declared) before signalling Finished, so a
		// future boundary reaches the subscriber while the trailing groups are still in flight.
		if emit_boundary && let Poll::Ready(res) = track.poll_finished(waiter) {
			return Poll::Ready(res.map(Recv::Boundary));
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
	kio::wait(|waiter| poll_recv_next(track, datagrams, emit_boundary, waiter)).await
}

/// Bound `group` to what this subscription asked for, reporting whether it can be served
/// at all.
///
/// Frame bounds qualify the start and end group only; everything in between is served
/// whole. `false` means the group's head is missing and the subscriber never asked for a
/// partial group, so it must be skipped: a group is the unit of decodability, and only
/// the subscriber knows whether a partial one is any use to it.
fn position_group(group: &mut group::Consumer, start: Option<(u64, u64)>, end: Option<(u64, u64)>) -> bool {
	let expected = match start {
		Some((sequence, frame)) if sequence == group.sequence => frame,
		_ => 0,
	};

	// `start_at` clamps up to the first frame the group still holds, so landing higher
	// than asked means the frames below it are gone.
	group.start_at(expected);
	if group.index() != expected {
		return false;
	}

	if let Some((sequence, frame)) = end
		&& sequence == group.sequence
	{
		group.end_at(frame);
	}

	true
}

/// A subscription's requested delivery range, exactly as it arrived on the wire.
///
/// The frame bounds qualify the start and end group, so they only mean anything paired
/// with one; [`Self::start_frame`] / [`Self::end_frame`] hand back that pairing.
struct Bounds {
	start_group: Option<u64>,
	start_frame: u64,
	end_group: Option<u64>,
	end_frame: Option<u64>,
}

impl Bounds {
	/// The requested range as the model's half-open pair of [`track::Position`]s.
	///
	/// The wire carries the two halves of each bound separately and both ends
	/// inclusive; the model carries whole positions with an exclusive end. The
	/// [`Subscription`] builders own that conversion, so this goes through them
	/// rather than repeating it.
	fn positions(&self) -> crate::track::Subscription {
		let mut sub = crate::track::Subscription::default();
		if let Some(group) = self.start_group {
			sub = sub.with_start(track::Position {
				group,
				frame: self.start_frame,
			});
		}
		if let Some(group) = self.end_group {
			sub = sub.with_end(match self.end_frame {
				Some(frame) => track::Position::after(group, frame),
				None => track::Position::after_group(group),
			});
		}
		sub
	}

	/// The group to apply a frame offset to and the offset itself, or `None` when
	/// delivery starts on a group boundary.
	fn start_frame(&self) -> Option<(u64, u64)> {
		self.start_group
			.map(|group| (group, self.start_frame))
			.filter(|(_, frame)| *frame != 0)
	}

	/// The group to cap and the last frame to serve within it (inclusive).
	fn end_frame(&self) -> Option<(u64, u64)> {
		self.end_group.zip(self.end_frame)
	}
}

impl From<&lite::Subscribe<'_>> for Bounds {
	fn from(msg: &lite::Subscribe<'_>) -> Self {
		Self {
			start_group: msg.start_group,
			start_frame: msg.start_frame,
			end_group: msg.end_group,
			end_frame: msg.end_frame,
		}
	}
}

impl From<&lite::SubscribeUpdate> for Bounds {
	fn from(msg: &lite::SubscribeUpdate) -> Self {
		Self {
			start_group: msg.start_group,
			start_frame: msg.start_frame,
			end_group: msg.end_group,
			end_frame: msg.end_frame,
		}
	}
}

/// Shared per-subscription state for the publisher side. Cloned cheaply. Every
/// field is either small or already Arc-backed for each in-flight serve_group task
/// so each in-flight group reads the latest SUBSCRIBE_UPDATE priority via its own
/// consumer cursor.
#[derive(Clone)]
struct Subscription<S: crate::transport::poll::Session> {
	session: S,
	id: u64,
	track_name: Arc<str>,
	priority: PriorityQueue,
	track_priority: kio::Consumer<u8>,
	/// Last track priority observed by this clone, so a change only fires once.
	track_priority_seen: u8,
	version: Version,
	/// Negotiated timestamp scale for this track. `Some(_)` on lite-05+ after
	/// TRACK_INFO; used to validate per-frame timestamps before encoding.
	timescale: Option<crate::Timescale>,
}

impl<S: crate::transport::poll::Session> Subscription<S> {
	/// Send one datagram best-effort over a QUIC datagram (lite-05 §6.4).
	///
	/// The datagram is dropped (there is no group fallback) if the encoded body doesn't fit the
	/// transport's datagram limit or the send fails (congestion / no capacity right now).
	fn serve_datagram(&mut self, datagram: crate::Datagram) {
		let body = lite::Datagram {
			subscribe: self.id,
			sequence: datagram.sequence,
			// Already at the track timescale (normalized by the model producer).
			timestamp: datagram.timestamp.value(),
			payload: datagram.payload,
		};
		// has_datagrams is checked before this runs, so encoding never hits the version guard.
		let Ok(body) = body.encode_bytes(self.version) else {
			return;
		};

		let max = self.session.max_datagram_size();
		if body.len() > max {
			tracing::debug!(
				sequence = datagram.sequence,
				size = body.len(),
				max,
				"dropping datagram larger than the transport limit"
			);
			return;
		}

		let _ = self.session.send_datagram(&body);
	}

	/// Read the latest SUBSCRIBE_UPDATE track priority, marking it seen.
	fn track_priority_current(&mut self) -> u8 {
		self.track_priority_seen = *self.track_priority.read();
		self.track_priority_seen
	}

	/// Test shim: drive one group stream like the old `serve_group`, surfacing the
	/// error the machine otherwise swallows after aborting the stream.
	#[cfg(test)]
	async fn serve_group(
		self,
		sequence: u64,
		frame_start: u64,
		priority: PriorityHandle,
		group: group::Consumer,
	) -> Result<(), Error> {
		let mut serve = Box::new(GroupServe::new(self, sequence, frame_start, priority, group));
		kio::wait(move |waiter| serve.poll_serve(waiter)).await
	}
}

/// What ended a subscription's run loop.
enum TrackEnd {
	/// The peer FIN'd the subscribe stream: drop the in-flight group machines so
	/// half-sent groups get cancelled rather than completed pointlessly.
	PeerFin,
	/// The live edge reached the track's boundary: drain the in-flight group
	/// machines, then FIN.
	Finished,
}

/// A subscription's run loop: one subscriber cursor serving groups, datagrams,
/// and SUBSCRIBE_UPDATE messages, with an in-flight group machine per group.
struct TrackRun<S: crate::transport::poll::Session> {
	ctx: Subscription<S>,
	track: track::Subscriber,
	/// Broadcasts SUBSCRIBE_UPDATE priorities to the in-flight group machines.
	track_priority_tx: kio::Producer<u8>,
	// Frame bounds qualify the start and end group only; everything in between is
	// served whole. Each is `(group, frame)`, or `None` for no offset at all.
	start_frame: Option<(u64, u64)>,
	end_frame: Option<(u64, u64)>,
	// Lite05+ resolves the range on the Subscribe Stream itself: SUBSCRIBE_START
	// once the first group is known, SUBSCRIBE_END as soon as the track declares its
	// exclusive final sequence (which may be ahead of the live edge).
	emit_range: bool,
	start_sent: bool,
	end_sent: bool,
	// Serve datagrams off this same subscriber, but only on lite-05 over a
	// datagram-capable transport (qmux/WebSocket/TCP/UDS report size 0). No group
	// fallback: otherwise off.
	datagrams: bool,
	children: kio::Tasks<GroupServe<S>>,
}

impl<S: crate::transport::poll::Session> TrackRun<S> {
	fn new(
		ctx: Subscription<S>,
		mut track: track::Subscriber,
		bounds: Bounds,
		track_priority_tx: kio::Producer<u8>,
	) -> Self {
		position_cursor(&mut track, ctx.version, bounds.start_group);

		// Apply the initial cap from the original Subscribe. Subsequent updates
		// flow through the SUBSCRIBE_UPDATE arm below.
		track.end_at(bounds.end_group);

		let emit_range = ctx.version.has_track_stream();
		let datagrams = ctx.version.has_datagrams() && ctx.session.max_datagram_size() > 0;

		Self {
			start_frame: bounds.start_frame(),
			end_frame: bounds.end_frame(),
			ctx,
			track,
			track_priority_tx,
			emit_range,
			start_sent: false,
			end_sent: false,
			datagrams,
			children: kio::Tasks::new(),
		}
	}

	/// `end_group` is a serving cap, not a subscription terminator: groups with
	/// sequence > cap are held in the producer's cache until the subscriber raises
	/// the cap (or unsets it) via SUBSCRIBE_UPDATE, then served in order. Only a
	/// peer FIN actually ends the subscription. This is what lets relays pause an
	/// upstream subscription across consumer churn without tearing it down.
	fn poll(&mut self, stream: &mut Stream<S, Version>, waiter: &kio::Waiter) -> Poll<Result<TrackEnd, Error>> {
		let mut cx = Context::from_waker(waiter.waker());
		loop {
			// Deliver the buffered range messages before selecting more work.
			ready!(stream.writer.poll_flush(&mut cx))?;

			// Drive the in-flight group machines; completions just retire.
			let _ = self.children.poll(waiter);

			// Control first: SUBSCRIBE_UPDATE/FIN messages are rare, so they can't
			// starve the data path, while a deep group backlog polled first could
			// defer an unsubscribe or priority change indefinitely. Partial message
			// bytes persist in the reader's buffer across turns.
			if let Poll::Ready(upd) = stream.reader.poll_decode_maybe::<lite::SubscribeUpdate>(&mut cx) {
				let Some(upd) = upd? else {
					return Poll::Ready(Ok(TrackEnd::PeerFin));
				};
				if let Ok(mut value) = self.track_priority_tx.write() {
					*value = upd.priority;
				}
				// Feed the full update into the model subscriber so the producer's
				// aggregate reflects it (and a relay re-forwards it upstream).
				let bounds = Bounds::from(&upd);
				let _ = self.track.update(crate::track::Subscription {
					priority: upd.priority,
					max_age: serving_max_age(self.ctx.version, upd.max_age),
					..bounds.positions()
				});
				if let Some(start_group) = upd.start_group {
					self.track.start_at(start_group);
				}
				self.track.end_at(upd.end_group);
				self.start_frame = bounds.start_frame();
				self.end_frame = bounds.end_frame();
				continue;
			}

			// One cursor drives the whole subscription: poll the cap-aware arrival-order
			// group and, when enabled, the next best-effort datagram. Groups are polled
			// first so a datagram burst can't starve them; datagrams flow whenever no
			// group is ready (including while groups are parked above the cap).
			let emit_boundary = self.emit_range && !self.end_sent;
			if let Poll::Ready(res) = poll_recv_next(&mut self.track, self.datagrams, emit_boundary, waiter) {
				match res? {
					Recv::Group(mut group) => {
						let sequence = group.sequence;
						if !position_group(&mut group, self.start_frame, self.end_frame) {
							// Its head is gone, and this subscriber didn't ask for a
							// partial group. Skip it rather than open a stream that can
							// only be reset; the next servable group resolves the start.
							tracing::debug!(subscribe = self.ctx.id, track = %self.ctx.track_name, sequence, "skipping group with a missing head");
							continue;
						}
						if self.emit_range && !self.start_sent {
							self.start_sent = true;
							// Only the group: the subscriber derives the start frame from
							// its own request (see `lite::SubscribeStart`).
							stream
								.writer
								.buffer(&lite::SubscribeResponse::Start(lite::SubscribeStart {
									group: sequence,
								}))?;
							// SUBSCRIBE_OK is an implicit drop of everything below the
							// resolved start (the subscriber records it as a permanent
							// miss), so a lower group arriving late must not be served
							// after all. A widening SUBSCRIBE_UPDATE re-lowers the floor,
							// renegotiating the resolved start along with the demand.
							self.track.start_at(sequence);
						}

						let frame_start = group.index();
						tracing::debug!(subscribe = self.ctx.id, track = %self.ctx.track_name, sequence, "serving group");

						// Use the latest priority for new groups so SUBSCRIBE_UPDATE applies to them too.
						let current_priority = self.ctx.track_priority_current();
						// The subscribe id scopes the group tie-break: one queue serves every
						// subscription on the session, and only groups of the same one may be
						// ranked against each other by sequence.
						let handle = self
							.ctx
							.priority
							.insert(Priority::new(current_priority, self.ctx.id, sequence));
						self.children
							.push(GroupServe::new(self.ctx.clone(), sequence, frame_start, handle, group));
					}
					Recv::Datagram(datagram) => self.ctx.serve_datagram(datagram),
					Recv::Boundary(group) => {
						// The track declared its exclusive final sequence. Forward it now,
						// even if trailing groups (below `group`) are still in flight, then
						// keep serving them until the live edge reaches the boundary.
						self.end_sent = true;
						stream
							.writer
							.buffer(&lite::SubscribeResponse::End(lite::SubscribeEnd { group }))?;
					}
					Recv::Finished => return Poll::Ready(Ok(TrackEnd::Finished)),
				}
				continue;
			}

			return Poll::Pending;
		}
	}
}

/// Serves one group on its own unidirectional stream: the header, then every
/// frame, applying queue and SUBSCRIBE_UPDATE priority changes as they land.
struct GroupServe<S: crate::transport::poll::Session> {
	ctx: Subscription<S>,
	priority: PriorityHandle,
	group: group::Consumer,
	sequence: u64,
	frame_start: u64,
	// Lite05+ delta-encodes per-frame timestamps within the group. The first
	// frame's delta is absolute (against an implicit prev value of 0), every
	// subsequent delta is signed against the previous frame.
	prev_ts: u64,
	state: GroupState<S>,
}

// A state machine's enum is its storage: one transient instance per stream, so the
// big variant is the working state, not padding held in bulk.
#[allow(clippy::large_enum_variant)]
enum GroupState<S: crate::transport::poll::Session> {
	/// Waiting for stream credit on this machine's own session handle.
	Open,
	/// Streaming frames: the write buffer drains first, then the pending chunk,
	/// then the pending frame, then the next frame.
	Serve {
		writer: Writer<S::SendStream, Version>,
		frame: Option<frame::Consumer>,
		chunk: Option<bytes::Bytes>,
		batch: Box<frame::Buffer>,
		batch_pos: usize,
	},
	/// Every frame is written and the FIN sent: wait for the acknowledgement so a
	/// late cancel can still reset the stream.
	Closed {
		writer: Writer<S::SendStream, Version>,
	},
	Done,
}

impl<S: crate::transport::poll::Session> GroupServe<S> {
	fn new(
		ctx: Subscription<S>,
		sequence: u64,
		frame_start: u64,
		priority: PriorityHandle,
		group: group::Consumer,
	) -> Self {
		Self {
			ctx,
			priority,
			group,
			sequence,
			frame_start,
			prev_ts: 0,
			state: GroupState::Open,
		}
	}

	/// Serve the group, aborting the stream with the real reason (Old, Lagged,
	/// Evicted, ...) on failure so the subscriber can tell a truncated group from
	/// a routine cancel. Without this the Writer's Drop fallback would report
	/// every failure as Cancel.
	fn poll_serve(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		loop {
			match &mut self.state {
				GroupState::Open => {
					if self.group.poll_expired(waiter) {
						self.state = GroupState::Done;
						return Poll::Ready(Err(Error::Old));
					}
					let mut cx = Context::from_waker(waiter.waker());
					let stream = match ready!(self.ctx.session.poll_open_uni(&mut cx)) {
						Ok(stream) => stream,
						Err(err) => {
							self.state = GroupState::Done;
							return Poll::Ready(Err(Error::from_transport(err)));
						}
					};
					let mut writer = Writer::new(stream, self.ctx.version);
					writer.set_priority(self.priority.send_order());

					let msg = lite::Group {
						subscribe: self.ctx.id,
						sequence: self.sequence,
						frame_start: self.frame_start,
					};
					if let Err(err) = writer.buffer(&lite::DataType::Group).and_then(|()| writer.buffer(&msg)) {
						self.state = GroupState::Done;
						writer.abort(&err);
						return Poll::Ready(Err(err));
					}
					self.state = GroupState::Serve {
						writer,
						frame: None,
						chunk: None,
						batch: Box::new(frame::Buffer::new()),
						batch_pos: 0,
					};
				}
				GroupState::Serve {
					writer,
					frame,
					chunk,
					batch,
					batch_pos,
				} => {
					let mut cx = Context::from_waker(waiter.waker());

					// Queue and SUBSCRIBE_UPDATE priority changes apply on every pass,
					// whatever the write pipeline is blocked on. The rank is re-read as
					// a send order when handled, since the two conventions are inverted.
					while let Poll::Ready(rank) = self.priority.poll_next(waiter) {
						writer.set_priority(PriorityHandle::send_order_of(rank));
					}
					let seen = self.ctx.track_priority_seen;
					// A dropped producer just disables this arm, like the queue arm above.
					if let Poll::Ready(Ok(value)) = self.ctx.track_priority.poll(waiter, |value| {
						if **value != seen {
							Poll::Ready(**value)
						} else {
							Poll::Pending
						}
					}) {
						self.ctx.track_priority_seen = value;
						let rank = self.priority.set_track(value);
						writer.set_priority(PriorityHandle::send_order_of(rank));
					}

					let outcome = 'serve: {
						// The peer closing first cancels the group.
						if writer.poll_closed(&mut cx).is_ready() {
							break 'serve Err(Error::Cancel);
						}
						loop {
							match writer.poll_flush(&mut cx) {
								Poll::Ready(Ok(())) => {}
								Poll::Ready(Err(err)) => break 'serve Err(err),
								// Parking on the transport is the one stall the group cursor cannot
								// see, and the only place a served group applies the drift budget:
								// flow control must not pin a stream that has gone stale. `true`
								// because the transport still owns bytes the cursor has released.
								Poll::Pending => {
									if self.group.poll_expired_while_pending(waiter, true) {
										break 'serve Err(Error::Old);
									}
									return Poll::Pending;
								}
							}
							if let Some(pending) = chunk {
								match writer.poll_write(&mut cx, pending) {
									Poll::Ready(Ok(_)) => {
										if !bytes::Buf::has_remaining(pending) {
											*chunk = None;
										}
									}
									Poll::Ready(Err(err)) => break 'serve Err(err),
									// Parking on the transport is the one stall the group cursor cannot
									// see, and the only place a served group applies the drift budget:
									// flow control must not pin a stream that has gone stale. `true`
									// because the transport still owns bytes the cursor has released.
									Poll::Pending => {
										if self.group.poll_expired_while_pending(waiter, true) {
											break 'serve Err(Error::Old);
										}
										return Poll::Pending;
									}
								}
							} else if let Some(pending) = frame {
								match pending.poll_read_chunk(waiter) {
									Poll::Ready(Ok(Some(next))) => *chunk = Some(next),
									Poll::Ready(Ok(None)) => *frame = None,
									Poll::Ready(Err(err)) => break 'serve Err(err),
									Poll::Pending => return Poll::Pending,
								}
							} else if *batch_pos < batch.len() {
								let batched = &mut batch.filled_mut()[*batch_pos];
								let buffered = buffer_frame_info(
									writer,
									batched.timestamp,
									batched.payload.len() as u64,
									self.ctx.timescale,
									&mut self.prev_ts,
								);
								if let Err(err) = buffered {
									break 'serve Err(err);
								}
								let payload = std::mem::take(&mut batched.payload);
								if !payload.is_empty() {
									*chunk = Some(payload);
								}
								*batch_pos += 1;
								self.group.keep_alive();
							} else {
								match self.group.poll_read_frames(waiter, batch) {
									Poll::Ready(Ok(count)) if count > 0 => {
										*batch_pos = 0;
										continue;
									}
									Poll::Ready(Ok(_)) => break 'serve Ok(()),
									Poll::Ready(Err(err)) => break 'serve Err(err),
									Poll::Pending => {}
								}

								match self.group.poll_next_frame(waiter) {
									Poll::Ready(Ok(Some(next))) => {
										let buffered = buffer_frame_info(
											writer,
											next.timestamp,
											next.size,
											self.ctx.timescale,
											&mut self.prev_ts,
										);
										if let Err(err) = buffered {
											break 'serve Err(err);
										}
										*frame = Some(next);
									}
									Poll::Ready(Ok(None)) => break 'serve Ok(()),
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
					let mut cx = Context::from_waker(waiter.waker());
					// poll_close releases the stream on completion: the peer acknowledged
					// everything, so the Drop fallback must not reset the stream and
					// discard bytes still retransmitting.
					let res = ready!(writer.poll_close(&mut cx));
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

impl<S: crate::transport::poll::Session> kio::Task for GroupServe<S> {
	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		// The machine owns its outcome: the stream was aborted with the reason (or
		// reset by the writer's Drop), which is all the subscriber sees.
		ready!(self.poll_serve(waiter)).map(|()| ()).unwrap_or(());
		Poll::Ready(())
	}
}

/// A group that fails mid-stream must reset with its own error code. The subscriber uses
/// that code to tell a truncated group (Old, Lagged, Evicted) from a routine cancel, so a
/// blanket [`Error::Cancel`] from the writer's drop fallback loses the reason.
#[cfg(all(test, not(loom)))]
mod serve_group_test {
	use super::*;
	use crate::lite::test_transport::*;
	use crate::{Timestamp, broadcast};
	use futures::FutureExt;

	/// The wire's inclusive pair maps to the model's exclusive end.
	///
	/// The inverse of the subscriber's `WireBounds`, and the same off-by-one risk: a
	/// whole end group becomes the head of the next one, a capped frame becomes the head
	/// of the frame above it.
	#[test]
	fn bounds_convert_to_positions() {
		let whole = Bounds {
			start_group: None,
			start_frame: 0,
			end_group: Some(5),
			end_frame: None,
		};
		assert_eq!(whole.positions().end, Some(track::Position::group(6)));

		let capped = Bounds {
			end_frame: Some(2),
			..whole
		};
		assert_eq!(capped.positions().end, Some(track::Position { group: 5, frame: 3 }));

		let started = Bounds {
			start_group: Some(5),
			start_frame: 3,
			end_group: None,
			end_frame: None,
		};
		let positions = started.positions();
		assert_eq!(positions.start, Some(track::Position { group: 5, frame: 3 }));
		assert_eq!(positions.end, None);

		// A frame bound the peer sent without its group has nothing to count from, so it
		// cannot reach the model at all.
		let orphan = Bounds {
			start_group: None,
			start_frame: 3,
			end_group: None,
			end_frame: Some(7),
		};
		assert_eq!((orphan.positions().start, orphan.positions().end), (None, None));
	}

	/// A group whose head the publisher no longer holds is skipped, not served short:
	/// only a subscriber that asked for a partial group may receive one.
	#[test]
	fn position_group_skips_a_missing_head() {
		let mut track = track::Producer::new(Arc::new(broadcast::Info::default()), "video", None);
		let mut group = track.create_group(group::Info { sequence: 3 }).unwrap();
		group.start_at(5).unwrap();
		group.write_frame(Timestamp::ZERO, b"tail".to_vec()).unwrap();

		// Asked for whole groups, so the missing frames 0..5 make this unservable.
		let mut consumer = group.consume();
		assert!(!position_group(&mut consumer, None, None));

		// Asked for exactly where it starts: servable, and positioned there.
		let mut consumer = group.consume();
		assert!(position_group(&mut consumer, Some((3, 5)), None));
		assert_eq!(consumer.index(), 5);

		// Asked for an earlier frame than the group holds: still a hole, still skipped.
		let mut consumer = group.consume();
		assert!(!position_group(&mut consumer, Some((3, 2)), None));

		// The offset belongs to the start group only; a later group is served whole.
		let mut other = track.create_group(group::Info { sequence: 4 }).unwrap();
		other.write_frame(Timestamp::ZERO, b"whole".to_vec()).unwrap();
		let mut consumer = other.consume();
		assert!(position_group(&mut consumer, Some((3, 5)), None));
		assert_eq!(consumer.index(), 0);
	}

	/// The end bound caps the end group and leaves the others whole.
	#[test]
	fn position_group_caps_the_end_group() {
		let mut track = track::Producer::new(Arc::new(broadcast::Info::default()), "video", None);
		let mut group = track.create_group(group::Info { sequence: 7 }).unwrap();
		for i in 0..4u8 {
			group.write_frame(Timestamp::ZERO, vec![i]).unwrap();
		}
		group.finish().unwrap();

		let mut consumer = group.consume();
		assert!(position_group(&mut consumer, None, Some((7, 1))));
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
		let mut consumer = group.consume();
		assert!(position_group(&mut consumer, None, Some((8, 1))));
		for i in 0..4u8 {
			assert_eq!(
				consumer.read_frame().now_or_never().unwrap().unwrap().unwrap().payload[0],
				i
			);
		}
	}

	#[tokio::test]
	async fn resets_with_the_abort_code() {
		let log = Log::default();
		let session = SinkSession::new(log.clone());

		let track_priority = kio::Producer::new(0u8);
		let subscription = Subscription {
			session,
			id: 0,
			track_name: "test".into(),
			priority: PriorityQueue::default(),
			track_priority: track_priority.consume(),
			track_priority_seen: 0,
			version: Version::Lite06Wip,
			timescale: Some(crate::Timescale::default()),
		};

		let mut track = track::Producer::new(Arc::new(broadcast::Info::default()), "test", None);
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

	/// A subscription group keeps checking the max age while a transport write is
	/// flow-control blocked, so a stalled send cannot pin the stream indefinitely.
	#[tokio::test]
	async fn blocked_transport_write_expires_with_the_group() {
		tokio::time::pause();

		let gate = kio::Producer::new(false);
		let session = SinkSession::gated_uni(gate.consume());
		let log = session.log.clone();
		let track_priority = kio::Producer::new(0u8);
		let subscription = Subscription {
			session,
			id: 0,
			track_name: "test".into(),
			priority: PriorityQueue::default(),
			track_priority: track_priority.consume(),
			track_priority_seen: 0,
			version: Version::Lite06Wip,
			timescale: Some(crate::Timescale::default()),
		};

		let mut track = track::Producer::new(Arc::new(broadcast::Info::default()), "test", None);
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

		tokio::time::advance(Duration::from_secs(1)).await;
		let mut edge = track.append_group().unwrap();
		edge.write_frame(Timestamp::from_millis(1000).unwrap(), b"edge".as_slice())
			.unwrap();
		edge.finish().unwrap();

		assert!(matches!(serve.await, Err(Error::Old)));
		assert_eq!(log.resets(), vec![crate::StreamError::Old.to_code()]);
	}

	/// The final payload remains guarded after its frame has advanced the group cursor.
	#[tokio::test]
	async fn blocked_final_transport_chunk_expires_with_the_group() {
		tokio::time::pause();

		let gate = kio::Producer::new(true);
		let session = SinkSession::gated_uni(gate.consume());
		let log = session.log.clone();
		let track_priority = kio::Producer::new(0u8);
		let subscription = Subscription {
			session,
			id: 0,
			track_name: "test".into(),
			priority: PriorityQueue::default(),
			track_priority: track_priority.consume(),
			track_priority_seen: 0,
			version: Version::Lite06Wip,
			timescale: Some(crate::Timescale::default()),
		};

		let mut track = track::Producer::new(Arc::new(broadcast::Info::default()), "test", None);
		let mut subscriber = track.subscribe(None);
		let mut old = track.append_group().unwrap();
		let mut frame = old
			.create_frame(frame::Info {
				timestamp: Timestamp::ZERO,
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
		frame.finish().unwrap();
		old.finish().unwrap();
		assert!(
			futures::poll!(serve.as_mut()).is_pending(),
			"the final byte is transport-blocked"
		);

		tokio::time::advance(Duration::from_secs(1)).await;
		let mut edge = track.append_group().unwrap();
		edge.write_frame(Timestamp::from_millis(1000).unwrap(), b"edge".as_slice())
			.unwrap();
		edge.finish().unwrap();

		assert!(matches!(serve.await, Err(Error::Old)));
		assert_eq!(log.resets(), vec![crate::StreamError::Old.to_code()]);
	}

	/// A subscription group keeps checking the max age while transport stream credit is
	/// exhausted, so returning credit is reserved for content that is still live.
	#[tokio::test]
	async fn blocked_transport_open_expires_with_the_group() {
		tokio::time::pause();

		let gate = kio::Producer::new(false);
		let session = SinkSession::gated_open_uni(gate.consume());
		let track_priority = kio::Producer::new(0u8);
		let subscription = Subscription {
			session,
			id: 0,
			track_name: "test".into(),
			priority: PriorityQueue::default(),
			track_priority: track_priority.consume(),
			track_priority_seen: 0,
			version: Version::Lite06Wip,
			timescale: Some(crate::Timescale::default()),
		};

		let mut track = track::Producer::new(Arc::new(broadcast::Info::default()), "test", None);
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

		tokio::time::advance(Duration::from_secs(1)).await;
		let mut edge = track.append_group().unwrap();
		edge.write_frame(Timestamp::from_millis(1000).unwrap(), b"edge".as_slice())
			.unwrap();
		edge.finish().unwrap();

		assert!(matches!(serve.await, Err(Error::Old)));
	}

	/// Lite01/02 have no max age field, so a SUBSCRIBE from one decodes as
	/// `Duration::ZERO`. Serving that as a real-time budget would hold every legacy
	/// peer to the live edge and discard backlog it never declined, so those versions
	/// get a non-dropping window and leave enforcement to the receiver.
	///
	/// These are the two most-preferred negotiated versions, so this is the common
	/// wire, not an edge case.
	#[test]
	fn a_version_without_the_field_serves_a_non_dropping_budget() {
		for version in [Version::Lite01, Version::Lite02] {
			let max_age = serving_max_age(version, Duration::ZERO);
			assert!(
				max_age >= Duration::from_secs(86_400),
				"{version:?} must not be served as real time: {max_age:?}"
			);
		}

		// A version that does carry it is taken at its word, zero included.
		assert_eq!(serving_max_age(Version::Lite05, Duration::ZERO), Duration::ZERO);
		assert_eq!(
			serving_max_age(Version::Lite05, Duration::from_secs(3)),
			Duration::from_secs(3)
		);
	}

	/// A group that completes cleanly must not reset at all. The completion path
	/// releases the stream via `poll_close`; leaving the writer to drop after
	/// `finish()` would fire the Drop fallback and tack a spurious Cancel reset
	/// onto a stream the peer already acknowledged.
	#[tokio::test]
	async fn completed_group_does_not_reset() {
		let log = Log::default();
		let session = SinkSession::new(log.clone());

		let track_priority = kio::Producer::new(0u8);
		let subscription = Subscription {
			session,
			id: 0,
			track_name: "test".into(),
			priority: PriorityQueue::default(),
			track_priority: track_priority.consume(),
			track_priority_seen: 0,
			version: Version::Lite06Wip,
			timescale: Some(crate::Timescale::default()),
		};

		let mut track = track::Producer::new(Arc::new(broadcast::Info::default()), "test", None);
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
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::lite::test_transport::SinkSession;
	use crate::model::ProduceTest;

	/// The tokio-backed test runtime, matching the fake transport.
	type TestRuntime = crate::runtime::tokio_test::Tokio<SinkSession>;

	/// A peer that declares no origin in its SETUP is split-horizoned by the identity
	/// the caller assigned it, on the data plane and not just the announce filter.
	/// Otherwise it can subscribe its way back to content that already flowed through
	/// it, which is the loop the announce filter exists to prevent.
	#[tokio::test(start_paused = true)]
	async fn serving_origin_falls_back_to_the_assigned_identity() {
		let assigned = crate::Hop::new(777).unwrap();
		let upstream = crate::Hop::new(778).unwrap();
		let origin = crate::origin::Info::new(crate::Hop::new(1).unwrap()).produce();

		let mut echoed_hops = Hops::new();
		echoed_hops.push(assigned).unwrap();
		let (_echoed, _echoed_server) = origin
			.announce_served("echoed", crate::origin::Route::default().with_hops(echoed_hops))
			.unwrap();

		let mut local_hops = Hops::new();
		local_hops.push(upstream).unwrap();
		let (_local, _local_server) = origin
			.announce_served("local", crate::origin::Route::default().with_hops(local_hops))
			.unwrap();

		// A SETUP that declares no origin of its own, so only the assigned one applies.
		let peer_setup = crate::lite::PeerSetup::default();
		peer_setup.set(crate::lite::Setup::default());
		let (_, goaway) = crate::goaway::Handle::new(true);

		let publisher = Publisher::new(PublisherConfig {
			runtime: TestRuntime::new(),
			session: SinkSession::new(Default::default()),
			origin: origin.consume(),
			version: Version::Lite06Wip,
			peer_setup,
			goaway,
			peer_hop: Some(assigned),
		});

		let serving = kio::wait(|waiter| publisher.shared.poll_serving_origin(waiter)).await;
		use futures::FutureExt;
		assert!(
			serving.request_broadcast("echoed/x").now_or_never().unwrap().is_err(),
			"served the peer its own route"
		);
		assert!(
			serving.request_broadcast("local/x").now_or_never().is_none(),
			"withheld an independent route"
		);
	}

	/// Lite01/02 send the initial active set as ANNOUNCE_INIT. It must apply the
	/// same per-peer route selection as the live loop: a broadcast whose only
	/// route flows through the excluded hop (here the peer's assigned identity,
	/// `Client::with_peer_hop`) is filtered from the initial set too.
	#[tokio::test]
	async fn announce_init_applies_route_selection() {
		let assigned = crate::Hop::new(777).unwrap();
		let clean_publisher = crate::Hop::new(778).unwrap();
		let self_origin = crate::Hop::new(1).unwrap();
		let origin = crate::origin::Info::new(self_origin).produce();

		let mut tainted_hops = Hops::new();
		tainted_hops.push(assigned).unwrap();
		let _tainted = origin
			.announce("echoed", crate::origin::Route::default().with_hops(tainted_hops))
			.unwrap();

		let mut clean_hops = Hops::new();
		clean_hops.push(clean_publisher).unwrap();
		let _clean = origin
			.announce("local", crate::origin::Route::default().with_hops(clean_hops))
			.unwrap();

		let gate = kio::Producer::new(true);
		let session = SinkSession::gated_bi(gate.consume());
		let log = session.log.clone();
		let mut stream = Stream::open(&mut session.clone(), Version::Lite01).await.unwrap();

		let consumer = origin.consume().excluding(assigned);
		let mut announced = consumer.announced();
		let mut run = std::pin::pin!(Publisher::<SinkSession, TestRuntime>::run_announce(
			&mut stream,
			&consumer,
			&mut announced,
			crate::Path::new(""),
			self_origin,
			Version::Lite01,
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

	/// Decode the PROBE messages the publisher wrote. The publisher only replies on
	/// a stream the subscriber opened, so there is no leading ControlType here.
	fn decode_probes(bytes: &[u8]) -> Vec<lite::Probe> {
		decode_probes_version(bytes, Version::Lite05)
	}

	fn decode_probes_version(bytes: &[u8], version: Version) -> Vec<lite::Probe> {
		use crate::coding::Decode as _;
		let mut slice = bytes;
		let mut out = Vec::new();
		while bytes::Buf::remaining(&slice) > 0 {
			out.push(lite::Probe::decode(&mut slice, version).unwrap());
		}
		out
	}

	/// Build the poll-based PROBE server around a stream opened by the test peer.
	fn probe_server(
		session: SinkSession,
		stream: Stream<SinkSession, Version>,
		version: Version,
	) -> ProbeServe<SinkSession, TestRuntime> {
		let origin = Hop::random().produce();
		let (_, goaway) = crate::goaway::Handle::new(true);
		let publisher = Publisher::new(PublisherConfig {
			runtime: TestRuntime::new(),
			session,
			origin: origin.consume(),
			version,
			peer_setup: crate::lite::PeerSetup::default(),
			goaway,
			peer_hop: None,
		});
		ProbeServe::new(publisher.shared.clone(), publisher.runtime.clone(), stream)
	}

	/// Drive the PROBE state machine against a transport reporting `stats`, and
	/// return whatever it wrote before parking.
	async fn probe_writes(stats: crate::lite::test_transport::SinkStats) -> Vec<lite::Probe> {
		probe_writes_version(stats, Version::Lite05).await
	}

	/// As above, on a specific negotiated version.
	async fn probe_writes_version(stats: crate::lite::test_transport::SinkStats, version: Version) -> Vec<lite::Probe> {
		let gate = kio::Producer::new(true);
		let mut session = SinkSession::gated_bi(gate.consume()).with_stats(stats);
		let log = session.log.clone();
		let stream = Stream::open(&mut session, version).await.unwrap();

		let mut server = probe_server(session, stream, version);
		let mut run = std::pin::pin!(kio::wait(|waiter| server.poll_probe(waiter)));
		// The loop reports on a 100ms cadence, so let the first tick land before
		// reading what it wrote. It parks on the next tick either way.
		assert!(futures::poll!(run.as_mut()).is_pending());
		tokio::time::sleep(Duration::from_millis(150)).await;
		assert!(futures::poll!(run.as_mut()).is_pending());

		let writes = log.writes.lock().unwrap().clone();
		decode_probes_version(&writes, version)
	}

	/// A transport that exposes an RTT but no send-rate estimate must still report.
	///
	/// The two PROBE fields are independent, each using 0 for unknown, so discarding
	/// the whole message for want of a bitrate leaves a subscriber with no RTT at
	/// all. That is what pins a qmux viewer to its fallback jitter buffer.
	#[tokio::test(start_paused = true)]
	async fn reports_rtt_without_a_bitrate() {
		let stats = crate::lite::test_transport::SinkStats::default().with_rtt(std::time::Duration::from_millis(40));
		let probes = probe_writes(stats).await;

		assert_eq!(probes.len(), 1, "expected exactly one report");
		assert_eq!(probes[0].rtt, Some(40));
		assert_eq!(probes[0].bitrate, None, "unknown bitrate, not a measured zero");
	}

	/// The mirror case: a send rate with no RTT still reports.
	#[tokio::test(start_paused = true)]
	async fn reports_bitrate_without_an_rtt() {
		let stats = crate::lite::test_transport::SinkStats::default().with_send_rate(1_000_000);
		let probes = probe_writes(stats).await;

		assert_eq!(probes.len(), 1);
		assert_eq!(probes[0].bitrate, Some(1_000_000));
		assert_eq!(probes[0].rtt, None);
	}

	/// A transport measuring neither has nothing to say, and must not emit a report
	/// claiming two zeroes.
	#[tokio::test(start_paused = true)]
	async fn reports_nothing_when_nothing_is_measurable() {
		let probes = probe_writes(crate::lite::test_transport::SinkStats::default()).await;
		assert!(probes.is_empty(), "expected no report, got {probes:?}");
	}

	/// Lite03's PROBE carries no RTT field, so an RTT-only report has nothing to
	/// say there. Sending one anyway would serialize as a bare "bitrate unknown"
	/// and, worse, fire again on every RTT movement.
	#[tokio::test(start_paused = true)]
	async fn lite03_sends_nothing_for_an_rtt_only_report() {
		let stats = crate::lite::test_transport::SinkStats::default().with_rtt(std::time::Duration::from_millis(40));
		let probes = probe_writes_version(stats, Version::Lite03).await;
		assert!(probes.is_empty(), "expected no report on lite-03, got {probes:?}");
	}

	/// Lite03 still reports the half it can carry.
	#[tokio::test(start_paused = true)]
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
	#[tokio::test(start_paused = true)]
	async fn a_bitrate_going_unknown_is_retracted_once() {
		let gate = kio::Producer::new(true);
		let stats = crate::lite::test_transport::SinkStats::default().with_send_rate(1_000_000);
		let mut session = SinkSession::gated_bi(gate.consume()).with_stats(stats);
		let log = session.log.clone();
		let stream = Stream::open(&mut session, Version::Lite05).await.unwrap();

		let mut server = probe_server(session.clone(), stream, Version::Lite05);
		let mut run = std::pin::pin!(kio::wait(|waiter| server.poll_probe(waiter)));
		assert!(futures::poll!(run.as_mut()).is_pending());
		tokio::time::sleep(Duration::from_millis(150)).await;
		assert!(futures::poll!(run.as_mut()).is_pending());

		// The transport stops measuring. Everything after this is unknown.
		session.set_stats(crate::lite::test_transport::SinkStats::default());

		// Well past PROBE_MAX_AGE, so a stale-report timer would have fired repeatedly.
		for _ in 0..3 {
			tokio::time::sleep(Duration::from_secs(11)).await;
			assert!(futures::poll!(run.as_mut()).is_pending());
		}

		let writes = log.writes.lock().unwrap().clone();
		let probes = decode_probes(&writes);
		assert_eq!(
			probes.len(),
			2,
			"the measurement then one retraction, not a repeating 'unknown': {probes:?}"
		);
		assert_eq!(probes[0].bitrate, Some(1_000_000));
		assert_eq!(probes[1].bitrate, None);
	}
}
