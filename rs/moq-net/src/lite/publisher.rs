use crate::{announce, frame, group, origin, track};
use std::{sync::Arc, task::Poll, time::Duration};

use futures::{FutureExt, StreamExt, stream::FuturesUnordered};
use web_transport_trait::Stats;

use crate::{
	AsPath, Error, Origin, OriginList,
	coding::{Encode, Stream, Writer},
	lite::{
		self,
		priority::{Priority, PriorityHandle, PriorityQueue},
	},
	util::{MaybeBoxedExt, MaybeSendBox, TaskSet},
};

use super::Version;

/// Publisher-side bookkeeping for one announced path, so upstream route changes
/// forward as a restart. `sent` is the hop chain last written to the peer, or
/// `None` while the announce is filtered (reflected or excluded).
struct WatchedRoute {
	consumer: crate::broadcast::Consumer,
	/// Demand edges re-price the route without a route change, so the announce
	/// loop watches this alongside `route_changed`.
	demand: crate::broadcast::Demand,
	path: crate::PathOwned,
	sent: Option<SentRoute>,
	/// When demand drained while a zero cost was advertised. The restart that
	/// restores the cold cost is deferred by [`COST_LINGER`] past this, so
	/// viewer churn doesn't flap routing across the mesh; demand returning in
	/// the window cancels the restore.
	idle_at: Option<web_async::time::Instant>,
}

/// What the peer currently holds for a path: the forwarded hop chain plus, on
/// lite-06+, the route cost. A fresh route that differs in either is worth a wire
/// message; one that matches is not.
#[derive(Clone)]
struct SentRoute {
	hops: OriginList,
	cost: lite::RouteCost,
	/// Whether this is the route we actually serve from (the table's first
	/// entry) rather than a standby selected because the serving chain flows
	/// through the peer. Only a serving advertisement carries the demand
	/// discount, so only it re-prices on demand edges; watching demand for a
	/// standby would fire forever without ever changing the advertised cost.
	serving: bool,
}

pub(super) struct PublisherConfig<S: web_transport_trait::Session> {
	pub session: S,
	/// The origin we read local broadcasts from. Traffic stats are attributed
	/// through this handle: tag it with [`origin::Consumer::with_stats`] first.
	pub origin: origin::Consumer,
	pub version: Version,
	/// The peer's SETUP (lite-05+), shared with the subscriber half that reads
	/// it. Carries the peer's declared origin id for split-horizon serving.
	pub peer_setup: super::PeerSetup,
	/// The origin (hop) id assigned to the peer, used whenever the peer doesn't
	/// declare one itself. See `Client::with_peer_origin`.
	pub peer_origin: Option<Origin>,
}

pub(super) struct Publisher<S: web_transport_trait::Session> {
	session: S,
	origin: origin::Consumer,
	self_origin: Origin,
	// The peer's SETUP, read for the origin id it declared. Used to serve the
	// peer from a source whose chain excludes them, keeping the data plane on
	// the same split-horizon rule as the announces we send them.
	peer_setup: super::PeerSetup,
	// The identity assigned to the peer by the caller (`Client::with_peer_origin`, or
	// the per-session default a server hands every request), standing in wherever the
	// peer declines to declare one. Backs both the announce filter and the serving
	// origin, so a peer that names itself nowhere on the wire is still split-horizoned.
	peer_origin: Option<Origin>,
	// The excluded origin handle, resolved once: the peer sends exactly one
	// SETUP, so its declared id never changes for the session.
	serving: std::sync::OnceLock<origin::Consumer>,
	priority: PriorityQueue,
	version: Version,
}

impl<S: web_transport_trait::Session> Publisher<S> {
	pub fn new(config: PublisherConfig<S>) -> Self {
		// Identity stamped onto outbound announce hops. Derived from the
		// origin we're consuming so it matches the local relay identity
		// across every session, required for cross-session loop detection.
		let self_origin = *config.origin;
		Self {
			session: config.session,
			origin: config.origin,
			self_origin,
			peer_setup: config.peer_setup,
			peer_origin: config.peer_origin,
			serving: std::sync::OnceLock::new(),
			priority: Default::default(),
			version: config.version,
		}
	}

	/// The origin to resolve a peer-requested broadcast from: excludes routes
	/// through the peer, so a subscription is never served data that flowed
	/// through the subscriber. The identity is the one the peer declared in its
	/// SETUP, or the one the caller assigned it when it declared none, the same
	/// order the announce filter applies. The first call waits for the peer's
	/// SETUP (sent at startup on every lite-05+ session, well before it could
	/// learn of anything to subscribe to); the result is cached, since the peer
	/// sends exactly one SETUP per session.
	async fn serving_origin(&self) -> origin::Consumer {
		if let Some(origin) = self.serving.get() {
			return origin.clone();
		}
		// Pre-SETUP versions never declare an id, so only the assigned one applies.
		let declared = match self.version.has_setup_stream() {
			true => self.peer_setup.origin().await,
			false => None,
		};
		let origin = match declared.or(self.peer_origin) {
			Some(peer) => self.origin.clone().excluding(peer),
			None => self.origin.clone(),
		};
		// A concurrent first call may have won the race; either value is
		// identical, so keep whichever landed.
		self.serving.get_or_init(|| origin).clone()
	}

	pub async fn run(self) -> Result<(), Error> {
		// `origin::Consumer` and friends are cheap to clone (shared handles), so each control
		// stream gets its own child future and they all make progress independently.
		let this = Arc::new(self);
		let mut tasks = TaskSet::owned();

		loop {
			let stream = tasks.drive(Stream::accept(&this.session, this.version)).await?;

			let this = this.clone();
			tasks.push(async move {
				if let Err(err) = this.handle(stream).await {
					tracing::warn!(%err, "control stream error");
				}
			});
		}
	}

	async fn handle(&self, mut stream: Stream<S, Version>) -> Result<(), Error> {
		let kind = stream.reader.decode().await?;

		match kind {
			lite::ControlType::Announce => self.recv_announce(stream).await,
			lite::ControlType::Subscribe => self.recv_subscribe(stream).await,
			lite::ControlType::Fetch => self.recv_fetch(stream).await,
			lite::ControlType::Track => self.recv_track(stream).await,
			lite::ControlType::Probe => {
				self.recv_probe(stream).await;
				Ok(())
			}
			lite::ControlType::Goaway => {
				tracing::info!("received goaway stream");
				Ok(())
			}
			lite::ControlType::Session => Err(Error::UnexpectedStream),
		}
	}

	async fn recv_probe(&self, mut stream: Stream<S, Version>) {
		match Self::run_probe(&self.session, &mut stream, self.version).await {
			Ok(()) => {
				tracing::debug!("probe stream closed");
			}
			Err(err) => {
				tracing::warn!(%err, "probe stream error");
				stream.writer.abort(&err);
			}
		}
	}

	async fn run_probe(session: &S, stream: &mut Stream<S, Version>, version: Version) -> Result<(), Error> {
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
				.clamp(PROBE_INTERVAL.as_secs_f64(), PROBE_MAX_AGE.as_secs_f64());
			let range = PROBE_MAX_AGE.as_secs_f64() - PROBE_INTERVAL.as_secs_f64();
			PROBE_MAX_DELTA * (PROBE_MAX_AGE.as_secs_f64() - t) / range
		}

		let mut last_sent: Option<(lite::Probe, web_async::time::Instant)> = None;
		let mut interval = web_async::time::interval(PROBE_INTERVAL);

