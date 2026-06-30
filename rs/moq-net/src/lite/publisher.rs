use std::time::Duration;

use futures::{FutureExt, StreamExt, stream::FuturesUnordered};
use web_async::FuturesExt;
use web_transport_trait::Stats;

use crate::{
	AsPath, BroadcastConsumer, Error, Origin, OriginConsumer, OriginList, StatsHandle as MoqStats, Track,
	TrackConsumer,
	coding::{Stream, Writer},
	lite::{
		self,
		priority::{Priority, PriorityHandle, PriorityQueue},
	},
	model::GroupConsumer,
};

use super::Version;

pub(super) struct PublisherConfig<S: web_transport_trait::Session> {
	pub session: S,
	/// The origin we read local broadcasts from. None gives this session a
	/// dummy, immediately-closed origin (i.e. nothing to publish).
	pub origin: Option<OriginConsumer>,
	/// Stats aggregator for this session's egress. Use [`MoqStats::default`]
	/// to opt out.
	pub stats: MoqStats,
	pub version: Version,
}

pub(super) struct Publisher<S: web_transport_trait::Session> {
	session: S,
	origin: OriginConsumer,
	stats: MoqStats,
	/// Per-session egress broadcast-subscription tracker. Each downstream
	/// subscription holds a guard so `broadcasts - broadcasts_closed` counts
	/// the distinct sessions (viewers) watching each broadcast.
	broadcasts: crate::SessionBroadcasts,
	self_origin: Origin,
	priority: PriorityQueue,
	version: Version,
}

impl<S: web_transport_trait::Session> Publisher<S> {
	pub fn new(config: PublisherConfig<S>) -> Self {
		// Default to a dummy origin that is immediately closed.
		let origin = config.origin.unwrap_or_else(|| Origin::random().produce().consume());
		// Identity stamped onto outbound announce hops. Derived from the
		// origin we're consuming so it matches the local relay identity
		// across every session, required for cross-session loop detection.
		let self_origin = *origin;
		let broadcasts = config.stats.publisher_broadcasts();
		Self {
			session: config.session,
			origin,
			stats: config.stats,
			broadcasts,
			self_origin,
			priority: Default::default(),
			version: config.version,
		}
	}

	pub async fn run(mut self) -> Result<(), Error> {
		loop {
			let mut stream = Stream::accept(&self.session, self.version).await?;

			// To avoid cloning the origin, we process each control stream in received order.
			// This adds some head-of-line blocking but it delays an expensive clone.
			let kind = stream.reader.decode().await?;

			if let Err(err) = match kind {
				lite::ControlType::Announce => self.recv_announce(stream).await,
				lite::ControlType::Subscribe => self.recv_subscribe(stream).await,
				lite::ControlType::Probe => {
					self.recv_probe(stream);
					Ok(())
				}
				lite::ControlType::Goaway => {
					tracing::info!("received goaway stream");
					Ok(())
				}
				lite::ControlType::Session | lite::ControlType::Fetch => Err(Error::UnexpectedStream),
			} {
				tracing::warn!(%err, "control stream error");
			}
		}
	}

	fn recv_probe(&self, mut stream: Stream<S, Version>) {
		let session = self.session.clone();
		let version = self.version;

		web_async::spawn(async move {
			match Self::run_probe(&session, &mut stream, version).await {
				Ok(()) => {
					tracing::debug!("probe stream closed");
				}
				Err(err) => {
					tracing::warn!(%err, "probe stream error");
					stream.writer.abort(&err);
				}
			}
		});
	}

	async fn run_probe(session: &S, stream: &mut Stream<S, Version>, _version: Version) -> Result<(), Error> {
		const PROBE_INTERVAL: Duration = Duration::from_millis(100);
		const PROBE_MAX_AGE: Duration = Duration::from_secs(10);
		const PROBE_MAX_DELTA: f64 = 0.25;

		let mut last_sent: Option<(u64, tokio::time::Instant)> = None;
		let mut interval = tokio::time::interval(PROBE_INTERVAL);

		loop {
			tokio::select! {
				res = stream.reader.closed() => return res,
				_ = interval.tick() => {}
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
				last_sent = Some((bitrate, tokio::time::Instant::now()));
			}
		}
	}

