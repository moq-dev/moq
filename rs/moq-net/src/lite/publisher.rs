use std::{sync::Arc, time::Duration};

use futures::{FutureExt, StreamExt, stream::FuturesUnordered};
use web_transport_trait::Stats;

use crate::{
	AnnounceConsumer, AsPath, BroadcastConsumer, Compression, Error, Origin, OriginConsumer, OriginList,
	StatsHandle as MoqStats, Track, TrackConsumer,
	coding::{Stream, Writer},
	lite::{
		self,
		priority::{Priority, PriorityHandle, PriorityQueue},
	},
	model::{FrameConsumer, GroupConsumer},
};

use super::Version;

pub(super) struct PublisherConfig<S: web_transport_trait::Session> {
	pub session: S,
	/// The origin we read local broadcasts from. None gives this session a
	/// dummy, immediately-closed origin (i.e. nothing to publish).
	pub origin: Option<OriginConsumer>,
	/// Stats aggregator for this session's egress. Use [`MoqStats::disabled`]
	/// to opt out.
	pub stats: MoqStats,
	pub version: Version,
}

pub(super) struct Publisher<S: web_transport_trait::Session> {
	session: S,
	origin: OriginConsumer,
	stats: MoqStats,
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
		Self {
			session: config.session,
			origin,
			stats: config.stats,
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

		let origin = self.origin.scope(&[prefix.as_path()]).ok_or(Error::Unauthorized)?;
		let mut announced = origin.announced();

		let version = self.version;
		let self_origin = self.self_origin;
		let stats = self.stats.clone();
		web_async::spawn(async move {
			if let Err(err) = Self::run_announce(
				&mut stream,
				&origin,
				&mut announced,
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

	#[allow(clippy::too_many_arguments)]
	async fn run_announce(
		stream: &mut Stream<S, Version>,
		origin: &OriginConsumer,
		announced: &mut AnnounceConsumer,
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
				while let Some((path, active)) = announced.try_next() {
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
				next = announced.next() => {
					match next {
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
								let guard = stats.broadcast(&absolute).publisher();
								let prev = stats_guards.insert(absolute, guard);
								debug_assert!(prev.is_none(), "origin announced a path that was already active");
								let msg = lite::Announce::Active { suffix, hops };
								stream.writer.encode(&msg).await?;
							} else {
								tracing::debug!(broadcast = %origin.absolute(&path), "unannounce");
								stats_guards.remove(&origin.absolute(&path).to_owned());
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

		// Per-track subscription guard. The broadcast itself is tracked elsewhere by
		// run_announce, so we use publisher_track to avoid double-counting broadcasts.
		let track_stats = self.stats.broadcast(&absolute).publisher_track(&track);

		let session = self.session.clone();
		web_async::spawn(async move {
			if let Err(err) = Self::run_subscribe(
				session,
				&mut stream,
				&subscribe,
				broadcast,
				priority,
				track_stats,
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
		track_stats: crate::PublisherTrack,
		version: Version,
	) -> Result<(), Error> {
		let track = Track {
			name: subscribe.track.to_string(),
			priority: subscribe.priority,
			compress: false,
		};

		let broadcast = consumer.ok_or(Error::NotFound)?;
		let track = broadcast.subscribe_track(&track)?;

		// TODO wait until track.info() to get the *real* priority

		// Compress only when the producer marked the track worth it and the
		// negotiated draft understands the SUBSCRIBE_OK codec field. Older drafts
		// (lite-04 and below) get None and the frames stream verbatim.
		let supports_compression = !matches!(
			version,
			Version::Lite01 | Version::Lite02 | Version::Lite03 | Version::Lite04
		);
		let compression = if track.compress && supports_compression {
			Compression::Deflate
		} else {
			Compression::None
		};

		let info = lite::SubscribeOk {
			priority: track.priority,
			ordered: false,
			max_latency: std::time::Duration::ZERO,
			start_group: None,
			end_group: None,
			compression,
		};

		stream.writer.encode(&lite::SubscribeResponse::Ok(info)).await?;

		// Track-level subscriber priority. SUBSCRIBE_UPDATE messages broadcast new values
		// to both run_track (so future groups inherit the new priority) and serve_group
		// tasks (so in-flight groups update via PriorityHandle::set_track). The Sender
		// stays in run_subscribe and gets handed to run_track so the same loop that
		// parses SUBSCRIBE_UPDATEs also fans the new priority out.
		let (track_priority_tx, track_priority_rx) = tokio::sync::watch::channel(track.priority);

		let sub = Subscription {
			session,
			id: subscribe.id,
			track_name: Arc::from(track.name.as_str()),
			track_stats: Arc::new(track_stats),
			priority,
			track_priority: track_priority_rx,
			version,
			compression,
		};

		// `end_group` is a serving cap, not a subscription terminator: groups with
		// sequence > cap are held in the producer's cache until the subscriber raises
		// the cap (or unsets it) via SUBSCRIBE_UPDATE, then served in order. Only a
		// peer FIN actually ends the subscription. This is what lets relays pause an
		// upstream subscription across consumer churn without tearing it down.
		sub.run_track(
			track,
			subscribe.start_group,
			subscribe.end_group,
			&mut stream.reader,
			&track_priority_tx,
		)
		.await?;

		stream.writer.finish()?;
		stream.writer.closed().await
	}
}

/// Shared per-subscription state for the publisher side. Cloned (cheaply — every
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
	/// Codec announced in SUBSCRIBE_OK; every frame on this subscription is
	/// compressed with it before hitting the wire.
	compression: Compression,
}

impl<S: web_transport_trait::Session> Subscription<S> {
	async fn run_track(
		mut self,
		mut track: TrackConsumer,
		start_group: Option<u64>,
		initial_end_group: Option<u64>,
		reader: &mut crate::coding::Reader<S::RecvStream, Version>,
		track_priority_tx: &tokio::sync::watch::Sender<u8>,
	) -> Result<(), Error> {
		let mut tasks: FuturesUnordered<futures::future::BoxFuture<'static, ()>> = FuturesUnordered::new();

		// Start the consumer at the specified sequence, otherwise start at the latest group.
		if let Some(start_group) = start_group.or_else(|| track.latest()) {
			track.start_at(start_group);
		}

		// Apply the initial cap from the original Subscribe. Subsequent updates
		// flow through the SubscribeUpdate select arm below.
		track.end_at(initial_end_group);

		loop {
			tokio::select! {
				// Drive in-flight group futures; never matches because the inner block returns false.
				true = async {
					while tasks.next().await.is_some() {}
					false
				} => unreachable!(),

				// next_group respects the cap set via track.end_at and parks
				// while the next sequence is above the cap. Groups beyond the
				// cap stay in the producer's cache (bounded by its 5s eviction).
				res = track.next_group() => {
					match res? {
						Some(group) => self.spawn_serve(group, &mut tasks),
						None => {
							// Track finished cleanly; drain in-flight tasks and exit.
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
						// Peer FIN'd — they're done with this subscription. Drop any
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

	fn spawn_serve(
		&mut self,
		group: GroupConsumer,
		tasks: &mut FuturesUnordered<futures::future::BoxFuture<'static, ()>>,
	) {
		let sequence = group.sequence;
		tracing::debug!(subscribe = self.id, track = %self.track_name, sequence, "serving group");

		// Use the latest priority for new groups so SUBSCRIBE_UPDATE applies to them too.
		let current_priority = *self.track_priority.borrow_and_update();
		let handle = self.priority.insert(Priority::new(current_priority, sequence));
		let fut = self.clone().serve_group(sequence, handle, group);
		tasks.push(fut.map(|_| ()).boxed());
	}

	async fn serve_group(
		mut self,
		sequence: u64,
		mut priority: PriorityHandle,
		mut group: GroupConsumer,
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

		while let Some(frame) = self.next_frame(&mut stream, &mut priority, &mut group).await? {
			self.serve_frame(&mut stream, &mut priority, frame).await?;
		}

		stream.finish()?;
		stream.closed().await?;

		tracing::debug!(sequence, "finished group");

		Ok(())
	}

	/// Send one frame. Uncompressed frames stream chunk-by-chunk so we never
	/// buffer the whole payload; a compressed frame must buffer to feed the
	/// codec, and its wire size becomes the compressed length (the subscriber
	/// inflates it from the codec in SUBSCRIBE_OK).
	async fn serve_frame(
		&mut self,
		stream: &mut Writer<S::SendStream, Version>,
		priority: &mut PriorityHandle,
		mut frame: FrameConsumer,
	) -> Result<(), Error> {
		match self.compression {
			Compression::None => {
				stream.encode(&frame.size).await?;
				self.track_stats.frame();

				while let Some(chunk) = self.read_chunk(stream, priority, &mut frame).await? {
					self.write_chunk(stream, priority, chunk).await?;
				}
			}
			compression => {
				let payload = self.read_all(stream, priority, &mut frame).await?;
				let chunk = bytes::Bytes::from(compression.compress(&payload));
				stream.encode(&(chunk.len() as u64)).await?;
				self.track_stats.frame();
				self.write_chunk(stream, priority, chunk).await?;
			}
		}

		Ok(())
	}

	/// Await the next frame in the group, applying any priority changes that
	/// arrive meanwhile. Errors with [`Error::Cancel`] if the peer closes first.
	async fn next_frame(
		&mut self,
		stream: &mut Writer<S::SendStream, Version>,
		priority: &mut PriorityHandle,
		group: &mut GroupConsumer,
	) -> Result<Option<FrameConsumer>, Error> {
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
		frame: &mut FrameConsumer,
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

	/// Await the full frame payload, applying priority changes meanwhile.
	async fn read_all(
		&mut self,
		stream: &mut Writer<S::SendStream, Version>,
		priority: &mut PriorityHandle,
		frame: &mut FrameConsumer,
	) -> Result<bytes::Bytes, Error> {
		loop {
			tokio::select! {
				biased;
				_ = stream.closed() => return Err(Error::Cancel),
				data = frame.read_all() => return data,
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
		loop {
			tokio::select! {
				biased;
				result = stream.write_all(&mut chunk) => {
					result?;
					break;
				}
				new_pri = priority.next() => stream.set_priority(new_pri),
				Ok(()) = self.track_priority.changed() => priority.set_track(*self.track_priority.borrow_and_update()),
			}
		}
		self.track_stats.bytes(n);
		Ok(())
	}
}