		loop {
			// Tick the probe interval, bailing as soon as the peer closes its side.
			let closed = {
				let mut closed = std::pin::pin!(stream.reader.closed());
				kio::wait(|waiter| {
					if let Poll::Ready(res) = waiter.poll_future(closed.as_mut()) {
						return Poll::Ready(Some(res));
					}
					let mut cx = std::task::Context::from_waker(waiter.waker());
					interval.poll_tick(&mut cx).map(|_| None)
				})
				.await
			};
			if let Some(res) = closed {
				return res;
			}

			// The two fields are independent on the wire, each using 0 for unknown,
			// so a transport that exposes only one still has something to report.
			// Anything this version can't carry is dropped here rather than by the
			// encoder, so it reads as unknown to every check below.
			// Scoped so the borrowed stats handle is dropped before the encode
			// below awaits; it isn't `Send`.
			let report = {
				let stats = session.stats();
				lite::Probe {
					bitrate: stats.estimated_send_rate(),
					rtt: version
						.has_probe_rtt()
						.then(|| stats.rtt().map(|d| d.as_millis() as u64))
						.flatten(),
				}
			};

			// Nothing left to report. Say so once if it retracts a value the peer is
			// still holding, then stay quiet rather than repeating "unknown" every
			// time the max age comes around.
			if report.bitrate.is_none() && report.rtt.is_none() {
				let retracts = last_sent
					.as_ref()
					.is_some_and(|(prev, _)| prev.bitrate.is_some() || prev.rtt.is_some());
				if !retracts {
					continue;
				}
			}

			let should_send = match &last_sent {
				None => true,
				Some((prev, at)) => {
					let elapsed = at.elapsed();
					elapsed >= PROBE_MAX_AGE
						|| moved(prev.bitrate, report.bitrate, bitrate_threshold(elapsed))
						|| moved(prev.rtt, report.rtt, PROBE_RTT_DELTA)
				}
			};

			if should_send {
				stream.writer.encode(&report).await?;
				last_sent = Some((report, web_async::time::Instant::now()));
			}
		}
	}

	pub async fn recv_announce(&self, mut stream: Stream<S, Version>) -> Result<(), Error> {
		let interest = stream.reader.decode::<lite::AnnounceRequest>().await?;
		let prefix = interest.prefix.to_owned();

		// The identity whose routes we filter out. Lite-04/05 carry it per
		// announce stream; lite-06+ reads the session-wide SETUP Origin
		// parameter, the same identity the subscribe path excludes. A peer that
		// declares nothing falls back to the identity the caller assigned it
		// (`with_peer_origin`), if any.
		let assigned = self.peer_origin.map(|origin| origin.id()).unwrap_or(0);
		let exclude_hop = if self.version.has_exclude_hop() {
			match interest.exclude_hop {
				0 => assigned,
				id => id,
			}
		} else if self.version.has_setup_stream() {
			self.peer_setup
				.origin()
				.await
				.map(|origin| origin.id())
				.unwrap_or(assigned)
		} else {
			assigned
		};

		// If the requested prefix is outside our scope (an empty origin, or a token
		// that doesn't grant it), we simply have nothing to announce. Respond with an
		// empty set and keep the stream open (the subscriber treats a FIN here as a
		// fatal stream close), rather than erroring, which would reset the stream.
		let origin = self
			.origin
			.scope(&[prefix.as_path()])
			.unwrap_or_else(|| self.origin.empty());
		// Register the split-horizon peer on the announce cursor too. The origin
		// model uses this exposure to park a reflected copy before it can replace
		// the source we are currently advertising to that peer.
		let origin = match Origin::new(exclude_hop) {
			Ok(peer) => origin.excluding(peer),
			Err(_) => origin,
		};
		let mut announced = origin.announced();

		if let Err(err) = Self::run_announce(
			&mut stream,
			&origin,
			&mut announced,
			&prefix,
			self.self_origin,
			exclude_hop,
			self.version,
		)
		.await
		{
			match &err {
				Error::Cancel | Error::Transport(_) => {
					tracing::debug!(prefix = %origin.absolute(prefix), "announcing cancelled");
				}
				err => {
					tracing::warn!(%err, prefix = %origin.absolute(prefix), "announcing error");
				}
			}

			stream.writer.abort(&err);
		}

		Ok(())
	}

	#[allow(clippy::too_many_arguments)]
	async fn run_announce(
		stream: &mut Stream<S, Version>,
		origin: &origin::Consumer,
		announced: &mut announce::Consumer,
		prefix: impl AsPath,
		self_origin: Origin,
		// Peer's session-level origin id, sent in ANNOUNCE_REQUEST on lite-04/05.
		// We skip forwarding announces whose hop chain already contains this id, so
		// reflected announces (cluster loops) never hit the wire. Zero means the peer
		// didn't send one (every other version), in which case we forward and the peer
		// drops the reflection on receipt.
		exclude_hop: u64,
		version: Version,
	) -> Result<(), Error> {
		let prefix = prefix.as_path();

		// Lite06+: announce ids. Every `active` we send implicitly assigns the next
		// per-stream ordinal, and `ended` references the id instead of repeating the
		// path. Keyed by suffix; only announces that actually hit the wire get an id
		// (filtered ones were never seen by the peer).
		let mut next_announce_id: u64 = 0;
		let mut announce_ids: std::collections::HashMap<crate::PathOwned, u64> = std::collections::HashMap::new();

		// Lite05+: watch every announced broadcast's route and forward changes as a
		// restart, so an upstream failover re-advertises downstream instead of the
		// peer keeping a stale hop chain. Keyed by suffix; filtered announces are
		// watched too, since an update can cross the forwarding filter either way.
		let mut watched: std::collections::HashMap<crate::PathOwned, WatchedRoute> = std::collections::HashMap::new();
		// Pre-restart versions (Lite01-04) never populate `watched`, but the
		// broadcast consumer handed out by an excluding cursor carries the
		// ExclusionGuard that keeps the front marked as exposed to this peer. Hold
		// it for as long as the peer holds the advertisement, or the guard releases
		// right after the Active is written and a reflected UNKNOWN route can
		// replace the incumbent after all.
		let mut held: std::collections::HashMap<crate::PathOwned, crate::broadcast::Consumer> =
			std::collections::HashMap::new();

		match version {
			Version::Lite01 | Version::Lite02 => {
				let mut init = Vec::new();

				// Send ANNOUNCE_INIT as the first message with all currently active paths
				// We use `try_next()` to synchronously get the initial updates.
				while let Some(crate::announce::Update { path, broadcast }) = announced.try_next() {
					let suffix = path
						.strip_prefix(&prefix)
						.expect("origin returned invalid path")
						.to_owned();
					let absolute = origin.absolute(&path).to_owned();

					if let Some(broadcast) = broadcast {
						// The same per-peer selection as the live loop: an initial path
						// with no advertisable route (a reflection, or every hop through
						// the peer's assigned identity) is filtered like a live announce.
						let selected = Self::select_route(
							&broadcast.routes(),
							&broadcast.demand(),
							self_origin,
							exclude_hop,
							version,
							&absolute,
						);
						if selected.is_none() {
							continue;
						}
						tracing::debug!(broadcast = %absolute, "announce");
						if !init.contains(&suffix) {
							init.push(suffix);
						}
					} else {
						// A potential race.
						tracing::debug!(broadcast = %absolute, "unannounce");
						init.retain(|p| p != &suffix);
					}
				}

				let announce_init = lite::AnnounceInit { suffixes: init };
				stream.writer.encode(&announce_init).await?;
			}
			_ if version.has_announce_ok() => {
				// Drain the current active set synchronously (like the Lite01/02 path),
				// stashing suffix+hops so we can both COUNT them for AnnounceOk and re-send
				// them afterward. The receiver stamps our origin onto each hop chain, so we
				// forward the stored chain as-is (no self push here).
				let mut initial: Vec<(crate::PathOwned, SentRoute)> = Vec::new();
				while let Some(crate::announce::Update { path, broadcast }) = announced.try_next() {
					let suffix = path
						.strip_prefix(&prefix)
						.expect("origin returned invalid path")
						.to_owned();
					let absolute = origin.absolute(&path).to_owned();

					match broadcast {
						Some(broadcast) => {
							let routes = broadcast.routes();
							let demand = broadcast.demand();
							// Watch even the announces we filter below: a later route update
							// can cross the forwarding filter in either direction.
							watched.insert(
								suffix.clone(),
								WatchedRoute {
									consumer: broadcast.clone(),
									demand: demand.clone(),
									path: path.clone(),
									sent: None,
									idle_at: None,
								},
							);
							// The same per-peer selection as the live loop, so the count
							// matches exactly what we send.
							let Some(route) =
								Self::select_route(&routes, &demand, self_origin, exclude_hop, version, &absolute)
							else {
								continue;
							};
							tracing::debug!(broadcast = %absolute, "announce");
							initial.retain(|(s, _)| s != &suffix);
							initial.push((suffix, route));
						}
						None => {
							// A potential race: a just-announced path already unannounced.
							tracing::debug!(broadcast = %absolute, "unannounce");
							watched.remove(&suffix);
							initial.retain(|(s, _)| s != &suffix);
						}
					}
				}

				// Report our origin id (stamped onto hops by the receiver, not us)
				// and the count of initial announces that follow immediately.
				let ok = lite::AnnounceOk {
					origin: self_origin,
					active: initial.len() as u64,
				};
				let mut buf = bytes::BytesMut::new();
				ok.encode(&mut buf, version)?;
				for (suffix, route) in &initial {
					if version.has_announce_id() {
						announce_ids.insert(suffix.clone(), next_announce_id);
						next_announce_id += 1;
					}
					if let Some(entry) = watched.get_mut(suffix) {
						entry.sent = Some(route.clone());
					}
					lite::AnnounceBroadcast::Active {
						suffix: suffix.as_path(),
						hops: route.hops.clone(),
						cost: route.cost,
					}
					.encode(&mut buf, version)?;
				}
				let mut buf = buf.freeze();
				stream.writer.write_all(&mut buf).await?;
			}
			_ => {
				// Lite03/Lite04: no announce init, no AnnounceOk.
			}
		}

		// One announce-loop turn: either an (un)announce from the origin, a route
		// change on an already-announced broadcast, or a demand edge re-pricing
		// one. Resolved outside the select so the handlers below can freely
		// mutate the maps its futures borrow. A route turn re-reads the
		// broadcast's route table and re-runs the per-peer selection; `Err`
		// means the broadcast is gone.
		enum Op {
			Announce(Option<crate::announce::Update>),
			Route(crate::PathOwned, Result<(), Error>),
			Idle(crate::PathOwned),
			/// The linger sleep fired without an expired entry (it was canceled,
			/// or a later deadline remains): restart the turn so the next
			/// deadline arms a fresh sleep.
			Linger,
		}

		use crate::broadcast::COST_LINGER;

		// Send updates as they arrive. Closure wins the race so a dead peer can't
		// stall on a busy announce feed.
		let mut linger = kio::time::Deadline::new();
		loop {
			// The earliest deferred cost-restore, if any entry's linger is running.
			linger.set(
				watched
					.values()
					.filter_map(|entry| entry.idle_at)
					.min()
					.map(|at| at + COST_LINGER),
			);
			let op = {
				let mut closed = std::pin::pin!(stream.reader.closed());
				kio::wait(|waiter| {
					if let Poll::Ready(res) = waiter.poll_future(closed.as_mut()) {
						return Poll::Ready(Err(res));
					}
					if let Poll::Ready(next) = announced.poll_next(waiter) {
						return Poll::Ready(Ok(Op::Announce(next)));
					}
					// Stamped per poll rather than kept: the turn always ends in a
					// `Ready` below once it fires, so it never has to survive.
					let fired = linger.poll(waiter).is_ready().then(web_async::time::Instant::now);
					// Poll every watched broadcast for a route-table change; each
					// wake rescans the map, which announce-control rates make fine.
					for (suffix, entry) in watched.iter_mut() {
						if let Poll::Ready(res) = entry.consumer.poll_routes_changed(waiter) {
							return Poll::Ready(Ok(Op::Route(suffix.clone(), res)));
						}
						// Demand edges re-price the route without a route change:
						// watch the direction opposite the advertised cost. Closure
						// is ignored here; the route watch above surfaces it.
						if !version.has_route_cost() {
							continue;
						}
						let Some(sent) = &entry.sent else { continue };
						// The demand discount only applies to the serving route:
						// a standby advertised to an excluded peer keeps its own
						// cost, so demand edges can never re-price it.
						if !sent.serving {
							continue;
						}
						if sent.cost != lite::RouteCost(0) {
							if let Poll::Ready(Ok(())) = entry.demand.poll_used(waiter) {
								return Poll::Ready(Ok(Op::Route(suffix.clone(), Ok(()))));
							}
							continue;
						}
						match entry.idle_at {
							// Demand coming back within the linger cancels the
							// restore; fall through to re-arm the unused watch.
							Some(_) if entry.demand.is_used() => entry.idle_at = None,
							// The linger expired: re-price via the route path.
							Some(at) if fired.is_some_and(|now| now >= at + COST_LINGER) => {
								entry.idle_at = None;
								return Poll::Ready(Ok(Op::Route(suffix.clone(), Ok(()))));
							}
							// Still lingering: the sleep owns the wakeup, and
							// `poll_used` re-arms the cancel check above.
							Some(_) => {
								let _ = entry.demand.poll_used(waiter);
								continue;
							}
							None => {}
						}
						if let Poll::Ready(Ok(())) = entry.demand.poll_unused(waiter) {
							return Poll::Ready(Ok(Op::Idle(suffix.clone())));
						}
					}
					match fired {
						Some(_) => Poll::Ready(Ok(Op::Linger)),
						None => Poll::Pending,
					}
				})
				.await
			};
			let op = match op {
				Ok(op) => op,
				Err(res) => return res,
			};

			match op {
				Op::Announce(None) => {
					stream.writer.finish()?;
					return stream.writer.closed().await;
				}
				Op::Announce(Some(crate::announce::Update { path, broadcast })) => {
					let suffix = path
						.strip_prefix(&prefix)
						.expect("origin returned invalid path")
						.to_owned();
					let absolute = origin.absolute(&path).to_owned();

					match broadcast {
						Some(active) => {
							let routes = active.routes();
							let demand = active.demand();
							if lite::restart_supported(version) {
								// Watch even if filtered below: a route update can cross
								// the forwarding filter in either direction.
								watched.insert(
									suffix.clone(),
									WatchedRoute {
										consumer: active.clone(),
										demand: demand.clone(),
										path: path.clone(),
										sent: None,
										idle_at: None,
									},
								);
							}
							let Some(route) =
								Self::select_route(&routes, &demand, self_origin, exclude_hop, version, &absolute)
							else {
								continue;
							};
							tracing::debug!(broadcast = %absolute, "announce");
							if version.has_announce_id() {
								let prev = announce_ids.insert(suffix.clone(), next_announce_id);
								debug_assert!(prev.is_none(), "announce id still assigned for a new announce");
								next_announce_id += 1;
							}
							if let Some(entry) = watched.get_mut(&suffix) {
								entry.sent = Some(route.clone());
							}
							if !lite::restart_supported(version) {
								held.insert(suffix.clone(), active.clone());
							}
							stream
								.writer
								.encode(&lite::AnnounceBroadcast::Active {
									suffix,
									hops: route.hops,
									cost: route.cost,
								})
								.await?;
						}
						None => {
							tracing::debug!(broadcast = %absolute, "unannounce");
							// A watched entry with `sent: None` means the peer holds no live
							// advertisement (a route-filter retract already sent its Ended);
							// repeating the Ended would be a spurious wire message. Pre-watch
							// versions never populate `watched`, so they keep sending the
							// Ended even for announces filtered above.
							let retracted = watched.remove(&suffix).is_some_and(|entry| entry.sent.is_none());
							held.remove(&suffix);
							if version.has_announce_id() {
								// Retract by id; nothing to send if the announce was filtered and
								// the peer never saw it (an unknown id is a protocol violation).
								if let Some(id) = announce_ids.remove(&suffix) {
									stream.writer.encode(&lite::AnnounceBroadcast::EndedId { id }).await?;
								}
							} else if !retracted {
								// An ended announce doesn't need hops; the receiver matches on path only.
								stream
									.writer
									.encode(&lite::AnnounceBroadcast::Ended {
										suffix,
										hops: OriginList::new(),
									})
									.await?;
							}
						}
					}
				}
				Op::Route(suffix, res) => {
					if res.is_err() {
						// The broadcast is gone; the origin delivers the Ended itself.
						watched.remove(&suffix);
						continue;
					}
					let Some(entry) = watched.get_mut(&suffix) else {
						continue;
					};
					// Any re-price supersedes a pending cost-restore; a stale
					// timestamp would spin the linger sleep forever.
					entry.idle_at = None;
					let absolute = origin.absolute(&entry.path).to_owned();
					let routes = entry.consumer.routes();
					let hops = Self::select_route(&routes, &entry.demand, self_origin, exclude_hop, version, &absolute);
					let sent = entry.sent.clone();
					match (hops, sent) {
						// Neither the forwarded chain nor the cost moved: nothing to
						// send. The serving flag may still have flipped (a failover
						// onto the already-advertised standby), so store it for the
						// demand watches without a wire message.
						(Some(route), Some(sent)) if route.hops == sent.hops && route.cost == sent.cost => {
							entry.sent = Some(route);
						}
						// The chain or the cost changed (an upstream failover, a repriced
						// link, or a broadcast going hot): restart, so the peer updates its
						// route in place instead of re-resolving.
						(Some(route), Some(_)) => {
							tracing::debug!(broadcast = %absolute, "reannounce");
							if version.has_announce_id() {
								// The id exists for every live advertisement; a panic here would
								// silently kill the announce loop (the peer keeps stale routes),
								// so a bookkeeping bug degrades to a skipped restart instead.
								let Some(id) = announce_ids.get(&suffix).copied() else {
									debug_assert!(false, "announced path without an announce id");
									tracing::warn!(broadcast = %absolute, "restart without an announce id; skipping");
									continue;
								};
								entry.sent = Some(route.clone());
								stream
									.writer
									.encode(&lite::AnnounceBroadcast::Restart {
										id,
										hops: route.hops,
										cost: route.cost,
									})
									.await?;
							} else {
								// Lite05: a duplicate ANNOUNCE for a live path is the restart.
								entry.sent = Some(route.clone());
								stream
									.writer
									.encode(&lite::AnnounceBroadcast::Active {
										suffix,
										hops: route.hops,
										cost: route.cost,
									})
									.await?;
							}
						}
						// Previously filtered, now forwardable: a fresh announce.
						(Some(route), None) => {
							tracing::debug!(broadcast = %absolute, "announce");
							if version.has_announce_id() {
								announce_ids.insert(suffix.clone(), next_announce_id);
								next_announce_id += 1;
							}
							entry.sent = Some(route.clone());
							stream
								.writer
								.encode(&lite::AnnounceBroadcast::Active {
									suffix,
									hops: route.hops,
									cost: route.cost,
								})
								.await?;
						}
						// The new chain must not be forwarded (it now loops through the
						// peer, or the peer excluded it): retract.
						(None, Some(_)) => {
							tracing::debug!(broadcast = %absolute, "unannounce (filtered route)");
							entry.sent = None;
							if version.has_announce_id() {
								if let Some(id) = announce_ids.remove(&suffix) {
									stream.writer.encode(&lite::AnnounceBroadcast::EndedId { id }).await?;
								}
							} else {
								stream
									.writer
									.encode(&lite::AnnounceBroadcast::Ended {
										suffix,
										hops: OriginList::new(),
									})
									.await?;
							}
						}
						// Still filtered: keep watching.
						(None, None) => {}
					}
				}
				// Demand drained while advertising zero: start the linger. The
				// restore rides the deadline unless demand returns first.
				Op::Idle(suffix) => {
					if let Some(entry) = watched.get_mut(&suffix) {
						entry.idle_at = Some(web_async::time::Instant::now());
					}
				}
				// The linger sleep's job is done; the next turn arms the next
				// deadline (or none).
				Op::Linger => {}
			}
		}
	}

	/// Pick the route to advertise to this peer: the most preferred announced
	/// route whose hop chain avoids both the peer (`exclude_hop`) and ourselves
	/// (a reflection), with the outgoing chain and cost ready for the wire.
	///
	/// `routes` is the broadcast's table in preference order with the serving
	/// (active) route first, so the peer usually receives exactly what we serve
	/// everyone; a peer that the active chain flows through receives the best
	/// standby instead of nothing. The subscribe path picks its source by the
	/// same exclusion (see `origin::Consumer::excluding`), which is what keeps
	/// the advertised chain truthful and the mesh loop-free.
	///
	/// Returns `None` when no route qualifies: every chain loops through the
	/// peer or us, or none is announced.
	fn select_route(
		routes: &[crate::broadcast::Route],
		demand: &crate::broadcast::Demand,
		self_origin: Origin,
		exclude_hop: u64,
		version: Version,
		absolute: &crate::Path,
	) -> Option<SentRoute> {
		let exclude = match exclude_hop {
			0 => Origin::UNKNOWN,
			id => Origin::new(id).unwrap_or(Origin::UNKNOWN),
		};

		for (route, serving) in crate::broadcast::advertisable_routes(routes, self_origin, exclude) {
			let mut hops = route.hops.clone();
			// Lite05+ moves the self-stamp to the receiver, which appends our id (reported
			// once via AnnounceOk) on receipt. Older versions stamp it here, dropping if the
			// chain is full.
			if !version.has_announce_ok() && hops.push(self_origin).is_err() {
				tracing::warn!(broadcast = %absolute, "dropping announce; hop chain at MAX_HOPS (possible loop)");
				continue;
			}
			let cost = Self::outgoing_cost(version, demand, route, serving);
			return Some(SentRoute { hops, cost, serving });
		}
		tracing::debug!(broadcast = %absolute, %exclude_hop, "no advertisable route for this peer");
		None
	}

	/// The cost to advertise for a route, alongside its outgoing hop chain.
	///
	/// While the broadcast has demand, the *serving* (active) route costs zero:
	/// our ingress is already paid for (or, for a local standby publisher, the
	/// work is already running), so one more subscriber only pays the link to
	/// reach us. A standby advertised to a peer the active chain flows through
	/// keeps its own accumulated cost: serving that peer means opening a fresh
	/// ingest, which is not already paid for. Otherwise we forward the
	/// accumulated route cost unchanged, which for a standby publisher is its
	/// production cost and for a pure forwarder is the price of the fetch a
	/// subscription would trigger.
	///
	/// The receiving side adds its own link price on top, so we never account for
	/// the link we are sending over. Pre-lite-06 peers get nothing (the field isn't
	/// on their wire), leaving hop count as the metric exactly as before.
	fn outgoing_cost(
		version: Version,
		demand: &crate::broadcast::Demand,
		route: &crate::broadcast::Route,
		serving: bool,
	) -> lite::RouteCost {
		if !version.has_route_cost() {
			return lite::RouteCost::default();
		}

		lite::RouteCost(crate::broadcast::outgoing_cost(demand, route, serving))
	}

	pub async fn recv_track(&self, mut stream: Stream<S, Version>) -> Result<(), Error> {
		// The Track Stream is lite-05+ only.
		if !self.version.has_track_stream() {
			return Err(Error::UnexpectedStream);
		}

		let request = stream.reader.decode::<lite::Track>().await?;
		let track = request.track.clone();
		let absolute = self.origin.absolute(&request.broadcast).to_owned();

		tracing::debug!(broadcast = %absolute, %track, "track info requested");

		if let Err(err) = self.run_track_info(&mut stream, &request).await {
			match &err {
				Error::Cancel | Error::Transport(_) => {
					tracing::debug!(broadcast = %absolute, %track, "track info cancelled")
				}
				err => tracing::warn!(broadcast = %absolute, %track, %err, "track info error"),
			}
			stream.writer.abort(&err);
		}

		Ok(())
	}

	async fn run_track_info(&self, stream: &mut Stream<S, Version>, request: &lite::Track<'_>) -> Result<(), Error> {
		// The peer requested this exact path, so it has already seen an announcement for it.
		// `request_broadcast` resolves it immediately, or falls back to an `origin::Dynamic`
		// handler (as in recv_subscribe).
		let broadcast = self
			.serving_origin()
			.await
			.request_broadcast(&request.broadcast)
			.await?;
		let info = broadcast.track(&request.track)?.info().await?;

		// TRACK_INFO only flows on Lite05+ (the encode errors otherwise), where every
		// track is timed, so the model's timescale and retention bound go on the wire
		// verbatim.
		stream
			.writer
			.encode(&lite::TrackInfo {
				priority: info.priority,
				ordered: info.ordered,
				latency_max: info.latency_max,
				timescale: info.timescale,
			})
			.await?;

		stream.writer.finish()?;
		stream.writer.closed().await
	}

	pub async fn recv_subscribe(&self, mut stream: Stream<S, Version>) -> Result<(), Error> {
		let subscribe = stream.reader.decode::<lite::Subscribe>().await?;

		let id = subscribe.id;
		let track = subscribe.track.clone();
		let absolute = self.origin.absolute(&subscribe.broadcast).to_owned();

		tracing::info!(%id, broadcast = %absolute, %track, "subscribed started");

		// We just received a subscribe for this exact path, so by definition the peer has
		// already seen an announcement for it. `request_broadcast` resolves an announced
		// broadcast immediately; if it isn't announced it falls back to an `origin::Dynamic`
		// handler (or resolves to an error when there is none).
		let broadcast = self.serving_origin().await.request_broadcast(&subscribe.broadcast);

		// Stats (subscriptions, viewer refcount, groups/frames/bytes) are counted in
		// the model, through the tagged `origin::Consumer` this broadcast is resolved
		// from; the wire loop carries no counters.
		if let Err(err) = Self::run_subscribe(
			self.session.clone(),
			&mut stream,
			&subscribe,
			broadcast,
			self.priority.clone(),
			self.version,
		)
		.await
		{
			match &err {
				// TODO better classify WebTransport errors.
				Error::Cancel | Error::Transport(_) => {
					tracing::info!(%id, broadcast = %absolute, %track, "subscribed cancelled")
				}
				err => {
					tracing::warn!(%id, broadcast = %absolute, %track, %err, "subscribed error")
				}
			}
			stream.writer.abort(&err);
		} else {
			tracing::info!(%id, broadcast = %absolute, %track, "subscribed complete")
		}

		Ok(())
	}

	async fn run_subscribe(
		session: S,
		stream: &mut Stream<S, Version>,
		subscribe: &lite::Subscribe<'_>,
		broadcast: kio::Pending<origin::Requesting>,
		priority: PriorityQueue,
		version: Version,
	) -> Result<(), Error> {
		let subscription = crate::track::Subscription {
			priority: subscribe.priority,
			ordered: subscribe.ordered,
			latency_max: subscribe.max_latency,
			group_start: subscribe.start_group,
			group_end: subscribe.end_group,
		};

		// Awaits the dynamic fallback if the broadcast wasn't announced; resolves
		// immediately otherwise (including an unroutable/dropped error).
		let broadcast = broadcast.await?;
		let track_consumer = broadcast.track(&subscribe.track)?;
		// One subscriber for the whole subscription: `run_track` polls its groups and its
		// best-effort datagrams from this single cursor, so a group-only or datagram-only
		// track opens exactly one subscription (no duplicate demand).
		let track = track_consumer.subscribe(subscription).await?;

		// Per-frame timestamps require a wire format that carries them. Lite05+ prefixes
		// every frame with a zigzag-delta timestamp at the track's timescale; older
		// drafts have no wire field, so `None` here means "don't emit the prefix" (the
		// frames still carry timestamps in the model, just not on this wire).
		let timescale = if version.has_track_stream() {
			Some(track.info().timescale)
		} else {
			None
		};

		// Lite05+ accepts implicitly: no SUBSCRIBE_OK, the immutable properties live
		// in TRACK_INFO, and the resolved range arrives as SUBSCRIBE_START/END emitted
		// from run_track. Older drafts still acknowledge with SUBSCRIBE_OK here.
		if !version.has_track_stream() {
			let info = lite::SubscribeOk {
				priority: subscribe.priority,
				ordered: false,
				max_latency: std::time::Duration::ZERO,
				start_group: None,
				end_group: None,
			};
			stream.writer.encode(&lite::SubscribeResponse::Ok(info)).await?;
		}

		// Track-level subscriber priority. SUBSCRIBE_UPDATE messages broadcast new values
		// to both run_track (so future groups inherit the new priority) and serve_group
		// tasks (so in-flight groups update via PriorityHandle::set_track). The producer
		// stays in run_subscribe and gets handed to run_track so the same loop that
		// parses SUBSCRIBE_UPDATEs also fans the new priority out.
		let track_priority_tx = kio::Producer::new(subscribe.priority);

		let sub = Subscription {
			session,
			id: subscribe.id,
			track_name: Arc::from(track.name()),
			priority,
			track_priority: track_priority_tx.consume(),
			track_priority_seen: subscribe.priority,
			version,
			timescale,
		};

		// `end_group` is a serving cap, not a subscription terminator: groups with
		// sequence > cap are held in the producer's cache until the subscriber raises
		// the cap (or unsets it) via SUBSCRIBE_UPDATE, then served in order. Only a
		// peer FIN actually ends the subscription. This is what lets relays pause an
		// upstream subscription across consumer churn without tearing it down.
		//
		// run_track serves groups and best-effort datagrams off the one subscriber.
		sub.run_track(
			track,
			subscribe.start_group,
			subscribe.end_group,
			&mut stream.reader,
			&mut stream.writer,
			&track_priority_tx,
		)
		.await?;

		stream.writer.finish()?;
		stream.writer.closed().await
	}

	pub async fn recv_fetch(&self, mut stream: Stream<S, Version>) -> Result<(), Error> {
		// FETCH is lite-05+ only; older drafts have no dedicated FETCH stream.
		if !self.version.has_track_stream() {
			return Err(Error::UnexpectedStream);
		}

		let fetch = stream.reader.decode::<lite::Fetch>().await?;

		let track = fetch.track.clone();
		let group = fetch.group;
		let absolute = self.origin.absolute(&fetch.broadcast).to_owned();

		tracing::info!(broadcast = %absolute, %track, %group, "fetch started");

		// The peer fetched this exact path, so it has already seen an announcement for it.
		// `request_broadcast` resolves it immediately, or falls back to an `origin::Dynamic`
		// handler (as in recv_subscribe).
		let broadcast = self.serving_origin().await.request_broadcast(&fetch.broadcast);

		if let Err(err) = Self::run_fetch(&mut stream, &fetch, broadcast, self.version).await {
			match &err {
				Error::Cancel | Error::Transport(_) => {
					tracing::info!(broadcast = %absolute, %track, %group, "fetch cancelled")
				}
				err => tracing::warn!(broadcast = %absolute, %track, %group, %err, "fetch error"),
			}
			stream.writer.abort(&err);
		} else {
			tracing::info!(broadcast = %absolute, %track, %group, "fetch complete");
		}

		Ok(())
	}

	async fn run_fetch(
		stream: &mut Stream<S, Version>,
		fetch: &lite::Fetch<'_>,
		broadcast: kio::Pending<origin::Requesting>,
		version: Version,
	) -> Result<(), Error> {
		let broadcast = broadcast.await?;
		let track = broadcast.track(&fetch.track)?;

		let mut group = track
			.fetch_group(
				fetch.group,
				group::Fetch {
					priority: fetch.priority,
				},
			)
			.await?;

		// FETCH is gated to lite-05+, which learned the track timescale via TRACK_INFO.
		let timescale = if version.has_track_stream() {
			Some(group.timescale())
		} else {
			None
		};

		// Stream every frame in order. The delta-timestamp baseline resets to 0, so the
		// first served frame's delta is its absolute timestamp (the subscriber decodes
		// against the same baseline).
		//
		// A fetched group is usually cached whole, so the batch takes it under one lock;
		// a fetch that catches the live edge falls back to streaming the open tail.
		let mut prev_ts: u64 = 0;
		let mut buf: frame::Buffer = frame::Buffer::new();
		loop {
			let step = kio::wait(|waiter| match group.poll_read_frames(waiter, &mut buf) {
				Poll::Pending => group
					.poll_next_frame(waiter)
					.map_ok(|frame| frame.map_or(Step::Done, Step::Partial)),
				res => res.map_ok(|count| if count == 0 { Step::Done } else { Step::Batch }),
			})
			.await?;

			match step {
				Step::Batch => {
					for i in 0..buf.filled().len() {
						let frame = buf.filled()[i].clone();
						write_fetch_frame(&mut stream.writer, frame, timescale, &mut prev_ts).await?;
						// One stamp per batch isn't enough for a slow peer; see `write_group`.
						group.keep_alive();
					}
				}
				Step::Partial(mut frame) => {
					write_fetch_partial(&mut stream.writer, &mut frame, timescale, &mut prev_ts).await?;
				}
				Step::Done => break,
			}
		}

		stream.writer.finish()?;
		stream.writer.closed().await
	}
}

