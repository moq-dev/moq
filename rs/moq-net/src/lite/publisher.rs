use crate::{announce, frame, group, origin, stats, track};
use std::{sync::Arc, task::Poll, time::Duration};

use bytes::Buf;
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
	path: crate::PathOwned,
	sent: Option<SentRoute>,
}

/// What the peer currently holds for a path: the forwarded hop chain plus, on
/// lite-06+, the route cost. A fresh route that differs in either is worth a wire
/// message; one that matches is not.
#[derive(Clone, PartialEq, Eq)]
struct SentRoute {
	hops: OriginList,
	cost: lite::RouteCost,
}

pub(super) struct PublisherConfig<S: web_transport_trait::Session> {
	pub session: S,
	/// The origin we read local broadcasts from.
	pub origin: origin::Consumer,
	/// Stats aggregator for this session's egress. Use [`stats::Handle::default`]
	/// to opt out.
	pub stats: stats::Handle,
	pub version: Version,
}

pub(super) struct Publisher<S: web_transport_trait::Session> {
	session: S,
	origin: origin::Consumer,
	stats: stats::Handle,
	/// Per-session egress broadcast-subscription tracker. Each downstream
	/// subscription holds a guard so `broadcasts - broadcasts_closed` counts
	/// the distinct sessions (viewers) watching each broadcast.
	broadcasts: stats::SessionBroadcasts,
	self_origin: Origin,
	priority: PriorityQueue,
	version: Version,
}

