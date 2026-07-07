use crate::{announce, frame, group, origin, track};
use std::{sync::Arc, task::Poll, time::Duration};

use bytes::Buf;
use futures::{FutureExt, StreamExt, stream::FuturesUnordered};
use web_transport_trait::Stats;

use crate::{
	AsPath, Error, Origin, OriginList, StatsHandle as MoqStats,
	coding::{Encode, Stream, Writer},
	lite::{
		self,
		priority::{Priority, PriorityHandle, PriorityQueue},
	},
	util::{MaybeBoxedExt, MaybeSendBox},
};

use super::Version;

pub(super) struct PublisherConfig<S: web_transport_trait::Session> {
	pub session: S,
	/// The origin we read local broadcasts from.
	pub origin: origin::Consumer,
	/// Stats aggregator for this session's egress. Use [`MoqStats::default`]
	/// to opt out.
	pub stats: MoqStats,
	pub version: Version,
}

pub(super) struct Publisher<S: web_transport_trait::Session> {
	session: S,
	origin: origin::Consumer,
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
		// stream gets its own task and they all make progress independently.
		let this = Arc::new(self);

		loop {
			let stream = Stream::accept(&this.session, this.version).await?;

			let this = this.clone();
			web_async::spawn(async move {
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
				while let Some((path, event)) = announced.try_next() {
					let suffix = path
						.strip_prefix(&prefix)
						.expect("origin returned invalid path")
						.to_owned();
					let absolute = origin.absolute(&path).to_owned();

					// Lite01/02 only carries the set of active paths, so a restart is
					// indistinguishable from an active here.
					if event.broadcast().is_some() {
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
				let mut initial: Vec<(crate::PathOwned, OriginList)> = Vec::new();
				while let Some((path, event)) = announced.try_next() {
					let suffix = path
						.strip_prefix(&prefix)
						.expect("origin returned invalid path")
						.to_owned();
					let absolute = origin.absolute(&path).to_owned();

					match event.broadcast() {
						Some(broadcast) => {
							let info = broadcast.info();
							let hops = &info.hops;
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
							initial.push((suffix, hops.clone()));
						}
						None => {
							// A potential race: a just-announced path already unannounced.
							tracing::debug!(broadcast = %absolute, "unannounce");
							stats_guards.remove(&absolute);
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
				for (suffix, hops) in &initial {
					lite::AnnounceBroadcast::Active {
						suffix: suffix.as_path(),
						hops: hops.clone(),
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

		// Send updates as they arrive.
		loop {
			tokio::select! {
				biased;
				res = stream.reader.closed() => return res,
				next = announced.next() => {
						let Some((path, event)) = next else {
							stream.writer.finish()?;
							return stream.writer.closed().await;
						};

						let suffix = path.strip_prefix(&prefix).expect("origin returned invalid path").to_owned();
						let absolute = origin.absolute(&path).to_owned();

						match event {
							announce::Event::Active(active) => {
								let info = active.info();
								let Some(hops) = Self::prepare_active_hops(&info.hops, self_origin, exclude_hop, version, &absolute) else {
									continue;
								};
								tracing::debug!(broadcast = %absolute, "announce");
								let bs = stats.broadcast(&absolute);
								// Count the broadcast name length, not the encoded message size, so
								// stats don't penalize the broadcast for hop/framing overhead.
								bs.publisher_announced_bytes(absolute.as_str().len() as u64);
								let prev = stats_guards.insert(absolute.clone(), bs.publisher());
								debug_assert!(prev.is_none(), "origin announced a path that was already active");
								stream.writer.encode(&lite::AnnounceBroadcast::Active { suffix, hops }).await?;
							}
							announce::Event::Restart(active) => {
								// On lite-05+ a restart travels as a duplicate ANNOUNCE (a second
								// `Active` for an already-announced path). Older versions never defined
								// that, so split it into an unannounce followed by a fresh announce.
								let info = active.info();
								match Self::prepare_active_hops(&info.hops, self_origin, exclude_hop, version, &absolute) {
									Some(hops) => {
										tracing::debug!(broadcast = %absolute, "restart");
										let bs = stats.broadcast(&absolute);
										// Continuity: keep the existing stats guard (no close + reopen).
										if lite::restart_supported(version) {
											// One Active message on the wire, one name-length count.
											bs.publisher_announced_bytes(absolute.as_str().len() as u64);
											stream.writer.encode(&lite::AnnounceBroadcast::Active { suffix, hops }).await?;
										} else {
											// Ended + Active pair, so count the name twice.
											bs.publisher_announced_bytes(2 * absolute.as_str().len() as u64);
											stream
												.writer
												.encode(&lite::AnnounceBroadcast::Ended {
													suffix: suffix.clone(),
													hops: OriginList::new(),
												})
												.await?;
											stream.writer.encode(&lite::AnnounceBroadcast::Active { suffix, hops }).await?;
										}
									}
									None => {
										// The replacement loops back to us; from this peer's view the broadcast is gone.
										tracing::debug!(broadcast = %absolute, "restart replacement looped; unannouncing");
										stats.broadcast(&absolute)
											.publisher_announced_bytes(absolute.as_str().len() as u64);
										stats_guards.remove(&absolute);
										stream.writer.encode(&lite::AnnounceBroadcast::Ended { suffix, hops: OriginList::new() }).await?;
									}
								}
							}
							announce::Event::Ended => {
								tracing::debug!(broadcast = %absolute, "unannounce");
								// Count the name length whether or not a guard is held: the Ended
								// message is sent even for announces we filtered out above.
								stats.broadcast(&absolute)
									.publisher_announced_bytes(absolute.as_str().len() as u64);
								stats_guards.remove(&absolute);
								// An ended announce doesn't need hops; the receiver matches on path only.
								stream.writer.encode(&lite::AnnounceBroadcast::Ended { suffix, hops: OriginList::new() }).await?;
							}
						}
					}
			}
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
				cache: info.cache,
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
		broadcast: kio::Pending<origin::Requested>,
		priority: PriorityQueue,
		// The track guard (bumps `subscriptions`), the per-session broadcast
		// tracker, and the broadcast path. The `broadcasts` sentinel is taken
		// below, after the subscription is validated, and held for its lifetime.
		stats: (crate::PublisherTrack, crate::SessionBroadcasts, crate::PathOwned),
		version: Version,
	) -> Result<(), Error> {
		let (track_stats, broadcasts, absolute) = stats;
		let subscription = crate::Subscription {
			priority: subscribe.priority,
			ordered: subscribe.ordered,
			stale: subscribe.max_latency,
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
		// tasks (so in-flight groups update via PriorityHandle::set_track). The Sender
		// stays in run_subscribe and gets handed to run_track so the same loop that
		// parses SUBSCRIBE_UPDATEs also fans the new priority out.
		let (track_priority_tx, track_priority_rx) = tokio::sync::watch::channel(subscribe.priority);

		let sub = Subscription {
			session,
			id: subscribe.id,
			track_name: Arc::from(track.name()),
			track_stats: Arc::new(track_stats),
			priority,
			track_priority: track_priority_rx,
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
		broadcast: kio::Pending<origin::Requested>,
		track_stats: crate::PublisherTrack,
		version: Version,
	) -> Result<(), Error> {
		let broadcast = broadcast.await?;
		let track = broadcast.track(&fetch.track)?;

		let group = track
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

		// Stream the whole group in order. The delta-timestamp baseline starts at 0, so
		// the first frame's delta is its absolute timestamp (the subscriber decodes
		// against the same baseline).
		let mut index = 0usize;
		let mut prev_ts: u64 = 0;
		while let Some(mut frame) = group.get_frame(index).await? {
			write_fetch_frame(&mut stream.writer, &mut frame, timescale, &mut prev_ts, &track_stats).await?;
			index += 1;
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

		match recv_next(&mut subscriber, true).await.unwrap() {
			Recv::Datagram(datagram) => assert_eq!(&datagram.payload[..], b"last"),
			_ => panic!("expected datagram before finished"),
		}

		match recv_next(&mut subscriber, true).await.unwrap() {
			Recv::Finished => {}
			_ => panic!("expected finished after datagram"),
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
	track_stats: &crate::PublisherTrack,
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
/// best-effort datagram to forward, or the track finishing.
enum Recv {
	Group(group::Consumer),
	Datagram(crate::Datagram),
	Finished,
}

/// Poll a single [`track::Subscriber`] for the next group (cap-aware) or datagram from one `&mut`
/// borrow, so groups and datagrams share the same subscription. Groups are polled first so a
/// datagram burst can't starve them; datagrams are polled only when the transport carries them.
async fn recv_next(track: &mut track::Subscriber, datagrams: bool) -> Result<Recv, Error> {
	kio::wait(|waiter| {
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
		if groups_finished {
			return Poll::Ready(Ok(Recv::Finished));
		}
		Poll::Pending
	})
	.await
}

/// Shared per-subscription state for the publisher side. Cloned cheaply. Every
/// field is either small or already Arc-backed) for each spawned serve_group task
/// so each in-flight group reads the latest SUBSCRIBE_UPDATE priority via its own
/// watch::Receiver.
#[derive(Clone)]
struct Subscription<S: web_transport_trait::Session> {
	session: S,
	id: u64,
	track_name: Arc<str>,
	track_stats: Arc<crate::PublisherTrack>,
	priority: PriorityQueue,
	track_priority: tokio::sync::watch::Receiver<u8>,
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
		track_priority_tx: &tokio::sync::watch::Sender<u8>,
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
		// once the first group is known, SUBSCRIBE_END when the track finishes.
		let emit_range = self.version.has_track_stream();
		let mut start_sent = false;

		// Serve datagrams off this same subscriber, but only on lite-05 over a datagram-capable
		// transport (qmux/WebSocket/TCP/UDS report size 0). No group fallback: otherwise off.
		let datagrams = self.version.has_datagrams() && self.session.max_datagram_size() > 0;

		loop {
			tokio::select! {
				// Drive in-flight group futures; never matches because the inner block returns false.
				true = async {
					while tasks.next().await.is_some() {}
					false
				} => unreachable!(),

				// One cursor drives the whole subscription: poll the cap-aware next group and,
				// when enabled, the next best-effort datagram. Groups are polled first so a
				// datagram burst can't starve them; datagrams flow whenever no group is ready
				// (including while groups are paused above the cap).
				res = recv_next(&mut track, datagrams) => {
					match res? {
						Recv::Group(group) => {
							if emit_range && !start_sent {
								start_sent = true;
								writer
									.encode(&lite::SubscribeResponse::Start(lite::SubscribeStart { group: group.sequence }))
									.await?;
							}
							self.spawn_serve(group, &mut tasks);
						}
						Recv::Datagram(datagram) => self.serve_datagram(datagram),
						Recv::Finished => {
							// Track finished cleanly. Tell the subscriber no group will
							// follow, then drain in-flight tasks and exit.
							if emit_range {
								let group = track
									.latest()
									.map(|group| group.checked_add(1).ok_or(crate::coding::BoundsExceeded))
									.transpose()?
									.unwrap_or(0);
								writer
									.encode(&lite::SubscribeResponse::End(lite::SubscribeEnd { group }))
									.await?;
							}
							while tasks.next().await.is_some() {}
							return Ok(());
						}
					}
				}

				// SUBSCRIBE_UPDATE messages share this hot loop; safe because
				// decode_maybe is cancel-safe given quinn/qmux's cancel-safe
				// read primitives (see Reader::decode_maybe doc).
				upd = reader.decode_maybe::<lite::SubscribeUpdate>() => {
					let Some(upd) = upd? else {
						// Peer FIN'd. They're done with this subscription. Drop any
						// in-flight serve_group tasks (don't drain) so half-sent
						// groups get cancelled rather than completed pointlessly.
						return Ok(());
					};
					let _ = track_priority_tx.send(upd.priority);
					track.end_at(upd.end_group);
				}
			}
		}
	}

	fn spawn_serve(&mut self, group: group::Consumer, tasks: &mut FuturesUnordered<MaybeSendBox<'static, ()>>) {
		let sequence = group.sequence;
		tracing::debug!(subscribe = self.id, track = %self.track_name, sequence, "serving group");

		// Use the latest priority for new groups so SUBSCRIBE_UPDATE applies to them too.
		let current_priority = *self.track_priority.borrow_and_update();
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

		if body.len() <= self.session.max_datagram_size() && self.session.send_datagram(body).is_ok() {
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
		loop {
			tokio::select! {
				biased;
				_ = stream.closed() => return Err(Error::Cancel),
				frame = group.next_frame() => return frame,
				new_pri = priority.next() => stream.set_priority(new_pri),
				Ok(()) = self.track_priority.changed() => priority.set_track(*self.track_priority.borrow_and_update()),
			}
		}
	}

	/// Await the next chunk of `frame`, applying priority changes meanwhile.
	async fn read_chunk(
		&mut self,
		stream: &mut Writer<S::SendStream, Version>,
		priority: &mut PriorityHandle,
		frame: &mut frame::Consumer,
	) -> Result<Option<bytes::Bytes>, Error> {
		loop {
			tokio::select! {
				biased;
				_ = stream.closed() => return Err(Error::Cancel),
				chunk = frame.read_chunk() => return chunk,
				new_pri = priority.next() => stream.set_priority(new_pri),
				Ok(()) = self.track_priority.changed() => priority.set_track(*self.track_priority.borrow_and_update()),
			}
		}
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
		priority.set_track(*self.track_priority.borrow_and_update());
		stream.set_priority(priority.current());
	}
}