#[cfg(test)]
mod test {
	use super::*;
	use crate::{Timestamp, broadcast};

	fn track_producer(name: impl Into<Arc<str>>) -> track::Producer {
		track::Producer::new(Arc::new(broadcast::Info::default()), name, None)
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
		let mut producer = track_producer("test");
		let mut subscriber = producer.subscribe(None);

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

		// Staleness is the latency window's job, not arrival order's: the track
		// still finishes normally afterward.
		producer.finish_at(3).unwrap();
		match recv_next(&mut subscriber, false, false).await.unwrap() {
			Recv::Finished => {}
			_ => panic!("expected finished"),
		}
	}
}

/// The announce loop's demand/linger state machine: a drained broadcast keeps
/// advertising zero for `COST_LINGER` before the restart that restores its cold
/// cost, demand returning in the window cancels the restore, and a route change
/// supersedes it. Time is paused, so the 5s linger is deterministic.
#[cfg(test)]
mod announce_test {
	use super::*;
	use crate::coding::{Decode, Reader};
	use crate::lite::test_transport::*;
	use std::sync::Mutex;

	const VERSION: Version = Version::Lite06Wip;

	/// The broadcast's cold cost: what the route advertises without demand.
	const COLD: u64 = 7;

	/// The original publisher stamped on every harness route: content identity is
	/// keyed on the first hop, so a route change that should ride through as a
	/// restart must keep it.
	fn pub_hops() -> OriginList {
		OriginList::try_from(vec![Origin::new(9).unwrap()]).unwrap()
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
		/// Held for the whole test: dropping the origin producer unannounces
		/// every broadcast under it, which would end the announce loop.
		origin: origin::Producer,
		/// The publishing side: route changes go in here.
		source: crate::broadcast::Producer,
		/// A downstream viewer: its `track()` handles are the broadcast's demand.
		downstream: crate::broadcast::Consumer,
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

		/// Announce a second broadcast once the loop is already running, with a
		/// viewer attached so it advertises warm. Returns the producer (kept
		/// alive by the caller) and the viewer handle whose drop drains demand.
		async fn announce(&mut self, name: &str) -> (crate::broadcast::Producer, track::Consumer) {
			let source = self
				.origin
				.create_broadcast(
					name,
					crate::broadcast::Route::new()
						.with_hops(pub_hops())
						.with_cost(COLD)
						.with_announce(true),
				)
				.unwrap();
			let downstream = self.origin.consume().announced_broadcast(name).await.unwrap();
			let track = downstream.track("video").unwrap();
			settle().await;

			// It announces cold, then immediately re-prices warm for the viewer.
			match self.wire.take_announces().as_slice() {
				[
					lite::AnnounceBroadcast::Active { cost: first, .. },
					lite::AnnounceBroadcast::Restart { cost: second, .. },
				] => {
					assert_eq!(*first, lite::RouteCost(COLD));
					assert_eq!(*second, lite::RouteCost(0));
				}
				// The viewer may already be attached when the announce is built.
				[lite::AnnounceBroadcast::Active { cost, .. }] => assert_eq!(*cost, lite::RouteCost(0)),
				other => panic!("expected {name} to announce, got {other:?}"),
			}

			(source, track)
		}
	}