impl<S: web_transport_trait::Session> Publisher<S> {
	pub fn new(config: PublisherConfig<S>) -> Self {
		// Identity stamped onto outbound announce hops. Derived from the
		// origin we're consuming so it matches the local relay identity
		// across every session, required for cross-session loop detection.
		let self_origin = *config.origin;
		let broadcasts = config.stats.publisher_broadcasts();
		Self {
			session: config.session,
			origin: config.origin,
			stats: config.stats,
			broadcasts,
			self_origin,
			priority: Default::default(),
			version: config.version,
		}
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

	async fn run_probe(session: &S, stream: &mut Stream<S, Version>, _version: Version) -> Result<(), Error> {
		const PROBE_INTERVAL: Duration = Duration::from_millis(100);
		const PROBE_MAX_AGE: Duration = Duration::from_secs(10);
		const PROBE_MAX_DELTA: f64 = 0.25;

		let mut last_sent: Option<(u64, web_async::time::Instant)> = None;
		let mut interval = web_async::time::interval(PROBE_INTERVAL);

		loop {
			// Tick the probe interval, bailing as soon as the peer closes its side.
			let closed = {
				let mut closed = std::pin::pin!(stream.reader.closed());
				let mut tick = std::pin::pin!(interval.tick());
				kio::wait(|waiter| {
					if let Poll::Ready(res) = waiter.poll_future(closed.as_mut()) {
						return Poll::Ready(Some(res));
					}
					waiter.poll_future(tick.as_mut()).map(|_| None)
				})
				.await
			};
			if let Some(res) = closed {
				return res;
			}

			let Some(bitrate) = session.stats().estimated_send_rate() else {
				continue;
			};

			let should_send = match last_sent {
				None => true,
				Some((0, _)) => bitrate > 0,
				Some((prev, at)) => {
					let elapsed = at.elapsed().as_secs_f64();
					let t = elapsed.clamp(PROBE_INTERVAL.as_secs_f64(), PROBE_MAX_AGE.as_secs_f64());
					let range = PROBE_MAX_AGE.as_secs_f64() - PROBE_INTERVAL.as_secs_f64();
					let threshold = PROBE_MAX_DELTA * (PROBE_MAX_AGE.as_secs_f64() - t) / range;
					let change = (bitrate as f64 - prev as f64).abs() / prev as f64;
					change >= threshold
				}
			};

			if should_send {
				let rtt = session.stats().rtt().map(|d| d.as_millis() as u64);
				stream.writer.encode(&lite::Probe { bitrate, rtt }).await?;
				last_sent = Some((bitrate, web_async::time::Instant::now()));
			}
		}
	}

	pub async fn recv_announce(&self, mut stream: Stream<S, Version>) -> Result<(), Error> {
		let interest = stream.reader.decode::<lite::AnnounceRequest>().await?;
		let prefix = interest.prefix.to_owned();
		let exclude_hop = interest.exclude_hop;

		// If the requested prefix is outside our scope (an empty origin, or a token
		// that doesn't grant it), we simply have nothing to announce. Respond with an
		// empty set and keep the stream open (the subscriber treats a FIN here as a
		// fatal stream close), rather than erroring, which would reset the stream.
		let origin = self
			.origin
			.scope(&[prefix.as_path()])
			.unwrap_or_else(|| self.origin.empty());
		let mut announced = origin.announced();

		if let Err(err) = Self::run_announce(
			&mut stream,
			&origin,
			&mut announced,
			&prefix,
			self.self_origin,
			exclude_hop,
			self.stats.clone(),
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
		// Peer's session-level origin id, sent in AnnounceInterest. We skip
		// forwarding announces whose hop chain already contains this id, so
		// reflected announces (cluster loops) never hit the wire. Zero means
		// the peer didn't set it (Lite03 or earlier), pass through.
		exclude_hop: u64,
		stats: stats::Handle,
		version: Version,
	) -> Result<(), Error> {
		let prefix = prefix.as_path();

		// Per-path stats guards: dropping the guard records `broadcasts_closed`.
		// The origin contract guarantees announce/unannounce toggles per path, so a
		// new active announcement must always be for a path with no live guard.
		let mut stats_guards: std::collections::HashMap<crate::PathOwned, stats::Publisher> =
			std::collections::HashMap::new();

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

					if broadcast.is_some() {
						tracing::debug!(broadcast = %absolute, "announce");
						let guard = stats.broadcast(&absolute).publisher();
						stats_guards.entry(absolute).or_insert(guard);
						if !init.contains(&suffix) {
							init.push(suffix);
						}
					} else {
						// A potential race.
						tracing::debug!(broadcast = %absolute, "unannounce");
						stats_guards.remove(&absolute);
						init.retain(|p| p != &suffix);
					}
				}

				let announce_init = lite::AnnounceInit { suffixes: init };
				stream.writer.encode(&announce_init).await?;

				// AnnounceInit batches the initial active set into one message; attribute
				// it per broadcast by name length so Lite01/02 isn't undercounted.
				for absolute in stats_guards.keys() {
					stats
						.broadcast(absolute)
						.publisher_announced_bytes(absolute.as_str().len() as u64);
				}
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
							let route = broadcast.route();
							let hops = route.hops.clone();
							let cost = Self::outgoing_cost(version, &broadcast, &route);
							// Watch even the announces we filter below: a later route update
							// can cross the forwarding filter in either direction.
							watched.insert(
								suffix.clone(),
								WatchedRoute {
									consumer: broadcast.clone(),
									path: path.clone(),
									sent: None,
								},
							);
							// Apply the same exclude_hop and reflected-announce skips as the live
							// loop so the count matches exactly what we send (minus the self push).
							if exclude_hop != 0 && hops.iter().any(|h| h.id() == exclude_hop) {
								continue;
							}
							if hops.contains(&self_origin) {
								continue;
							}
							tracing::debug!(broadcast = %absolute, "announce");
							let guard = stats.broadcast(&absolute).publisher();
							stats_guards.entry(absolute.clone()).or_insert(guard);
							initial.retain(|(s, _)| s != &suffix);
							initial.push((suffix, SentRoute { hops, cost }));
						}
						None => {
							// A potential race: a just-announced path already unannounced.
							tracing::debug!(broadcast = %absolute, "unannounce");
							stats_guards.remove(&absolute);
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

				// Count each initial announce by broadcast name length, mirroring the
				// live loop below (the name, not the encoded message size).
				for absolute in stats_guards.keys() {
					stats
						.broadcast(absolute)
						.publisher_announced_bytes(absolute.as_str().len() as u64);
				}
			}
			_ => {
				// Lite03/Lite04: no announce init, no AnnounceOk.
			}
		}

