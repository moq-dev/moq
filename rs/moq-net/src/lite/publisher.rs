use crate::{announce, frame, group, origin, track};
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
	/// Receive-side GOAWAY signal: recorded when the peer's Goaway stream arrives.
	pub goaway: crate::goaway::Protocol,
}

pub(super) struct Publisher<S: web_transport_trait::Session> {
	session: S,
	origin: origin::Consumer,
	self_origin: Origin,
	// The peer's SETUP, read for the origin id it declared. Used to serve the
	// peer from a source whose chain excludes them, keeping the data plane on
	// the same split-horizon rule as the announces we send them.
	peer_setup: super::PeerSetup,
	// The excluded origin handle, resolved once: the peer sends exactly one
	// SETUP, so its declared id never changes for the session.
	serving: std::sync::OnceLock<origin::Consumer>,
	priority: PriorityQueue,
	version: Version,
	goaway: crate::goaway::Protocol,
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
			serving: std::sync::OnceLock::new(),
			priority: Default::default(),
			version: config.version,
			goaway: config.goaway,
		}
	}

	/// The origin to resolve a peer-requested broadcast from: excludes routes
	/// through the peer when its SETUP declared an origin id, so a subscription
	/// is never served data that flowed through the subscriber. The first call
	/// waits for the peer's SETUP (sent at startup on every lite-05+ session,
	/// well before it could learn of anything to subscribe to); the result is
	/// cached, since the peer sends exactly one SETUP per session.
	async fn serving_origin(&self) -> origin::Consumer {
		if let Some(origin) = self.serving.get() {
			return origin.clone();
		}
		// Pre-SETUP versions never declare an id; there is nothing to exclude.
		let origin = if !self.version.has_setup_stream() {
			self.origin.clone()
		} else {
			match self.peer_setup.origin().await {
				Some(peer) => self.origin.clone().excluding(peer),
				None => self.origin.clone(),
			}
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
			lite::ControlType::Goaway => self.recv_goaway(stream).await,
			lite::ControlType::Session => Err(Error::UnexpectedStream),
		}
	}

	/// Decode the peer's GOAWAY and surface it through [`crate::Session::recv_goaway`].
	///
	/// A decode error propagates to the caller, which logs and continues: a
	/// malformed GOAWAY must not tear down the session it is trying to drain.
	async fn recv_goaway(&self, mut stream: Stream<S, Version>) -> Result<(), Error> {
		let msg: lite::Goaway = stream.reader.decode().await?;
		tracing::info!(uri = %msg.uri, "received goaway");

		let uri = msg.uri.into_owned();
		let goaway = crate::goaway::Goaway {
			uri: uri.clone(),
			// moq-lite has no timeout field on the wire.
			timeout: None,
		};

		if let Err(err) = self.goaway.record(goaway) {
			// A second Goaway stream is a protocol violation. The control loop only
			// logs per-stream errors, so close the session here rather than letting a
			// peer silently replace a redirect an observer may already be acting on.
			tracing::warn!(%uri, "duplicate GOAWAY received; closing session");
			self.session.close(err.to_code(), &err.to_string());
			return Err(err);
		}
		Ok(())
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

		// The identity whose routes we filter out. Lite-04/05 carry it per
		// announce stream; lite-06+ reads the session-wide SETUP Origin
		// parameter, the same identity the subscribe path excludes.
		let exclude_hop = if self.version.has_exclude_hop() {
			interest.exclude_hop
		} else if self.version.has_setup_stream() {
			self.peer_setup.origin().await.map(|origin| origin.id()).unwrap_or(0)
		} else {
			0
		};

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

		// How long a drained broadcast keeps advertising zero before the restart
		// that restores its cold cost. Pure hysteresis: demand edges arrive
		// exactly (via `broadcast::Demand`), but re-pricing the instant the last
		// viewer leaves would flap routing across the mesh on viewer churn.
		const COST_LINGER: Duration = Duration::from_secs(5);

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
		for (index, route) in routes.iter().enumerate() {
			// Offline routes are reachable by exact path but never advertised.
			if !route.announce {
				continue;
			}
			if exclude_hop != 0 && route.hops.iter().any(|h| h.id() == exclude_hop) {
				continue;
			}
			if route.hops.contains(&self_origin) {
				continue;
			}
			let mut hops = route.hops.clone();
			// Lite05+ moves the self-stamp to the receiver, which appends our id (reported
			// once via AnnounceOk) on receipt. Older versions stamp it here, dropping if the
			// chain is full.
			if !version.has_announce_ok() && hops.push(self_origin).is_err() {
				tracing::warn!(broadcast = %absolute, "dropping announce; hop chain at MAX_HOPS (possible loop)");
				continue;
			}
			let serving = index == 0;
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
	/// A ceiling-cost route pierces the carrying discount. For a drain (the
	/// ceiling's primary producer) the paid-for ingress is going away, so
	/// advertising zero would keep pulling subscribers onto a path about to
	/// vanish when a peer with any other edge should move now, while the seam is
	/// seamless. The rule keys on the value, not the reason: the reason does not
	/// travel on the wire, and a cost that saturated through charges marks a
	/// path of last resort all the same.
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

		let draining = route.cost >= crate::broadcast::DRAIN_COST;
		match serving && demand.is_used() && !draining {
			true => lite::RouteCost(0),
			// Clamp to what a varint can carry. Costs that arrive from a peer are
			// already capped by `RouteCost::charged`, but a locally created route sets
			// `Route::cost` directly and nothing stops it naming more than the wire can
			// express. This is the single point every advertised cost passes through, so
			// it is the one place that has to hold.
			false => lite::RouteCost(route.cost.min(crate::broadcast::MAX_COST)),
		}
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
			..Bounds::from(subscribe).positions()
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
			Bounds::from(subscribe),
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
					frame_start: fetch.start_frame,
					..Default::default()
				},
			)
			.await?;

		// The response carries no header, so a short run is indistinguishable from one
		// that started elsewhere: only serve a range we can cover exactly. `fetch_group`
		// already positions the consumer, so this is a belt-and-braces check on a
		// promise the wire can't restate.
		if group.index() != fetch.start_frame {
			return Err(Error::Lagged);
		}
		// The end is a serving cap only: the cached group runs to the end of the group
		// so it stays usable for anyone else (see `group::Fetch::frame_start`).
		group.end_at(fetch.end_frame);

		// FETCH is gated to lite-05+, which learned the track timescale via TRACK_INFO.
		let timescale = if version.has_track_stream() {
			Some(group.timescale())
		} else {
			None
		};

		// Stream every frame in order. The delta-timestamp baseline resets to 0, so the
		// first served frame's delta is its absolute timestamp (the subscriber decodes
		// against the same baseline).
		let mut prev_ts: u64 = 0;
		while let Some(mut frame) = group.next_frame().await? {
			write_fetch_frame(&mut stream.writer, &mut frame, timescale, &mut prev_ts).await?;
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

	/// A locally created route may set `Route::cost` past what a varint can carry,
	/// since nothing validates the public field. The advertised cost has to clamp,
	/// or forwarding the announce fails to encode and takes the announce stream
	/// with it.
	#[test]
	fn outgoing_cost_clamps_to_the_wire_ceiling() {
		use crate::broadcast;

		let mut producer = broadcast::Info::new().produce();
		producer
			.set_route(broadcast::Route::announced().with_cost(u64::MAX))
			.unwrap();
		let consumer = producer.consume();
		let route = consumer.route();
		let demand = consumer.demand();

		let cost = Publisher::<crate::lite::test_transport::SinkSession>::outgoing_cost(
			Version::Lite06Wip,
			&demand,
			&route,
			false,
		);
		assert_eq!(cost, lite::RouteCost(broadcast::MAX_COST));

		let mut buf = Vec::new();
		cost.encode(&mut buf, Version::Lite06Wip)
			.expect("an advertised cost must always encode");
	}

	/// A draining serving route must advertise its drain cost even while the
	/// broadcast has demand. The carrying discount exists because the ingress is
	/// already paid for; a draining ingress is going away, and masking it with
	/// zero would keep downstream peers glued to a path about to vanish instead
	/// of migrating to another edge while the handover is seamless.
	#[test]
	fn outgoing_cost_advertises_a_drain_despite_demand() {
		use crate::broadcast;

		let mut producer = broadcast::Info::new().produce();
		producer
			.set_route(broadcast::Route::announced().with_cost(broadcast::DRAIN_COST))
			.unwrap();
		let consumer = producer.consume();
		let route = consumer.route();
		let demand = consumer.demand();

		// A consumed track: demand.is_used() is true, which is exactly the case
		// that used to collapse the cost to zero.
		let track = producer.create_track("video", None).unwrap();
		let _reader = track.consume();
		assert!(demand.is_used(), "the consumed track should register demand");

		let cost = Publisher::<crate::lite::test_transport::SinkSession>::outgoing_cost(
			Version::Lite06Wip,
			&demand,
			&route,
			true,
		);
		assert_eq!(cost, lite::RouteCost(broadcast::DRAIN_COST));

		// A drain that arrived over the wire propagates through a carrying relay:
		// the upstream's ceiling plus our link price saturates back to the
		// ceiling, which pierces this hop's discount too. Without that, each
		// carrying hop would re-mask the drain as cost 0.
		let forwarded = lite::RouteCost(broadcast::DRAIN_COST).charged(5).0;
		producer
			.set_route(broadcast::Route::announced().with_cost(forwarded))
			.unwrap();
		let route = consumer.route();
		let cost = Publisher::<crate::lite::test_transport::SinkSession>::outgoing_cost(
			Version::Lite06Wip,
			&demand,
			&route,
			true,
		);
		assert_eq!(cost, lite::RouteCost(broadcast::DRAIN_COST));

		// A healthy serving route with demand still gets the carrying discount.
		producer.set_route(broadcast::Route::announced().with_cost(7)).unwrap();
		let route = consumer.route();
		let cost = Publisher::<crate::lite::test_transport::SinkSession>::outgoing_cost(
			Version::Lite06Wip,
			&demand,
			&route,
			true,
		);
		assert_eq!(cost, lite::RouteCost(0));
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
) -> Result<(), Error> {
	encode_frame_timing(writer, frame, timescale, prev_ts).await?;

	writer.encode(&frame.size).await?;
	while let Some(chunk) = frame.read_chunk().await? {
		writer.write_chunk(chunk).await?;
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
		bounds: Bounds,
		reader: &mut crate::coding::Reader<S::RecvStream, Version>,
		writer: &mut Writer<S::SendStream, Version>,
		track_priority_tx: &kio::Producer<u8>,
	) -> Result<(), Error> {
		let mut tasks: FuturesUnordered<MaybeSendBox<'static, ()>> = FuturesUnordered::new();

		// Start the consumer at the specified sequence, otherwise start at the latest group.
		if let Some(start_group) = bounds.start_group.or_else(|| track.latest()) {
			track.start_at(start_group);
		}

		// Apply the initial cap from the original Subscribe. Subsequent updates
		// flow through the SubscribeUpdate select arm below.
		track.end_at(bounds.end_group);

		// Frame bounds qualify the start and end group only; everything in between is
		// served whole. Each is `(group, frame)`, or `None` for no offset at all.
		let mut start_frame = bounds.start_frame();
		let mut end_frame = bounds.end_frame();

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
					Recv::Group(mut group) => {
						let sequence = group.sequence;
						if !position_group(&mut group, start_frame, end_frame) {
							// Its head is gone, and this subscriber didn't ask for a
							// partial group. Skip it rather than open a stream that can
							// only be reset; the next servable group resolves the start.
							tracing::debug!(subscribe = self.id, track = %self.track_name, sequence, "skipping group with a missing head");
							continue;
						}
						if emit_range && !start_sent {
							start_sent = true;
							// Only the group: the subscriber derives the start frame from
							// its own request (see `lite::SubscribeStart`).
							writer
								.encode(&lite::SubscribeResponse::Start(lite::SubscribeStart {
									group: sequence,
								}))
								.await?;
							// SUBSCRIBE_OK is an implicit drop of everything below the
							// resolved start (the subscriber records it as a permanent
							// miss), so a lower group arriving late must not be served
							// after all. A widening SUBSCRIBE_UPDATE re-lowers the floor,
							// renegotiating the resolved start along with the demand.
							track.start_at(sequence);
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
					let bounds = Bounds::from(&upd);
					let _ = track.update(crate::track::Subscription {
						priority: upd.priority,
						ordered: upd.ordered,
						latency_max: upd.max_latency,
						..bounds.positions()
					});
					if let Some(start_group) = upd.start_group {
						track.start_at(start_group);
					}
					track.end_at(upd.end_group);
					start_frame = bounds.start_frame();
					end_frame = bounds.end_frame();
				}
			}
		}
	}

	fn queue_serve(&mut self, group: group::Consumer, tasks: &mut FuturesUnordered<MaybeSendBox<'static, ()>>) {
		let sequence = group.sequence;
		let frame_start = group.index();
		tracing::debug!(subscribe = self.id, track = %self.track_name, sequence, "serving group");

		// Use the latest priority for new groups so SUBSCRIBE_UPDATE applies to them too.
		let current_priority = self.track_priority_current();
		let handle = self.priority.insert(Priority::new(current_priority, sequence));
		let fut = self.clone().serve_group(sequence, frame_start, handle, group);
		tasks.push(fut.map(|_| ()).maybe_boxed());
	}

	async fn serve_group(
		mut self,
		sequence: u64,
		frame_start: u64,
		mut priority: PriorityHandle,
		mut group: group::Consumer,
	) -> Result<(), Error> {
		let msg = lite::Group {
			subscribe: self.id,
			sequence,
			frame_start,
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

		stream.finish()?;
		stream.closed().await?;

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
		stream.set_priority(priority.current());
		stream.encode(&lite::DataType::Group).await?;
		stream.encode(msg).await?;

		// Lite05+ delta-encodes per-frame timestamps within the group. The first
		// frame's delta is absolute (against an implicit prev value of 0), every
		// subsequent delta is signed against the previous frame.
		let mut prev_ts: u64 = 0;
		while let Some(frame) = self.next_frame(stream, priority, group).await? {
			self.serve_frame(stream, priority, frame, &mut prev_ts).await?;
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
		encode_frame_timing(stream, &frame, self.timescale, prev_ts).await?;

		stream.encode(&frame.size).await?;

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

	/// Write a whole chunk, applying priority changes between partial writes.
	async fn write_chunk(
		&mut self,
		stream: &mut Writer<S::SendStream, Version>,
		priority: &mut PriorityHandle,
		mut chunk: bytes::Bytes,
	) -> Result<(), Error> {
		while chunk.has_remaining() {
			self.apply_priority(stream, priority);
			stream.write(&mut chunk).await?;
		}
		Ok(())
	}

	fn apply_priority(&mut self, stream: &mut Writer<S::SendStream, Version>, priority: &mut PriorityHandle) {
		let track_priority = self.track_priority_current();
		priority.set_track(track_priority);
		stream.set_priority(priority.current());
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

		let handle = subscription.priority.insert(Priority::new(0, 0));
		let mut serve = std::pin::pin!(subscription.serve_group(0, 0, handle, group.consume()));

		// Drain the frame, leaving the task parked awaiting the next one.
		assert!(futures::poll!(serve.as_mut()).is_pending());

		// The group is dropped from the cache mid-stream: a truncated group, not a cancel.
		group.abort(Error::Old).unwrap();

		assert!(matches!(serve.await, Err(Error::Old)));
		assert_eq!(log.resets(), vec![Error::Old.to_code()]);
	}
}