	async fn settle() {
		tokio::time::sleep(Duration::from_millis(1)).await;
	}

	/// Announce one broadcast with cold cost [`COLD`] and run the announce loop
	/// against it, optionally with a viewer already attached (so the initial
	/// announce goes out warm, at cost zero).
	async fn harness(demand: bool) -> (Harness, Option<track::Consumer>) {
		let origin = Origin::new(1).unwrap().produce();
		let source = origin
			.create_broadcast(
				"cam",
				crate::broadcast::Route::new()
					.with_hops(pub_hops())
					.with_cost(COLD)
					.with_announce(true),
			)
			.unwrap();
		let downstream = origin.consume().announced_broadcast("cam").await.unwrap();
		let track = demand.then(|| downstream.track("video").unwrap());

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
			Publisher::<SinkSession>::run_announce(&mut stream, &consumer, &mut announced, "", self_origin, 0, VERSION)
				.await
		});
		settle().await;

		let mut wire = Wire { writes, cursor: 0 };
		assert_eq!(wire.take_ok().active, 1, "expected one initial announce");
		let expected = if demand { 0 } else { COLD };
		match wire.take_announces().as_slice() {
			[lite::AnnounceBroadcast::Active { cost, .. }] => assert_eq!(*cost, lite::RouteCost(expected)),
			other => panic!("expected the initial announce, got {other:?}"),
		}

		(
			Harness {
				origin,
				source,
				downstream,
				wire,
				task,
			},
			track,
		)
	}

	/// Demand draining while zero is advertised must not re-price immediately:
	/// the restore waits out the linger, so viewer churn doesn't flap routing.
	#[tokio::test(start_paused = true)]
	async fn drain_defers_the_cold_restore() {
		let (h, track) = harness(true).await;

		drop(track);
		settle().await;
		h.assert_idle();

		// Still inside the linger window: still quiet.
		tokio::time::sleep(Duration::from_secs(3)).await;
		h.assert_idle();
	}

	/// Demand returning within the linger cancels the pending restore, and the
	/// next drain starts a fresh window rather than inheriting the old deadline.
	///
	/// The second drain is what makes the cancellation observable. Silence alone
	/// can't distinguish "the deadline was cleared" from "it fired but re-priced
	/// to the same zero cost, so nothing went out": both are quiet. By draining
	/// again at t=4s, an uncancelled t=0 deadline would fire at t=5s with demand
	/// already gone, sending the restart a full four seconds early.
	#[tokio::test(start_paused = true)]
	async fn demand_return_cancels_the_restore() {
		let (mut h, track) = harness(true).await;

		// t=0: demand drains, arming the restore for t=5s.
		drop(track);
		tokio::time::sleep(Duration::from_secs(3)).await;

		// t=3s: a new viewer inside the window cancels it.
		let track = h.downstream.track("video").unwrap();
		tokio::time::sleep(Duration::from_secs(1)).await;

		// t=4s: drained again, so the restore is due at t=9s, not t=5s.
		drop(track);
		tokio::time::sleep(Duration::from_secs(2)).await;

		// t=6s: past the stale deadline. A restart here means it was never cleared.
		h.assert_idle();

		// t=10s: past the fresh deadline, so the restore finally lands.
		tokio::time::sleep(Duration::from_secs(4)).await;
		match h.wire.take_announces().as_slice() {
			[lite::AnnounceBroadcast::Restart { id: 0, cost, .. }] => assert_eq!(*cost, lite::RouteCost(COLD)),
			other => panic!("expected the restore on the fresh deadline, got {other:?}"),
		}
	}

	/// Each lingering broadcast restores on its own deadline: the loop sleeps
	/// until the *earliest* pending restore, not the latest.
	///
	/// With one broadcast the deadline scan is trivially correct, so this stages
	/// two with staggered drains. Taking the maximum instead would hold the first
	/// broadcast's restore back until the second's deadline.
	#[tokio::test(start_paused = true)]
	async fn staggered_lingers_restore_independently() {
		let (mut h, first) = harness(true).await;
		let (_second_source, second) = h.announce("cam2").await;

		// t=0: the first drains, due at t=5s.
		drop(first);
		tokio::time::sleep(Duration::from_secs(2)).await;
		h.assert_idle();

		// t=2s: the second drains, due at t=7s.
		drop(second);
		tokio::time::sleep(Duration::from_secs(4)).await;

		// t=6s: only the first has expired.
		match h.wire.take_announces().as_slice() {
			[lite::AnnounceBroadcast::Restart { id: 0, cost, .. }] => assert_eq!(*cost, lite::RouteCost(COLD)),
			other => panic!("expected only the first restore, got {other:?}"),
		}

		// t=8s: now the second's own deadline has passed.
		tokio::time::sleep(Duration::from_secs(2)).await;
		match h.wire.take_announces().as_slice() {
			[lite::AnnounceBroadcast::Restart { id: 1, cost, .. }] => assert_eq!(*cost, lite::RouteCost(COLD)),
			other => panic!("expected the second restore, got {other:?}"),
		}
	}

	/// An expired linger sends exactly one restart restoring the cold cost.
	#[tokio::test(start_paused = true)]
	async fn linger_expiry_restores_the_cold_cost() {
		let (mut h, track) = harness(true).await;

		drop(track);
		tokio::time::sleep(Duration::from_secs(6)).await;

		match h.wire.take_announces().as_slice() {
			[lite::AnnounceBroadcast::Restart { id: 0, cost, .. }] => assert_eq!(*cost, lite::RouteCost(COLD)),
			other => panic!("expected one cold-cost restart, got {other:?}"),
		}

		// The restore is a one-shot: the loop settles back to idle.
		tokio::time::sleep(Duration::from_secs(30)).await;
		h.assert_idle();
	}

	/// A route change during the linger supersedes the pending restore: the
	/// restart it triggers carries the new chain (and, with demand still gone,
	/// the cold cost), and the old deadline then passes without a second one.
	#[tokio::test(start_paused = true)]
	async fn route_change_supersedes_the_linger() {
		let (mut h, track) = harness(true).await;

		drop(track);
		tokio::time::sleep(Duration::from_secs(3)).await;
		h.wire.assert_quiet();

		// An upstream failover mid-linger: a new chain with the same first hop
		// (the same original publisher reached another way).
		let hops = OriginList::try_from(vec![Origin::new(9).unwrap(), Origin::new(12).unwrap()]).unwrap();
		h.source
			.set_route(
				crate::broadcast::Route::new()
					.with_hops(hops.clone())
					.with_cost(COLD)
					.with_announce(true),
			)
			.unwrap();
		settle().await;

		match h.wire.take_announces().as_slice() {
			[
				lite::AnnounceBroadcast::Restart {
					id: 0,
					hops: sent,
					cost,
				},
			] => {
				assert_eq!(sent, &hops);
				assert_eq!(*cost, lite::RouteCost(COLD));
			}
			other => panic!("expected the failover restart, got {other:?}"),
		}

		// The pending restore went with it: the old deadline passes silently.
		tokio::time::sleep(Duration::from_secs(30)).await;
		h.assert_idle();
	}

	/// The warm edge has no hysteresis: a viewer arriving on a cold
	/// advertisement re-prices to zero immediately.
	#[tokio::test(start_paused = true)]
	async fn demand_reprices_warm_immediately() {
		let (mut h, _) = harness(false).await;

		let _track = h.downstream.track("video").unwrap();
		settle().await;

		match h.wire.take_announces().as_slice() {
			[lite::AnnounceBroadcast::Restart { id: 0, cost, .. }] => assert_eq!(*cost, lite::RouteCost(0)),
			other => panic!("expected the warm restart, got {other:?}"),
		}
	}

	/// Spawn `run_announce` against the origin with the given `exclude_hop`,
	/// capturing the wire. The per-peer variant of the `harness` setup.
	fn spawn_announce(
		consumer: origin::Consumer,
		exclude_hop: u64,
	) -> (Wire, tokio::task::JoinHandle<Result<(), Error>>) {
		let log = Log::default();
		let writes = log.writes.clone();
		let mut stream = Stream::<SinkSession, Version> {
			writer: Writer::new(SinkSend::new(log), VERSION),
			reader: Reader::new(PendingRecv, VERSION),
		};
		let task = tokio::spawn(async move {
			let mut announced = consumer.announced();
			let self_origin = *consumer;
			Publisher::<SinkSession>::run_announce(
				&mut stream,
				&consumer,
				&mut announced,
				"",
				self_origin,
				exclude_hop,
				VERSION,
			)
			.await
		});
		(Wire { writes, cursor: 0 }, task)
	}

	/// A peer the active chain flows through receives the best clean standby
	/// instead of nothing, and at the standby's own cost: the warm discount
	/// applies only to the route we would actually serve everyone else from,
	/// since serving this peer means opening a fresh ingest.
	#[tokio::test(start_paused = true)]
	async fn excluded_peer_receives_the_standby() {
		let peer = Origin::new(33).unwrap();
		let origin = Origin::new(1).unwrap().produce();

		// Active: free, but its chain flows through the peer. Standby: the same
		// publisher reached directly, at its cold cost.
		let tainted = OriginList::try_from(vec![Origin::new(9).unwrap(), peer]).unwrap();
		let _a = origin
			.create_broadcast(
				"cam",
				crate::broadcast::Route::new()
					.with_hops(tainted.clone())
					.with_announce(true),
			)
			.unwrap();
		settle().await;
		let _b = origin
			.create_broadcast(
				"cam",
				crate::broadcast::Route::new()
					.with_hops(pub_hops())
					.with_cost(COLD)
					.with_announce(true),
			)
			.unwrap();
		settle().await;

		// A viewer warms the broadcast, so the serving route advertises zero.
		let downstream = origin.consume().announced_broadcast("cam").await.unwrap();
		let _track = downstream.track("video").unwrap();
		settle().await;

		// An ordinary peer gets the active route, discounted for the demand.
		let (mut wire, _task) = spawn_announce(origin.consume(), 0);
		settle().await;
		assert_eq!(wire.take_ok().active, 1);
		match wire.take_announces().as_slice() {
			[lite::AnnounceBroadcast::Active { hops, cost, .. }] => {
				assert_eq!(hops, &tainted);
				assert_eq!(*cost, lite::RouteCost(0));
			}
			other => panic!("expected the active route, got {other:?}"),
		}

		// The peer in the active chain gets the standby, undiscounted.
		let (mut wire, _task) = spawn_announce(origin.consume(), peer.id());
		settle().await;
		assert_eq!(wire.take_ok().active, 1);
		match wire.take_announces().as_slice() {
			[lite::AnnounceBroadcast::Active { hops, cost, .. }] => {
				assert_eq!(hops, &pub_hops());
				assert_eq!(*cost, lite::RouteCost(COLD));
			}
			other => panic!("expected the standby route, got {other:?}"),
		}
	}

	/// A relay carrying a broadcast via its peer initially has nothing to
	/// advertise to that peer; a standby with the same original publisher
	/// attaching later must go out as a fresh announce, giving the peer the
	/// route it needs to fail over (#2473, e2e finding 1). The active source
	/// dying afterward changes nothing on this stream: the standby is already
	/// the advertised route, so the failover is invisible to the peer.
	#[tokio::test(start_paused = true)]
	async fn standby_attach_announces_to_excluded_peer() {
		let peer = Origin::new(33).unwrap();
		let origin = Origin::new(1).unwrap().produce();

		// The active route: the broadcast as carried via the peer itself.
		let via_peer = OriginList::try_from(vec![Origin::new(9).unwrap(), peer]).unwrap();
		let source_a = origin
			.create_broadcast(
				"cam",
				crate::broadcast::Route::new().with_hops(via_peer).with_announce(true),
			)
			.unwrap();
		settle().await;

		// The peer sees nothing: its own hop is on the only route.
		let (mut wire, task) = spawn_announce(origin.consume(), peer.id());
		settle().await;
		assert_eq!(wire.take_ok().active, 0);
		wire.assert_quiet();

		// The standby (same first hop, reached directly) attaches later: a
		// fresh announce toward the peer.
		let _source_b = origin
			.create_broadcast(
				"cam",
				crate::broadcast::Route::new()
					.with_hops(pub_hops())
					.with_cost(COLD)
					.with_announce(true),
			)
			.unwrap();
		settle().await;
		match wire.take_announces().as_slice() {
			[lite::AnnounceBroadcast::Active { hops, cost, .. }] => {
				assert_eq!(hops, &pub_hops());
				assert_eq!(*cost, lite::RouteCost(COLD));
			}
			other => panic!("expected the standby announce, got {other:?}"),
		}

		// The active source dying promotes the standby locally; the peer's
		// advertisement is already that standby, so the wire stays quiet.
		source_a.abort(Error::Dropped).unwrap();
		settle().await;
		wire.assert_quiet();
		assert!(!task.is_finished(), "the announce loop ended unexpectedly");
	}

	/// The route swinging into the peer's chain retracts the announce (no clean
	/// standby remains), and swinging back out re-announces fresh.
	#[tokio::test(start_paused = true)]
	async fn retracts_when_route_swings_through_peer() {
		let peer = Origin::new(33).unwrap();
		let origin = Origin::new(1).unwrap().produce();
		let mut source = origin
			.create_broadcast(
				"cam",
				crate::broadcast::Route::new()
					.with_hops(pub_hops())
					.with_cost(COLD)
					.with_announce(true),
			)
			.unwrap();
		settle().await;

		let (mut wire, task) = spawn_announce(origin.consume(), peer.id());
		settle().await;
		assert_eq!(wire.take_ok().active, 1);
		match wire.take_announces().as_slice() {
			[lite::AnnounceBroadcast::Active { hops, .. }] => assert_eq!(hops, &pub_hops()),
			other => panic!("expected the initial announce, got {other:?}"),
		}

		// The same publisher, now reached through the peer: nothing left to
		// advertise to them.
		let through_peer = OriginList::try_from(vec![Origin::new(9).unwrap(), peer]).unwrap();
		source
			.set_route(
				crate::broadcast::Route::new()
					.with_hops(through_peer)
					.with_cost(COLD)
					.with_announce(true),
			)
			.unwrap();
		settle().await;
		match wire.take_announces().as_slice() {
			[lite::AnnounceBroadcast::EndedId { id: 0 }] => {}
			other => panic!("expected the retract, got {other:?}"),
		}

		// Swinging back out is a fresh announce with the next id.
		source
			.set_route(
				crate::broadcast::Route::new()
					.with_hops(pub_hops())
					.with_cost(COLD)
					.with_announce(true),
			)
			.unwrap();
		settle().await;
		match wire.take_announces().as_slice() {
			[lite::AnnounceBroadcast::Active { hops, .. }] => assert_eq!(hops, &pub_hops()),
			other => panic!("expected the re-announce, got {other:?}"),
		}
		assert!(!task.is_finished(), "the announce loop ended unexpectedly");
	}
}