		// One announce-loop turn: either an (un)announce from the origin, a route
		// change on an already-announced broadcast, or the periodic cost recheck.
		// Resolved outside the select so the handlers below can freely mutate the
		// maps its futures borrow.
		enum Op {
			Announce(Option<crate::announce::Update>),
			Route(crate::PathOwned, Result<crate::broadcast::Route, Error>),
			Recheck,
		}

		// How often to re-evaluate the advertised cost of every watched broadcast.
		// The cost depends on liveness (`is_active`), which is not part of the
		// route: going active wakes this loop anyway (minting the track writes the
		// broadcast state), but the last consumer dropping does not, so decay back
		// to the cold cost rides this tick. The period is deliberately coarse: it
		// doubles as hysteresis, keeping a briefly-idle broadcast advertising zero
		// so viewer churn doesn't flap the mesh's routing.
		const COST_RECHECK: Duration = Duration::from_secs(5);
		let mut recheck = version
			.has_route_cost()
			.then(|| web_async::time::interval(COST_RECHECK));

		// Send updates as they arrive. Closure wins the race so a dead peer can't
		// stall on a busy announce feed.
		loop {
			let op = {
				let mut closed = std::pin::pin!(stream.reader.closed());
				// Pending forever when the version carries no cost on the wire.
				let mut tick = std::pin::pin!(async {
					match recheck.as_mut() {
						Some(interval) => interval.tick().await,
						None => std::future::pending().await,
					}
				});
				kio::wait(|waiter| {
					if let Poll::Ready(res) = waiter.poll_future(closed.as_mut()) {
						return Poll::Ready(Err(res));
					}
					if let Poll::Ready(next) = announced.poll_next(waiter) {
						return Poll::Ready(Ok(Op::Announce(next)));
					}
					// Poll every watched broadcast for a route change; each wake
					// rescans the map, which announce-control rates make fine.
					for (suffix, entry) in watched.iter_mut() {
						if let Poll::Ready(res) = entry.consumer.poll_route_changed(waiter) {
							return Poll::Ready(Ok(Op::Route(suffix.clone(), res)));
						}
						// The advertised cost drifts without a route change when
						// liveness flips; reuse the route path to re-send it.
						if version.has_route_cost()
							&& let Some(sent) = &entry.sent
						{
							let route = entry.consumer.route();
							if Self::outgoing_cost(version, &entry.consumer, &route) != sent.cost {
								return Poll::Ready(Ok(Op::Route(suffix.clone(), Ok(route))));
							}
						}
					}
					if waiter.poll_future(tick.as_mut()).is_ready() {
						return Poll::Ready(Ok(Op::Recheck));
					}
					Poll::Pending
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
							let route = active.route();
							if lite::restart_supported(version) {
								// Watch even if filtered below: a route update can cross
								// the forwarding filter in either direction.
								watched.insert(
									suffix.clone(),
									WatchedRoute {
										consumer: active.clone(),
										path: path.clone(),
										sent: None,
									},
								);
							}
							let Some(hops) =
								Self::prepare_active_hops(&route.hops, self_origin, exclude_hop, version, &absolute)
							else {
								continue;
							};
							let cost = Self::outgoing_cost(version, &active, &route);
							tracing::debug!(broadcast = %absolute, "announce");
							let bs = stats.broadcast(&absolute);
							// Count the broadcast name length, not the encoded message size, so
							// stats don't penalize the broadcast for hop/framing overhead.
							bs.publisher_announced_bytes(absolute.as_str().len() as u64);
							let prev = stats_guards.insert(absolute.clone(), bs.publisher());
							debug_assert!(prev.is_none(), "origin announced a path that was already active");
							if version.has_announce_id() {
								let prev = announce_ids.insert(suffix.clone(), next_announce_id);
								debug_assert!(prev.is_none(), "announce id still assigned for a new announce");
								next_announce_id += 1;
							}
							if let Some(entry) = watched.get_mut(&suffix) {
								entry.sent = Some(SentRoute {
									hops: hops.clone(),
									cost,
								});
							}
							stream
								.writer
								.encode(&lite::AnnounceBroadcast::Active { suffix, hops, cost })
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
							stats_guards.remove(&absolute);
							if version.has_announce_id() {
								// Retract by id; nothing to send if the announce was filtered and
								// the peer never saw it (an unknown id is a protocol violation).
								if let Some(id) = announce_ids.remove(&suffix) {
									stats
										.broadcast(&absolute)
										.publisher_announced_bytes(absolute.as_str().len() as u64);
									stream.writer.encode(&lite::AnnounceBroadcast::EndedId { id }).await?;
								}
							} else if !retracted {
								stats
									.broadcast(&absolute)
									.publisher_announced_bytes(absolute.as_str().len() as u64);
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
					let Ok(route) = res else {
						// The broadcast is gone; the origin delivers the Ended itself.
						watched.remove(&suffix);
						continue;
					};
					let Some(entry) = watched.get_mut(&suffix) else {
						continue;
					};
					let absolute = origin.absolute(&entry.path).to_owned();
					let cost = Self::outgoing_cost(version, &entry.consumer, &route);
					let hops = Self::prepare_active_hops(&route.hops, self_origin, exclude_hop, version, &absolute)
						.map(|hops| SentRoute { hops, cost });
					let sent = entry.sent.clone();
					match (hops, sent) {
						// Neither the forwarded chain nor the cost moved: nothing to send.
						(Some(route), Some(sent)) if route == sent => {}
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
								stats
									.broadcast(&absolute)
									.publisher_announced_bytes(absolute.as_str().len() as u64);
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
								stats
									.broadcast(&absolute)
									.publisher_announced_bytes(absolute.as_str().len() as u64);
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
							let bs = stats.broadcast(&absolute);
							bs.publisher_announced_bytes(absolute.as_str().len() as u64);
							stats_guards.insert(absolute.clone(), bs.publisher());
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
							stats_guards.remove(&absolute);
							stats
								.broadcast(&absolute)
								.publisher_announced_bytes(absolute.as_str().len() as u64);
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
				// The tick's only job is waking the loop: the next poll pass runs
				// the cost drift scan and surfaces any change as `Op::Route`.
				Op::Recheck => {}
			}
		}
	}

	/// The cost to advertise for a route, alongside its outgoing hop chain.
	///
	/// While we are actively carrying the broadcast the cost is zero: our ingress
	/// is already paid for (or, for a local standby publisher, the work is already
	/// running), so one more subscriber only pays the link to reach us. Otherwise
	/// we forward the accumulated route cost unchanged, which for a standby
	/// publisher is its production cost and for a pure forwarder is the price of
	/// the fetch a subscription would trigger.
	///
	/// The receiving side adds its own link price on top, so we never account for
	/// the link we are sending over. Pre-lite-06 peers get nothing (the field isn't
	/// on their wire), leaving hop count as the metric exactly as before.
	fn outgoing_cost(
		version: Version,
		consumer: &crate::broadcast::Consumer,
		route: &crate::broadcast::Route,
	) -> lite::RouteCost {
		if !version.has_route_cost() {
			return lite::RouteCost::default();
		}

		match consumer.is_active() {
			true => lite::RouteCost(0),
			false => lite::RouteCost(route.cost),
		}
	}

	/// Decide whether to forward an active announcement and compute the outgoing hop chain.
	///
	/// Returns `None` when the announce should be skipped: the peer asked us to exclude it
	/// (`exclude_hop`), it already passed through us (reflected loop), or the hop chain is full.
	fn prepare_active_hops(
		hops: &OriginList,
		self_origin: Origin,
		exclude_hop: u64,
		version: Version,
		absolute: &crate::Path,
	) -> Option<OriginList> {
		if exclude_hop != 0 && hops.iter().any(|h| h.id() == exclude_hop) {
			tracing::debug!(broadcast = %absolute, %exclude_hop, "skipping announce per peer's exclude_hop");
			return None;
		}
		if hops.contains(&self_origin) {
			tracing::debug!(broadcast = %absolute, "skipping reflected announce");
			return None;
		}
		let mut hops = hops.clone();
		// Lite05+ moves the self-stamp to the receiver, which appends our id (reported
		// once via AnnounceOk) on receipt. Older versions stamp it here, dropping if the
		// chain is full.
		if !version.has_announce_ok() && hops.push(self_origin).is_err() {
			tracing::warn!(broadcast = %absolute, "dropping announce; hop chain at MAX_HOPS (possible loop)");
			return None;
		}
		Some(hops)
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
		let broadcast = self.origin.request_broadcast(&request.broadcast).await?;
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
		let broadcast = self.origin.request_broadcast(&subscribe.broadcast);

		// Per-track subscription guard (bumps `subscriptions`). The per-(session,
		// broadcast) `broadcasts` sentinel that counts viewers is taken inside
		// `run_subscribe`, only once the subscription is validated and active, so
		// a stale/invalid SUBSCRIBE isn't counted as a viewer.
		let track_stats = self.stats.broadcast(&absolute).publisher_track(&track);

		if let Err(err) = Self::run_subscribe(
			self.session.clone(),
			&mut stream,
			&subscribe,
			broadcast,
			self.priority.clone(),
			(track_stats, self.broadcasts.clone(), absolute.clone()),
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
		// The track guard (bumps `subscriptions`), the per-session broadcast
		// tracker, and the broadcast path. The `broadcasts` sentinel is taken
		// below, after the subscription is validated, and held for its lifetime.
		stats: (stats::PublisherTrack, stats::SessionBroadcasts, crate::PathOwned),
		version: Version,
	) -> Result<(), Error> {
		let (track_stats, broadcasts, absolute) = stats;
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

		// Subscription is now active: count this session as a viewer of the
		// broadcast. Dropping this guard (subscription end) releases it.
		let _broadcast_sub = broadcasts.subscribe(&absolute);

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
			track_stats: Arc::new(track_stats),
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
		let broadcast = self.origin.request_broadcast(&fetch.broadcast);
		let track_stats = self.stats.broadcast(&absolute).publisher_track(&track);

		if let Err(err) = Self::run_fetch(&mut stream, &fetch, broadcast, track_stats, self.version).await {
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
		track_stats: stats::PublisherTrack,
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

		// Lite05+ FETCH responds with bare FRAME messages; the subscriber already has
		// the timescale from TRACK_INFO and the group sequence from its request.
		track_stats.group();

		// Stream every frame in order. The delta-timestamp baseline resets to 0, so the
		// first served frame's delta is its absolute timestamp (the subscriber decodes
		// against the same baseline).
		let mut prev_ts: u64 = 0;
		while let Some(mut frame) = group.next_frame().await? {
			write_fetch_frame(&mut stream.writer, &mut frame, timescale, &mut prev_ts, &track_stats).await?;
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
	frame: &frame::Consumer,
	timescale: Option<crate::Timescale>,
	prev_ts: &mut u64,
) -> Result<(), Error> {
	if timescale.is_none() {
		return Ok(());
	}

	let ts = frame.timestamp.value();
	encode_zigzag_delta(writer, ts, prev_ts).await?;

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
async fn write_fetch_frame<W: web_transport_trait::SendStream>(
	writer: &mut Writer<W, Version>,
	frame: &mut frame::Consumer,
	timescale: Option<crate::Timescale>,
	prev_ts: &mut u64,
	track_stats: &stats::PublisherTrack,
) -> Result<(), Error> {
	encode_frame_timing(writer, frame, timescale, prev_ts).await?;

	writer.encode(&frame.size).await?;
	track_stats.frame();
	while let Some(chunk) = frame.read_chunk().await? {
		let n = chunk.len() as u64;
		writer.write_chunk(chunk).await?;
		track_stats.bytes(n);
	}

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

/// Poll a single [`track::Subscriber`] for the next group (cap-aware) or datagram from one `&mut`
/// borrow, so groups and datagrams share the same subscription. Groups are polled first so a
/// datagram burst can't starve them; datagrams are polled only when the transport carries them.
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
		match track.poll_next_group(waiter) {
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
	track_stats: Arc<stats::PublisherTrack>,
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
					// One cursor drives the whole subscription: poll the cap-aware next group and,
					// when enabled, the next best-effort datagram. Groups are polled first so a
					// datagram burst can't starve them; datagrams flow whenever no group is ready
					// (including while groups are paused above the cap).
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
		stream.set_priority(priority.current());
		stream.encode(&lite::DataType::Group).await?;
		stream.encode(&msg).await?;
		self.track_stats.group();

		// Lite05+ delta-encodes per-frame timestamps within the group. The first
		// frame's delta is absolute (against an implicit prev value of 0), every
		// subsequent delta is signed against the previous frame.
		let mut prev_ts: u64 = 0;
		while let Some(frame) = self.next_frame(&mut stream, &mut priority, &mut group).await? {
			self.serve_frame(&mut stream, &mut priority, frame, &mut prev_ts)
				.await?;
		}

		stream.finish()?;
		stream.closed().await?;

		tracing::debug!(sequence, "finished group");

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

		if self.session.send_datagram(body).is_ok() {
			self.track_stats.group();
		}
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
		encode_frame_timing(stream, &frame, self.timescale, prev_ts).await?;

		stream.encode(&frame.size).await?;
		self.track_stats.frame();

		while let Some(chunk) = self.read_chunk(stream, priority, &mut frame).await? {
			self.write_chunk(stream, priority, chunk).await?;
		}

		Ok(())
	}

	/// Await the next frame in the group, applying any priority changes that
	/// arrive meanwhile. Errors with [`Error::Cancel`] if the peer closes first.
	async fn next_frame(
		&mut self,
		stream: &mut Writer<S::SendStream, Version>,
		priority: &mut PriorityHandle,
		group: &mut group::Consumer,
	) -> Result<Option<frame::Consumer>, Error> {
		Self::serve_step(
			stream,
			priority,
			&self.track_priority,
			&mut self.track_priority_seen,
			|waiter| group.poll_next_frame(waiter),
		)
		.await
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
			Priority(u8),
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
					if let Poll::Ready(new_pri) = priority.poll_next(waiter) {
						return Poll::Ready(Event::Priority(new_pri));
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
				Event::Priority(new_pri) => stream.set_priority(new_pri),
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

	/// Write a whole chunk, applying priority changes between partial writes,
	/// then count the bytes sent.
	async fn write_chunk(
		&mut self,
		stream: &mut Writer<S::SendStream, Version>,
		priority: &mut PriorityHandle,
		mut chunk: bytes::Bytes,
	) -> Result<(), Error> {
		let n = chunk.len() as u64;
		while chunk.has_remaining() {
			self.apply_priority(stream, priority);
			stream.write(&mut chunk).await?;
		}
		self.track_stats.bytes(n);
		Ok(())
	}

	fn apply_priority(&mut self, stream: &mut Writer<S::SendStream, Version>, priority: &mut PriorityHandle) {
		let track_priority = self.track_priority_current();
		priority.set_track(track_priority);
		stream.set_priority(priority.current());
	}
}