	pub async fn recv_announce(&mut self, mut stream: Stream<S, Version>) -> Result<(), Error> {
		let interest = stream.reader.decode::<lite::AnnounceInterest>().await?;
		let prefix = interest.prefix.to_owned();
		let exclude_hop = interest.exclude_hop;
		let version = self.version;

		let Some(mut origin) = self.origin.scope(&[prefix.as_path()]) else {
			// A peer may announce-interest in a prefix this session can't serve (e.g. a
			// publish-only token). FIN the stream so the peer's announce subscription
			// completes cleanly, rather than erroring and tearing down the whole session.
			web_async::spawn(async move {
				if let Err(err) = Self::finish_empty_announce(&mut stream, version).await {
					stream.writer.abort(&err);
				}
			});
			return Ok(());
		};

		let self_origin = self.self_origin;
		let stats = self.stats.clone();
		web_async::spawn(async move {
			if let Err(err) = Self::run_announce(
				&mut stream,
				&mut origin,
				&prefix,
				self_origin,
				exclude_hop,
				stats,
				version,
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
		});

		Ok(())
	}

	// FIN an announce stream that will never carry any announcements (the peer asked for a
	// prefix this session can't serve). Lite01/02 peers block reading ANNOUNCE_INIT, so send
	// an empty one before finishing; Lite03+ has no init.
	async fn finish_empty_announce(stream: &mut Stream<S, Version>, version: Version) -> Result<(), Error> {
		if let Version::Lite01 | Version::Lite02 = version {
			stream
				.writer
				.encode(&lite::AnnounceInit { suffixes: Vec::new() })
				.await?;
		}
		stream.writer.finish()?;
		stream.writer.closed().await
	}

	async fn run_announce(
		stream: &mut Stream<S, Version>,
		origin: &mut OriginConsumer,
		prefix: impl AsPath,
		self_origin: Origin,
		// Peer's session-level origin id, sent in AnnounceInterest. We skip
		// forwarding announces whose hop chain already contains this id, so
		// reflected announces (cluster loops) never hit the wire. Zero means
		// the peer didn't set it (Lite03 or earlier), pass through.
		exclude_hop: u64,
		stats: MoqStats,
		version: Version,
	) -> Result<(), Error> {
		let prefix = prefix.as_path();

		// Per-path stats guards: dropping the guard records `broadcasts_closed`.
		// The origin contract guarantees announce/unannounce toggles per path, so a
		// new active announcement must always be for a path with no live guard.
		let mut stats_guards: std::collections::HashMap<crate::PathOwned, crate::PublisherStats> =
			std::collections::HashMap::new();

		match version {
			Version::Lite01 | Version::Lite02 => {
				let mut init = Vec::new();

				// Send ANNOUNCE_INIT as the first message with all currently active paths
				// We use `try_next()` to synchronously get the initial updates.
				while let Some((path, active)) = origin.try_announced() {
					let suffix = path.strip_prefix(&prefix).expect("origin returned invalid path");

					if active.is_some() {
						tracing::debug!(broadcast = %origin.absolute(&path), "announce");
						let absolute = origin.absolute(&path).to_owned();
						let guard = stats.broadcast(&absolute).publisher();
						let prev = stats_guards.insert(absolute, guard);
						debug_assert!(prev.is_none(), "origin announced a path that was already active");
						init.push(suffix.to_owned());
					} else {
						// A potential race.
						tracing::debug!(broadcast = %origin.absolute(&path), "unannounce");
						stats_guards.remove(&origin.absolute(&path).to_owned());
						init.retain(|path| path != &suffix);
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
			_ => {
				// Lite03+: no more announce init.
			}
		}

		// Send updates as they arrive.
		loop {
			tokio::select! {
				biased;
				res = stream.reader.closed() => return res,
				announced = origin.announced() => {
					match announced {
						Some((path, active)) => {
							let suffix = path.strip_prefix(&prefix).expect("origin returned invalid path").to_owned();

							if let Some(active) = active {
								// Skip if the peer asked us to exclude announces whose hop chain
								// contains their id — they already saw this broadcast upstream.
								if exclude_hop != 0 && active.hops.iter().any(|h| h.id == exclude_hop) {
									tracing::debug!(
										broadcast = %origin.absolute(&path),
										%exclude_hop,
										"skipping announce per peer's exclude_hop",
									);
									continue;
								}
								// Defense in depth: never echo an announce that already passed
								// through us. The subscriber should drop these before they reach
								// our origin, but if one slips through, don't propagate the loop.
								if active.hops.contains(&self_origin) {
									tracing::debug!(
										broadcast = %origin.absolute(&path),
										"skipping reflected announce",
									);
									continue;
								}
								tracing::debug!(broadcast = %origin.absolute(&path), "announce");
								// Append our origin id to the hops so the next relay can detect loops.
								// If the chain is already at MAX_HOPS, skip the announce — this link is
								// effectively unreachable and the peer will eventually prune the loop.
								let mut hops = active.hops.clone();
								if hops.push(self_origin).is_err() {
									tracing::warn!(
										broadcast = %origin.absolute(&path),
										"dropping announce; hop chain at MAX_HOPS (possible loop)",
									);
									continue;
								}
								let absolute = origin.absolute(&path).to_owned();
								let bs = stats.broadcast(&absolute);
								// Count the broadcast name length, not the encoded message size, so
								// stats don't penalize the broadcast for hop/framing overhead.
								bs.publisher_announced_bytes(absolute.as_str().len() as u64);
								let prev = stats_guards.insert(absolute, bs.publisher());
								debug_assert!(prev.is_none(), "origin announced a path that was already active");
								let msg = lite::Announce::Active { suffix, hops };
								stream.writer.encode(&msg).await?;
							} else {
								let absolute = origin.absolute(&path).to_owned();
								tracing::debug!(broadcast = %absolute, "unannounce");
								// Count the name length whether or not a guard is held: the Ended
								// message is sent even for announces we filtered out above.
								stats
									.broadcast(&absolute)
									.publisher_announced_bytes(absolute.as_str().len() as u64);
								stats_guards.remove(&absolute);
								// An ended announce doesn't need hops — the receiver matches on path only.
								let msg = lite::Announce::Ended {
									suffix,
									hops: OriginList::new(),
								};
								stream.writer.encode(&msg).await?;
							}
						},
						None => {
							stream.writer.finish()?;
							return stream.writer.closed().await;
						}
					}
				}
			}
		}
	}

	pub async fn recv_subscribe(&mut self, mut stream: Stream<S, Version>) -> Result<(), Error> {
		let subscribe = stream.reader.decode::<lite::Subscribe>().await?;

		let id = subscribe.id;
		let track = subscribe.track.clone();
		let absolute = self.origin.absolute(&subscribe.broadcast).to_owned();

		tracing::info!(%id, broadcast = %absolute, %track, "subscribed started");

		// We just received a subscribe for this exact path, so by definition the peer has
		// already seen an announcement for it — synchronous lookup is appropriate here.
		let broadcast = self.origin.get_broadcast(&subscribe.broadcast);
		let priority = self.priority.clone();
		let version = self.version;

		// Per-track subscription guard (bumps `subscriptions`). The per-(session,
		// broadcast) `broadcasts` sentinel that counts viewers is taken inside
		// `run_subscribe`, only once the subscription is validated and active, so
		// a stale/invalid SUBSCRIBE isn't counted as a viewer.
		let track_stats = self.stats.broadcast(&absolute).publisher_track(&track);
		let broadcasts = self.broadcasts.clone();

		let session = self.session.clone();
		web_async::spawn(async move {
			if let Err(err) = Self::run_subscribe(
				session,
				&mut stream,
				&subscribe,
				broadcast,
				priority,
				(track_stats, broadcasts, absolute.clone()),
				version,
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
		});

		Ok(())
	}

	async fn run_subscribe(
		session: S,
		stream: &mut Stream<S, Version>,
		subscribe: &lite::Subscribe<'_>,
		consumer: Option<BroadcastConsumer>,
		priority: PriorityQueue,
		// The track guard (bumps `subscriptions`), the per-session broadcast
		// tracker, and the broadcast path. The `broadcasts` sentinel is taken
		// below, after the subscription is validated, and held for its lifetime.
		stats: (crate::PublisherTrack, crate::SessionBroadcasts, crate::PathOwned),
		version: Version,
	) -> Result<(), Error> {
		let (track_stats, broadcasts, absolute) = stats;
		let track = Track {
			name: subscribe.track.to_string(),
			priority: subscribe.priority,
		};

		let broadcast = consumer.ok_or(Error::NotFound)?;
		let track = broadcast.subscribe_track(&track)?;

		// Subscription is now active: count this session as a viewer of the
		// broadcast. Dropping this guard (subscription end) releases it.
		let _broadcast_sub = broadcasts.subscribe(&absolute);

		// TODO wait until track.info() to get the *real* priority

		let info = lite::SubscribeOk {
			priority: track.priority,
			ordered: false,
			max_latency: std::time::Duration::ZERO,
			start_group: None,
			end_group: None,
		};

		stream.writer.encode(&lite::SubscribeResponse::Ok(info)).await?;

		// Track-level subscriber priority. SUBSCRIBE_UPDATE messages broadcast new values
		// to both run_track (so future groups inherit the new priority) and serve_group
		// tasks (so in-flight groups update via PriorityHandle::set_track).
		let (track_priority_tx, track_priority_rx) = tokio::sync::watch::channel(track.priority);
		let track_stats = std::sync::Arc::new(track_stats);

		tokio::select! {
			res = Self::run_track(session, track, subscribe, priority, track_stats, track_priority_rx, version) => res?,
			res = Self::run_subscribe_updates(&mut stream.reader, &track_priority_tx) => res?,
		}

		stream.writer.finish()?;
		stream.writer.closed().await
	}

	async fn run_subscribe_updates<R: web_transport_trait::RecvStream>(
		reader: &mut crate::coding::Reader<R, Version>,
		priority_tx: &tokio::sync::watch::Sender<u8>,
	) -> Result<(), Error> {
		while let Some(upd) = reader.decode_maybe::<lite::SubscribeUpdate>().await? {
			let _ = priority_tx.send(upd.priority);
		}
		Ok(())
	}

	async fn run_track(
		session: S,
		mut track: TrackConsumer,
		subscribe: &lite::Subscribe<'_>,
		priority: PriorityQueue,
		track_stats: std::sync::Arc<crate::PublisherTrack>,
		mut track_priority: tokio::sync::watch::Receiver<u8>,
		version: Version,
	) -> Result<(), Error> {
		let mut tasks = FuturesUnordered::new();

		// Start the consumer at the specified sequence, otherwise start at the latest group.
		if let Some(start_group) = subscribe.start_group.or_else(|| track.latest()) {
			track.start_at(start_group);
		}

		loop {
			let group = tokio::select! {
				// Poll all active group futures; never matches but keeps them running.
				true = async {
					while tasks.next().await.is_some() {}
					false
				} => unreachable!(),
				Some(group) = track.recv_group().transpose() => group,
				else => return Ok(()),
			}?;

			let sequence = group.sequence;
			tracing::debug!(subscribe = %subscribe.id, track = %track.name, sequence, "serving group");

			let msg = lite::Group {
				subscribe: subscribe.id,
				sequence,
			};

			// Use the latest priority for new groups so SUBSCRIBE_UPDATE applies to them too.
			let current_priority = *track_priority.borrow_and_update();
			let handle = priority.insert(Priority::new(current_priority, sequence));
			tasks.push(
				Self::serve_group(
					session.clone(),
					msg,
					handle,
					group,
					track_stats.clone(),
					track_priority.clone(),
					version,
				)
				.map(|_| ()),
			);
		}
	}

	async fn serve_group(
		session: S,
		msg: lite::Group,
		mut priority: PriorityHandle,
		mut group: GroupConsumer,
		track_stats: std::sync::Arc<crate::PublisherTrack>,
		mut track_priority: tokio::sync::watch::Receiver<u8>,
		version: Version,
	) -> Result<(), Error> {
		let stream = session.open_uni().await.map_err(Error::from_transport)?;

		let mut stream = Writer::new(stream, version);
		stream.set_priority(priority.current());
		stream.encode(&lite::DataType::Group).await?;
		stream.encode(&msg).await?;
		track_stats.group();

		loop {
			let frame = tokio::select! {
				biased;
				_ = stream.closed() => return Err(Error::Cancel),
				frame = group.next_frame() => frame,
				new_pri = priority.next() => {
					stream.set_priority(new_pri);
					continue;
				}
				Ok(()) = track_priority.changed() => {
					priority.set_track(*track_priority.borrow_and_update());
					continue;
				}
			};

			let mut frame = match frame? {
				Some(frame) => frame,
				None => break,
			};

			stream.encode(&frame.size).await?;
			track_stats.frame();

			loop {
				let chunk = tokio::select! {
					biased;
					_ = stream.closed() => return Err(Error::Cancel),
					chunk = frame.read_chunk() => chunk,
					new_pri = priority.next() => {
						stream.set_priority(new_pri);
						continue;
					}
					Ok(()) = track_priority.changed() => {
						priority.set_track(*track_priority.borrow_and_update());
						continue;
					}
				};

				match chunk? {
					Some(mut chunk) => {
						let n = chunk.len() as u64;
						loop {
							tokio::select! {
								biased;
								result = stream.write_all(&mut chunk) => {
									result?;
									break;
								}
								new_pri = priority.next() => {
									stream.set_priority(new_pri);
								}
								Ok(()) = track_priority.changed() => {
									priority.set_track(*track_priority.borrow_and_update());
								}
							}
						}
						track_stats.bytes(n);
					}
					None => break,
				}
			}
		}

		stream.finish()?;
		stream.closed().await?;

		tracing::debug!(sequence = %msg.sequence, "finished group");

		Ok(())
	}
}