/// Encode the per-frame timing prefix when the track advertises a timescale:
/// `[zigzag-delta timestamp]` (the lite-05 FRAME format). With `None` the field is
/// omitted entirely, saving the bytes on tracks where timing isn't meaningful
/// (catalogs, control channels, IETF transport).
///
/// `prev_ts` carries the running baseline, so the first frame deltas against 0. The
/// model layer (`group::Producer::create_frame`) already converted the timestamp
/// into the track timescale, so its raw value goes straight onto the wire. Mirrors
/// the decode in the subscriber's `run_group`.
async fn encode_frame_timing<W: web_transport_trait::SendStream>(
	writer: &mut Writer<W, Version>,
	timestamp: crate::Timestamp,
	timescale: Option<crate::Timescale>,
	prev_ts: &mut u64,
) -> Result<(), Error> {
	if timescale.is_none() {
		return Ok(());
	}

	encode_zigzag_delta(writer, timestamp.value(), prev_ts).await?;

	Ok(())
}

/// Encode `curr` as a zigzag-mapped varint delta against `*prev`, then advance
/// `*prev` to `curr`.
async fn encode_zigzag_delta<W: web_transport_trait::SendStream>(
	writer: &mut Writer<W, Version>,
	curr: u64,
	prev: &mut u64,
) -> Result<(), Error> {
	let delta: i64 = (curr as i128 - *prev as i128)
		.try_into()
		.map_err(|_| Error::BoundsExceeded(crate::coding::BoundsExceeded))?;
	let zz = crate::coding::VarInt::from_zigzag(delta).map_err(crate::coding::EncodeError::from)?;
	writer.encode(&zz).await?;
	*prev = curr;
	Ok(())
}

/// Write one frame to a fetch stream in the lite wire format: the optional timing
/// prefix (see [`encode_frame_timing`]), the size, then the payload. Mirrors the
/// per-frame encoding in [`Subscription::serve_frame`] without the priority
/// machinery, since a one-shot fetch carries a single static priority set on the
/// stream up front.
/// Write one already-complete frame of a fetched group.
async fn write_fetch_frame<W: web_transport_trait::SendStream>(
	writer: &mut Writer<W, Version>,
	frame: frame::Frame,
	timescale: Option<crate::Timescale>,
	prev_ts: &mut u64,
) -> Result<(), Error> {
	encode_frame_timing(writer, frame.timestamp, timescale, prev_ts).await?;

	writer.encode(&(frame.payload.len() as u64)).await?;
	if !frame.payload.is_empty() {
		writer.write_chunk(frame.payload).await?;
	}

	Ok(())
}

/// Write one in-flight frame of a fetched group, streaming its chunks as they land.
async fn write_fetch_partial<W: web_transport_trait::SendStream>(
	writer: &mut Writer<W, Version>,
	frame: &mut frame::Consumer,
	timescale: Option<crate::Timescale>,
	prev_ts: &mut u64,
) -> Result<(), Error> {
	encode_frame_timing(writer, frame.timestamp, timescale, prev_ts).await?;

	writer.encode(&frame.size).await?;
	while let Some(chunk) = frame.read_chunk().await? {
		writer.write_chunk(chunk).await?;
	}

	Ok(())
}

/// What a group has next for [`Publisher::next_frames`]: a batch of complete frames
/// waiting in the buffer, a consumer for the in-flight tail, or the end of the group.
enum Step {
	/// The buffer was refilled; its frames are the next ones to send.
	Batch,
	/// Nothing is complete yet, so stream the open tail chunk by chunk.
	Partial(frame::Consumer),
	/// The group ended.
	Done,
}

/// What [`recv_next`] pulled from the one subscriber: the next group to serve, the next
/// best-effort datagram to forward, the track declaring its exclusive final sequence, or
/// the track finishing (the live edge having reached that boundary).
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
/// governed by the latency window (cache expiry), not arrival raciness.
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

/// Shared per-subscription state for the publisher side. Cloned cheaply. Every
/// field is either small or already Arc-backed for each in-flight serve_group task
/// so each in-flight group reads the latest SUBSCRIBE_UPDATE priority via its own
/// consumer cursor.
#[derive(Clone)]
struct Subscription<S: web_transport_trait::Session> {
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

impl<S: web_transport_trait::Session> Subscription<S> {
	async fn run_track(
		mut self,
		mut track: track::Subscriber,
		start_group: Option<u64>,
		initial_end_group: Option<u64>,
		reader: &mut crate::coding::Reader<S::RecvStream, Version>,
		writer: &mut Writer<S::SendStream, Version>,
		track_priority_tx: &kio::Producer<u8>,
	) -> Result<(), Error> {
		let mut tasks: FuturesUnordered<MaybeSendBox<'static, ()>> = FuturesUnordered::new();

		// Start the consumer at the specified sequence, otherwise start at the latest group.
		if let Some(start_group) = start_group.or_else(|| track.latest()) {
			track.start_at(start_group);
		}

		// Apply the initial cap from the original Subscribe. Subsequent updates
		// flow through the SubscribeUpdate select arm below.
		track.end_at(initial_end_group);

		// Lite05+ resolves the range on the Subscribe Stream itself: SUBSCRIBE_START
		// once the first group is known, SUBSCRIBE_END as soon as the track declares its
		// exclusive final sequence (which may be ahead of the live edge).
		let emit_range = self.version.has_track_stream();
		let mut start_sent = false;
		let mut end_sent = false;

		// Serve datagrams off this same subscriber, but only on lite-05 over a datagram-capable
		// transport (qmux/WebSocket/TCP/UDS report size 0). No group fallback: otherwise off.
		let datagrams = self.version.has_datagrams() && self.session.max_datagram_size() > 0;

		// Transient one-at-a-time value; the padding is never held in bulk (see `Recv`).
		#[allow(clippy::large_enum_variant)]
		enum Event {
			Recv(Result<Recv, Error>),
			Update(Result<Option<lite::SubscribeUpdate>, Error>),
		}

		loop {
			let event = {
				let emit_boundary = emit_range && !end_sent;
				// SUBSCRIBE_UPDATE messages share this hot loop; safe because
				// decode_maybe is cancel-safe given quinn/qmux's cancel-safe
				// read primitives (see Reader::decode_maybe doc).
				let mut update = std::pin::pin!(reader.decode_maybe::<lite::SubscribeUpdate>());
				kio::wait(|waiter| {
					// Drive in-flight group futures; completions just drop.
					let mut cx = std::task::Context::from_waker(waiter.waker());
					while let Poll::Ready(Some(())) = tasks.poll_next_unpin(&mut cx) {}

					// Control first: SUBSCRIBE_UPDATE/FIN messages are rare, so they can't
					// starve the data path, while a deep group backlog polled first could
					// defer an unsubscribe or priority change indefinitely.
					if let Poll::Ready(upd) = waiter.poll_future(update.as_mut()) {
						return Poll::Ready(Event::Update(upd));
					}
					// One cursor drives the whole subscription: poll the cap-aware arrival-order
					// group and, when enabled, the next best-effort datagram. Groups are polled
					// first so a datagram burst can't starve them; datagrams flow whenever no
					// group is ready (including while groups are parked above the cap).
					if let Poll::Ready(res) = poll_recv_next(&mut track, datagrams, emit_boundary, waiter) {
						return Poll::Ready(Event::Recv(res));
					}
					Poll::Pending
				})
				.await
			};

			match event {
				Event::Recv(res) => match res? {
					Recv::Group(group) => {
						if emit_range && !start_sent {
							start_sent = true;
							// SUBSCRIBE_OK promises nothing below this sequence will be
							// delivered. Arrival-order serving could later surface a
							// straggler below the first group, so pin the floor to what
							// was announced.
							track.start_at(group.sequence);
							writer
								.encode(&lite::SubscribeResponse::Start(lite::SubscribeStart {
									group: group.sequence,
								}))
								.await?;
						}
						self.queue_serve(group, &mut tasks);
					}
					Recv::Datagram(datagram) => self.serve_datagram(datagram),
					Recv::Boundary(group) => {
						// The track declared its exclusive final sequence. Forward it now,
						// even if trailing groups (below `group`) are still in flight, then
						// keep serving them until the live edge reaches the boundary.
						end_sent = true;
						writer
							.encode(&lite::SubscribeResponse::End(lite::SubscribeEnd { group }))
							.await?;
					}
					Recv::Finished => {
						// The live edge reached the boundary; SUBSCRIBE_END was already sent
						// (or the version predates the track stream). Drain in-flight group
						// tasks and FIN by returning.
						while tasks.next().await.is_some() {}
						return Ok(());
					}
				},
				Event::Update(upd) => {
					let Some(upd) = upd? else {
						// Peer FIN'd. They're done with this subscription. Drop any
						// in-flight serve_group tasks (don't drain) so half-sent
						// groups get cancelled rather than completed pointlessly.
						return Ok(());
					};
					if let Ok(mut value) = track_priority_tx.write() {
						*value = upd.priority;
					}
					// Feed the full update into the model subscriber so the producer's
					// aggregate reflects it (and a relay re-forwards it upstream).
					let _ = track.update(crate::track::Subscription {
						priority: upd.priority,
						ordered: upd.ordered,
						latency_max: upd.max_latency,
						group_start: upd.start_group,
						group_end: upd.end_group,
						..Default::default()
					});
					if let Some(start_group) = upd.start_group {
						track.start_at(start_group);
					}
					track.end_at(upd.end_group);
				}
			}
		}
	}

	fn queue_serve(&mut self, group: group::Consumer, tasks: &mut FuturesUnordered<MaybeSendBox<'static, ()>>) {
		let sequence = group.sequence;
		tracing::debug!(subscribe = self.id, track = %self.track_name, sequence, "serving group");

		// Use the latest priority for new groups so SUBSCRIBE_UPDATE applies to them too.
		let current_priority = self.track_priority_current();
		let handle = self.priority.insert(Priority::new(current_priority, sequence));
		let fut = self.clone().serve_group(sequence, handle, group);
		tasks.push(fut.map(|_| ()).maybe_boxed());
	}

	async fn serve_group(
		mut self,
		sequence: u64,
		mut priority: PriorityHandle,
		mut group: group::Consumer,
	) -> Result<(), Error> {
		let msg = lite::Group {
			subscribe: self.id,
			sequence,
		};
		let stream = self.session.open_uni().await.map_err(Error::from_transport)?;
		let mut stream = Writer::new(stream, self.version);

		if let Err(err) = self.write_group(&mut stream, &msg, &mut priority, &mut group).await {
			// Reset with the real reason (Old, Lagged, Evicted, ...) so the subscriber can
			// tell a truncated group from a routine cancel. Without this the Writer's Drop
			// fallback reports every failure as Cancel.
			stream.abort(&err);
			return Err(err);
		}

		// Consume the writer: close() waits for the peer to acknowledge everything,
		// and taking ownership disarms the Drop fallback that would otherwise reset
		// the finished stream with a spurious Cancel.
		stream.close().await?;

		tracing::debug!(sequence, "finished group");

		Ok(())
	}

	/// Write the group header and every frame, leaving the stream open for the caller to
	/// finish or abort.
	async fn write_group(
		&mut self,
		stream: &mut Writer<S::SendStream, Version>,
		msg: &lite::Group,
		priority: &mut PriorityHandle,
		group: &mut group::Consumer,
	) -> Result<(), Error> {
		stream.set_priority(priority.send_order());
		stream.encode(&lite::DataType::Group).await?;
		stream.encode(msg).await?;

		// Lite05+ delta-encodes per-frame timestamps within the group. The first
		// frame's delta is absolute (against an implicit prev value of 0), every
		// subsequent delta is signed against the previous frame.
		let mut prev_ts: u64 = 0;
		let mut buf = frame::Buffer::new();
		loop {
			match self.next_frames(stream, priority, group, &mut buf).await? {
				Step::Batch => {
					for i in 0..buf.filled().len() {
						// Index rather than iterate: the writes below borrow `self`
						// mutably, and a live `buf.filled()` iterator would pin `buf`
						// across them for no reason.
						let frame = buf.filled()[i].clone();
						self.serve_whole_frame(stream, priority, frame, &mut prev_ts).await?;
						// The fill stamped the group once. A flow-controlled peer can
						// take longer than `latency_max` to accept one batch, so keep
						// stamping or the rest of the group expires mid-serve.
						group.keep_alive();
					}
				}
				Step::Partial(frame) => self.serve_frame(stream, priority, frame, &mut prev_ts).await?,
				Step::Done => break,
			}
		}

		Ok(())
	}

	/// Send one datagram best-effort over a QUIC datagram (lite-05 §6.4).
	///
	/// The datagram is dropped (there is no group fallback) if the encoded body doesn't fit the
	/// transport's datagram limit or the send fails (congestion / no capacity right now).
	fn serve_datagram(&self, datagram: crate::Datagram) {
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

		let _ = self.session.send_datagram(body);
	}

	/// Send one frame: the size, then the payload streamed chunk-by-chunk so we
	/// never buffer the whole thing.
	async fn serve_frame(
		&mut self,
		stream: &mut Writer<S::SendStream, Version>,
		priority: &mut PriorityHandle,
		mut frame: frame::Consumer,
		prev_ts: &mut u64,
	) -> Result<(), Error> {
		encode_frame_timing(stream, frame.timestamp, self.timescale, prev_ts).await?;

		stream.encode(&frame.size).await?;

		while let Some(chunk) = self.read_chunk(stream, priority, &mut frame).await? {
			self.write_chunk(stream, priority, chunk).await?;
		}

		Ok(())
	}

	/// Await whatever the group has next: a batch of already-complete frames, or a
	/// consumer for the in-flight tail when nothing is complete yet.
	///
	/// A subscriber catching up on a cached group takes the whole backlog under one
	/// lock instead of one wait per frame. At the live edge the batch comes up empty
	/// and the tail streams chunk by chunk, exactly as it did before, so forwarding
	/// never waits for a frame to complete.
	async fn next_frames(
		&mut self,
		stream: &mut Writer<S::SendStream, Version>,
		priority: &mut PriorityHandle,
		group: &mut group::Consumer,
		buf: &mut frame::Buffer,
	) -> Result<Step, Error> {
		Self::serve_step(
			stream,
			priority,
			&self.track_priority,
			&mut self.track_priority_seen,
			|waiter| match group.poll_read_frames(waiter, buf) {
				// Nothing complete: the tail is either in flight or the group ended.
				Poll::Pending => group
					.poll_next_frame(waiter)
					.map_ok(|frame| frame.map_or(Step::Done, Step::Partial)),
				res => res.map_ok(|count| if count == 0 { Step::Done } else { Step::Batch }),
			},
		)
		.await
	}

	/// Send one already-complete frame: the timestamp, the size, then the payload.
	async fn serve_whole_frame(
		&mut self,
		stream: &mut Writer<S::SendStream, Version>,
		priority: &mut PriorityHandle,
		frame: frame::Frame,
		prev_ts: &mut u64,
	) -> Result<(), Error> {
		encode_frame_timing(stream, frame.timestamp, self.timescale, prev_ts).await?;
		stream.encode(&(frame.payload.len() as u64)).await?;
		if !frame.payload.is_empty() {
			self.write_chunk(stream, priority, frame.payload).await?;
		}
		Ok(())
	}

	/// Await the next chunk of `frame`, applying priority changes meanwhile.
	async fn read_chunk(
		&mut self,
		stream: &mut Writer<S::SendStream, Version>,
		priority: &mut PriorityHandle,
		frame: &mut frame::Consumer,
	) -> Result<Option<bytes::Bytes>, Error> {
		Self::serve_step(
			stream,
			priority,
			&self.track_priority,
			&mut self.track_priority_seen,
			|waiter| frame.poll_read_chunk(waiter),
		)
		.await
	}

	/// Poll `work` to completion while applying queue and SUBSCRIBE_UPDATE priority
	/// changes to the stream. Errors with [`Error::Cancel`] if the peer closes first.
	async fn serve_step<T>(
		stream: &mut Writer<S::SendStream, Version>,
		priority: &mut PriorityHandle,
		track_priority: &kio::Consumer<u8>,
		track_priority_seen: &mut u8,
		mut work: impl FnMut(&kio::Waiter) -> Poll<Result<T, Error>>,
	) -> Result<T, Error> {
		enum Event<T> {
			Closed,
			Work(Result<T, Error>),
			/// The handle's rank changed; the new value is re-read via
			/// [`PriorityHandle::send_order`] when handled.
			Priority,
			TrackPriority(u8),
		}

		loop {
			let event = {
				let mut closed = std::pin::pin!(stream.closed());
				let seen = *track_priority_seen;
				kio::wait(|waiter| {
					if waiter.poll_future(closed.as_mut()).is_ready() {
						return Poll::Ready(Event::Closed);
					}
					if let Poll::Ready(res) = work(waiter) {
						return Poll::Ready(Event::Work(res));
					}
					if priority.poll_next(waiter).is_ready() {
						return Poll::Ready(Event::Priority);
					}
					// A dropped producer just disables this arm, like the queue arm above.
					match track_priority.poll(waiter, |value| {
						if **value != seen {
							Poll::Ready(**value)
						} else {
							Poll::Pending
						}
					}) {
						Poll::Ready(Ok(value)) => Poll::Ready(Event::TrackPriority(value)),
						Poll::Ready(Err(_)) | Poll::Pending => Poll::Pending,
					}
				})
				.await
			};

			match event {
				Event::Closed => return Err(Error::Cancel),
				Event::Work(res) => return res,
				Event::Priority => stream.set_priority(priority.send_order()),
				Event::TrackPriority(new_track) => {
					*track_priority_seen = new_track;
					priority.set_track(new_track);
				}
			}
		}
	}

	/// Read the latest SUBSCRIBE_UPDATE track priority, marking it seen.
	fn track_priority_current(&mut self) -> u8 {
		self.track_priority_seen = *self.track_priority.read();
		self.track_priority_seen
	}

	/// Write a whole chunk at the current priority.
	async fn write_chunk(
		&mut self,
		stream: &mut Writer<S::SendStream, Version>,
		priority: &mut PriorityHandle,
		chunk: bytes::Bytes,
	) -> Result<(), Error> {
		self.apply_priority(stream, priority);
		stream.write_chunk(chunk).await
	}

	fn apply_priority(&mut self, stream: &mut Writer<S::SendStream, Version>, priority: &mut PriorityHandle) {
		let track_priority = self.track_priority_current();
		priority.set_track(track_priority);
		stream.set_priority(priority.send_order());
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

		let handle = subscription.priority.insert(Priority::new(0, 0));
		let mut serve = std::pin::pin!(subscription.serve_group(0, handle, group.consume()));

		// Drain the frame, leaving the task parked awaiting the next one.
		assert!(futures::poll!(serve.as_mut()).is_pending());

		// The group is dropped from the cache mid-stream: a truncated group, not a cancel.
		group.abort(Error::Old).unwrap();

		assert!(matches!(serve.await, Err(Error::Old)));
		assert_eq!(log.resets(), vec![Error::Old.to_code()]);
	}

	/// `write_group` takes two routes to the wire: complete frames come out of a batch
	/// read via `serve_whole_frame`, while an in-flight frame streams chunk by chunk
	/// via `serve_frame`. The two must encode identically, or a subscriber catching up
	/// sees different bytes than one at the live edge.
	#[tokio::test]
	async fn batched_and_streamed_frames_encode_identically() {
		const FRAMES: usize = 12;
		const PAYLOAD: usize = 7;

		fn subscription(log: &Log) -> Subscription<SinkSession> {
			let track_priority = kio::Producer::new(0u8);
			Subscription {
				session: SinkSession::new(log.clone()),
				id: 0,
				track_name: "test".into(),
				priority: PriorityQueue::default(),
				track_priority: track_priority.consume(),
				track_priority_seen: 0,
				version: Version::Lite06Wip,
				timescale: Some(crate::Timescale::default()),
			}
		}

		fn payload(i: usize) -> Vec<u8> {
			vec![i as u8; PAYLOAD]
		}

		fn timestamp(i: usize) -> Timestamp {
			Timestamp::from_millis(i as u64 * 10).unwrap()
		}

		// Every frame complete before serving starts: the batch read takes them all.
		let batched = {
			let log = Log::default();
			let subscription = subscription(&log);
			let mut track = track::Producer::new(Arc::new(broadcast::Info::default()), "test", None);
			let mut group = track.create_group(group::Info { sequence: 0 }).unwrap();
			for i in 0..FRAMES {
				group.write_frame(timestamp(i), payload(i).as_slice()).unwrap();
			}
			let consumer = group.consume();
			group.finish().unwrap();

			let handle = subscription.priority.insert(Priority::new(0, 0));
			subscription.serve_group(0, handle, consumer).await.unwrap();
			log.writes.lock().unwrap().clone()
		};

		// Every frame still open when the publisher reaches it, so each one streams a
		// chunk at a time down the `Step::Partial` path.
		let streamed = {
			let log = Log::default();
			let subscription = subscription(&log);
			let mut track = track::Producer::new(Arc::new(broadcast::Info::default()), "test", None);
			let mut group = track.create_group(group::Info { sequence: 0 }).unwrap();
			let consumer = group.consume();

			let handle = subscription.priority.insert(Priority::new(0, 0));
			let mut serve = std::pin::pin!(subscription.serve_group(0, handle, consumer));
			// Past the group header, parked with nothing to send.
			assert!(futures::poll!(serve.as_mut()).is_pending());

			for i in 0..FRAMES {
				{
					let mut frame = group
						.create_frame(frame::Info {
							size: PAYLOAD as u64,
							timestamp: timestamp(i),
						})
						.unwrap();
					// The publisher is parked on this frame, so each chunk is forwarded
					// before the next one is written.
					for byte in payload(i) {
						frame.write(&[byte][..]).unwrap();
						assert!(futures::poll!(serve.as_mut()).is_pending());
					}
					frame.finish().unwrap();
				}
				assert!(futures::poll!(serve.as_mut()).is_pending());
			}

			group.finish().unwrap();
			serve.await.unwrap();
			log.writes.lock().unwrap().clone()
		};

		assert!(!batched.is_empty(), "the group produced no bytes");
		assert_eq!(batched, streamed, "batched and streamed writes must encode the same");
	}

	/// A frame still being written must reach the wire as it fills rather than waiting
	/// for the whole thing: the batch read has to yield to the open tail.
	#[tokio::test]
	async fn an_open_frame_streams_before_it_completes() {
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
		let consumer = group.consume();

		// A large frame, opened but far from complete.
		let mut frame = group
			.create_frame(frame::Info {
				size: 4096,
				timestamp: Timestamp::from_millis(0).unwrap(),
			})
			.unwrap();
		frame.write(&[7u8; 512][..]).unwrap();

		let handle = subscription.priority.insert(Priority::new(0, 0));
		let mut serve = std::pin::pin!(subscription.serve_group(0, handle, consumer));
		// Let it run until it blocks on the rest of the frame.
		assert!(futures::poll!(serve.as_mut()).is_pending());

		let written = log.writes.lock().unwrap().len();
		assert!(
			written >= 512,
			"the open frame's first chunk must be forwarded before it completes, wrote {written}"
		);
	}

	/// A group that completes cleanly must not reset at all. The completion path
	/// consumes the writer via `close()`; leaving the writer to drop after `finish()`
	/// would fire the Drop fallback and tack a spurious Cancel reset onto a stream
	/// the peer already acknowledged.
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

		let handle = subscription.priority.insert(Priority::new(0, 0));
		subscription.serve_group(0, handle, consumer).await.unwrap();

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

	/// A subscriber that stops reading must not pin the group it was being served.
	///
	/// The publisher stamps the group's cache access once per frame, immediately
	/// before writing it, so a delivery in progress gets a full `latency_max` of
	/// grace per frame handed out. Nothing re-stamps inside the write itself: a peer
	/// whose flow control window stays shut for longer than the whole retention
	/// window lets the group expire mid-stream and the stream resets with `Old`.
	/// That is the point. Holding the group for as long as a wedged peer refuses to
	/// read would let any subscriber pin cache indefinitely.
	///
	/// A batch read takes up to `frame::Buffer` frames out of the group at once and
	/// owns their payloads, so the grace is that many frames rather than one. It is
	/// still bounded: the tail past the buffer goes with the group, which is why the
	/// group here runs longer than one batch.
	#[tokio::test(start_paused = true)]
	async fn stalled_write_releases_the_group() {
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
		let mut group = track.create_group(group::Info { sequence: 0 }).unwrap();
		group
			.write_frame(Timestamp::from_millis(0).unwrap(), b"first".as_slice())
			.unwrap();

		// The live edge moves on, so the served group is demoted and expirable.
		track
			.create_group(group::Info { sequence: 1 })
			.unwrap()
			.finish()
			.unwrap();

		let handle = subscription.priority.insert(Priority::new(0, 0));
		let mut serve = std::pin::pin!(subscription.serve_group(0, handle, group.consume()));

		// Write the header and the first frame, leaving the task awaiting the next.
		assert!(futures::poll!(serve.as_mut()).is_pending());

		// From here every write blocks, the way a shut flow control window does.
		*gate.write().ok().expect("gate open") = false;

		group
			.write_frame(Timestamp::from_millis(10).unwrap(), b"second".as_slice())
			.unwrap();
		// Enough filler to overrun the publisher's batch, so the tail below is left in
		// the group rather than taken along with "second".
		let batch = <frame::Buffer>::new().capacity();
		for i in 0..batch {
			group
				.write_frame(Timestamp::from_millis(11 + i as u64).unwrap(), b"pad".as_slice())
				.unwrap();
		}
		group
			.write_frame(Timestamp::from_millis(20).unwrap(), b"third".as_slice())
			.unwrap();
		group.finish().unwrap();

		// The publisher takes a batch ending at the filler (stamping the group) and
		// blocks writing "second".
		assert!(futures::poll!(serve.as_mut()).is_pending());

		// The write stays blocked well past the retention window while the source
		// keeps publishing, which is what runs the expiry scan.
		for sequence in 2..8u64 {
			tokio::time::advance(crate::track::DEFAULT_LATENCY_MAX / 2).await;
			track.create_group(group::Info { sequence }).unwrap().finish().unwrap();
			assert!(futures::poll!(serve.as_mut()).is_pending());
		}

		*gate.write().ok().expect("gate open") = true;
		let res = serve.await;
		assert!(
			matches!(res, Err(Error::Old)),
			"a wedged peer must not hold an expired group open: {res:?}"
		);

		// The reason reaches the peer, so it reads as a truncated group rather than
		// a routine cancel and it can re-request the sequence.
		assert_eq!(log.resets(), vec![Error::Old.to_code()]);

		// Only the untaken tail is lost: the frames already handed to the publisher
		// own their payloads, so the release can't reclaim them mid-write.
		let writes = log.writes.lock().unwrap();
		assert!(
			writes.windows(b"second".len()).any(|w| w == b"second"),
			"the in-flight batch still reached the wire"
		);
		assert!(
			!writes.windows(b"third".len()).any(|w| w == b"third"),
			"the untaken tail was released with the group"
		);
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::lite::test_transport::SinkSession;

	/// A peer that declares no origin in its SETUP is split-horizoned by the identity
	/// the caller assigned it, on the data plane and not just the announce filter.
	/// Otherwise it can subscribe its way back to content that already flowed through
	/// it, which is the loop the announce filter exists to prevent.
	#[tokio::test(start_paused = true)]
	async fn serving_origin_falls_back_to_the_assigned_identity() {
		let assigned = crate::Origin::new(777).unwrap();
		let upstream = crate::Origin::new(778).unwrap();
		let origin = crate::origin::Info::new(crate::Origin::new(1).unwrap()).produce();

		let mut echoed_hops = OriginList::new();
		echoed_hops.push(assigned).unwrap();
		let _echoed = origin
			.create_broadcast(
				"echoed",
				crate::broadcast::Route::new()
					.with_hops(echoed_hops)
					.with_announce(true),
			)
			.unwrap();

		let mut local_hops = OriginList::new();
		local_hops.push(upstream).unwrap();
		let _local = origin
			.create_broadcast(
				"local",
				crate::broadcast::Route::new().with_hops(local_hops).with_announce(true),
			)
			.unwrap();

		// Broadcast visibility is deferred until the executor ticks.
		tokio::time::sleep(std::time::Duration::from_millis(1)).await;

		// A SETUP that declares no origin of its own, so only the assigned one applies.
		let peer_setup = crate::lite::PeerSetup::default();
		peer_setup.set(crate::lite::Setup::default());

		let publisher = Publisher::new(PublisherConfig {
			session: SinkSession::new(Default::default()),
			origin: origin.consume(),
			version: Version::Lite06Wip,
			peer_setup,
			peer_origin: Some(assigned),
		});

		let serving = publisher.serving_origin().await;
		assert!(
			serving.get_broadcast("echoed").is_none(),
			"served the peer its own route"
		);
		assert!(
			serving.get_broadcast("local").is_some(),
			"withheld an independent route"
		);
	}

	/// Lite01/02 send the initial active set as ANNOUNCE_INIT. It must apply the
	/// same per-peer route selection as the live loop: a broadcast whose only
	/// route flows through the excluded hop (here the peer's assigned identity,
	/// `Client::with_peer_origin`) is filtered from the initial set too.
	#[tokio::test]
	async fn announce_init_applies_route_selection() {
		let assigned = crate::Origin::new(777).unwrap();
		let clean_publisher = crate::Origin::new(778).unwrap();
		let self_origin = crate::Origin::new(1).unwrap();
		let origin = crate::origin::Info::new(self_origin).produce();

		let mut tainted_hops = OriginList::new();
		tainted_hops.push(assigned).unwrap();
		let _tainted = origin
			.create_broadcast(
				"echoed",
				crate::broadcast::Route::new()
					.with_hops(tainted_hops)
					.with_announce(true),
			)
			.unwrap();

		let mut clean_hops = OriginList::new();
		clean_hops.push(clean_publisher).unwrap();
		let _clean = origin
			.create_broadcast(
				"local",
				crate::broadcast::Route::new().with_hops(clean_hops).with_announce(true),
			)
			.unwrap();

		// Broadcast visibility is deferred until the executor ticks.
		tokio::time::sleep(std::time::Duration::from_millis(1)).await;

		let gate = kio::Producer::new(true);
		let session = SinkSession::gated_bi(gate.consume());
		let log = session.log.clone();
		let mut stream = Stream::open(&session, Version::Lite01).await.unwrap();

		let consumer = origin.consume();
		let mut announced = consumer.announced();
		let mut run = std::pin::pin!(Publisher::<SinkSession>::run_announce(
			&mut stream,
			&consumer,
			&mut announced,
			crate::Path::new(""),
			self_origin,
			assigned.id(),
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

	/// Drive `run_probe` against a transport reporting `stats`, and return whatever
	/// it wrote before parking.
	async fn probe_writes(stats: crate::lite::test_transport::SinkStats) -> Vec<lite::Probe> {
		probe_writes_version(stats, Version::Lite05).await
	}

	/// As above, on a specific negotiated version.
	async fn probe_writes_version(stats: crate::lite::test_transport::SinkStats, version: Version) -> Vec<lite::Probe> {
		let gate = kio::Producer::new(true);
		let session = SinkSession::gated_bi(gate.consume()).with_stats(stats);
		let log = session.log.clone();
		let mut stream = Stream::open(&session, version).await.unwrap();

		let mut run = std::pin::pin!(Publisher::<SinkSession>::run_probe(&session, &mut stream, version));
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
		let session = SinkSession::gated_bi(gate.consume()).with_stats(stats);
		let log = session.log.clone();
		let mut stream = Stream::open(&session, Version::Lite05).await.unwrap();

		let mut run = std::pin::pin!(Publisher::<SinkSession>::run_probe(
			&session,
			&mut stream,
			Version::Lite05
		));
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
