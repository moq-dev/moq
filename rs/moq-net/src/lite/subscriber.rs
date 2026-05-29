use std::{
	collections::{HashMap, hash_map::Entry},
	sync::{Arc, atomic},
	time::Duration,
};

use futures::{StreamExt, stream::FuturesUnordered};

use crate::{
	AsPath, BandwidthProducer, Broadcast, BroadcastDynamic, Compression, Error, Frame, FrameProducer, Group,
	GroupProducer, MAX_FRAME_SIZE, OriginProducer, Path, PathOwned, StatsHandle, SubscriberStats, SubscriberTrack,
	TrackProducer,
	coding::{Reader, Stream},
	lite,
	model::BroadcastProducer,
};

use super::Version;

use web_async::Lock;

/// Keep an upstream subscription alive briefly after the last consumer leaves,
/// so a returning subscriber reuses the same TrackProducer instead of forcing a
/// fresh fetch (and the publisher re-serving the latest cached group).
const LINGER_TIMEOUT: Duration = Duration::from_secs(5);

pub(super) struct SubscriberConfig<S: web_transport_trait::Session> {
	pub session: S,
	/// The origin into which remote broadcasts are inserted.
	pub origin: Option<OriginProducer>,
	/// Receiver-side bandwidth producer for PROBE feedback. None disables the
	/// feature (used by versions that don't carry probe streams).
	pub recv_bandwidth: Option<BandwidthProducer>,
	/// Stats aggregator for this session's ingress. Use [`StatsHandle::disabled`]
	/// to opt out.
	pub stats: StatsHandle,
	pub version: Version,
}

#[derive(Clone)]
pub(super) struct Subscriber<S: web_transport_trait::Session> {
	session: S,

	origin: Option<OriginProducer>,
	stats: StatsHandle,
	recv_bandwidth: Option<BandwidthProducer>,
	// Session-level origin id shared with the Publisher. Used to filter out
	// reflected announces: we ask the peer (via AnnounceInterest.exclude_hop)
	// to skip broadcasts whose hop chain already passed through us, and we
	// double-check incoming announces against it as defense in depth.
	self_origin: crate::Origin,
	subscribes: Lock<HashMap<u64, TrackEntry>>,
	next_id: Arc<atomic::AtomicU64>,
	version: Version,
}

#[derive(Clone)]
struct TrackEntry {
	producer: TrackProducer,
	stats: Arc<SubscriberTrack>,
	/// Codec for this subscription, learned from SUBSCRIBE_OK. `None` until that
	/// message arrives; group streams block on it before decoding any frame,
	/// since a group can race ahead of SUBSCRIBE_OK on its own QUIC stream.
	compression: tokio::sync::watch::Receiver<Option<Compression>>,
}

/// Result of an upstream subscribe lifecycle.
enum SessionOutcome {
	/// The upstream cleanly FIN'd the subscribe stream — nothing more to deliver.
	Complete,
	/// Linger timeout expired without anyone returning, or Lite01/02 hit the no-linger path.
	Cancelled,
	/// The entire broadcast went away on the publisher side.
	BroadcastClosed(Error),
	/// Anything else (wire error, protocol violation, encode failure).
	Error(Error),
}

impl<S: web_transport_trait::Session> Subscriber<S> {
	pub fn new(config: SubscriberConfig<S>) -> Self {
		// Identity for incoming-hop loop detection. Derived from the local
		// origin we publish into so it matches the relay identity across
		// every session sharing that origin, required for cross-session
		// loop detection. If no origin is attached (the announce loop is
		// inert anyway), fall back to a random session-local id.
		let self_origin = config.origin.as_deref().copied().unwrap_or_else(crate::Origin::random);
		Self {
			session: config.session,
			origin: config.origin,
			stats: config.stats,
			recv_bandwidth: config.recv_bandwidth,
			self_origin,
			subscribes: Default::default(),
			next_id: Default::default(),
			version: config.version,
		}
	}

	pub async fn run(self) -> Result<(), Error> {
		let bw = self.clone();
		tokio::select! {
			Err(err) = self.clone().run_announce() => Err(err),
			res = self.run_uni() => res,
			Err(err) = bw.run_recv_bandwidth() => Err(err),
		}
	}

	async fn run_uni(self) -> Result<(), Error> {
		loop {
			let stream = self.session.accept_uni().await.map_err(Error::from_transport)?;

			let stream = Reader::new(stream, self.version);
			let this = self.clone();

			web_async::spawn(async move {
				if let Err(err) = this.run_uni_stream(stream).await {
					tracing::debug!(%err, "error running uni stream");
				}
			});
		}
	}

	async fn run_uni_stream(mut self, mut stream: Reader<S::RecvStream, Version>) -> Result<(), Error> {
		let kind = stream.decode().await?;

		let res = match kind {
			lite::DataType::Group => self.recv_group(&mut stream).await,
		};

		if let Err(err) = res {
			stream.abort(&err);
		}

		Ok(())
	}

	async fn run_announce(self) -> Result<(), Error> {
		let origin = match &self.origin {
			Some(origin) => origin,
			None => return Ok(()),
		};

		let prefixes: Vec<PathOwned> = origin.allowed().map(|p| p.to_owned()).collect();

		let mut tasks = FuturesUnordered::new();
		for prefix in prefixes {
			tasks.push(self.clone().run_announce_prefix(prefix));
		}

		while let Some(result) = tasks.next().await {
			result?;
		}

		Ok(())
	}

	async fn run_announce_prefix(mut self, prefix: PathOwned) -> Result<(), Error> {
		let mut stream = Stream::open(&self.session, self.version).await?;
		stream.writer.encode(&lite::ControlType::Announce).await?;

		// Ask the peer to filter out announces that already passed through us, so
		// reflected announces (the simple loop case) never hit the wire. Lite03
		// peers ignore this field, in which case start_announce below still drops.
		let msg = lite::AnnounceInterest {
			prefix: prefix.as_path(),
			exclude_hop: self.self_origin.id,
		};
		stream.writer.encode(&msg).await?;

		let mut producers = HashMap::new();
		// Per-broadcast subscriber-side stats guards. Dropping the guard records
		// `subscriber.broadcasts_closed`. We only insert a guard when start_announce
		// actually accepted the announcement (it may drop reflected loops), so the
		// guard set tracks `producers` exactly.
		let mut stats_guards: HashMap<PathOwned, SubscriberStats> = HashMap::new();

		// Stats keys are absolute paths (matching the publisher side) so the
		// fanned-out level keys line up with the absolute broadcast paths a
		// dashboard sees on the origin.

		match self.version {
			Version::Lite01 | Version::Lite02 => {
				let msg: lite::AnnounceInit = stream.reader.decode().await?;
				for suffix in msg.suffixes {
					let path = prefix.join(&suffix);
					let abs = self.origin.as_ref().unwrap().absolute(&path).to_owned();
					// Lite01/02 don't carry hop information; the broadcast starts with an empty chain.
					if self.start_announce(path.clone(), crate::OriginList::new(), &mut producers)? {
						stats_guards.insert(abs.clone(), self.stats.broadcast(&abs).subscriber());
					}
				}
			}
			_ => {
				// Lite03+: no AnnounceInit, initial state comes via Announce messages.
			}
		}

		while let Some(announce) = stream.reader.decode_maybe::<lite::Announce>().await? {
			match announce {
				lite::Announce::Active { suffix, hops } => {
					let path = prefix.join(&suffix);
					let abs = self.origin.as_ref().unwrap().absolute(&path).to_owned();
					if self.start_announce(path.clone(), hops, &mut producers)? {
						stats_guards.insert(abs.clone(), self.stats.broadcast(&abs).subscriber());
					}
				}
				lite::Announce::Ended { suffix, .. } => {
					let path = prefix.join(&suffix);
					tracing::debug!(broadcast = %self.log_path(&path), "unannounced");

					// The matching Active may have been silently dropped by
					// start_announce as a reflected loop, in which case
					// `producers` has no entry; that's expected, not an error.
					if let Some(mut producer) = producers.remove(&path) {
						producer.abort(Error::Cancel).ok();
						let abs = self.origin.as_ref().unwrap().absolute(&path).to_owned();
						stats_guards.remove(&abs);
					}
				}
			}
		}

		// Close the stream when there's nothing more to announce.
		stream.writer.finish()?;
		stream.writer.closed().await
	}

	/// Opens a PROBE stream on demand while a consumer is interested.
	///
	/// PROBE measures the peer's upload bandwidth to us, which is only meaningful
	/// when the peer is publishing broadcasts. If we have no origin to insert
	/// remote broadcasts into, skip the probe stream entirely.
	///
	/// Otherwise loop forever: wait for a consumer, race the probe stream against
	/// the consumer leaving, then loop back. Probe is best-effort, so stream
	/// errors are logged but never tear down the session.
	async fn run_recv_bandwidth(self) -> Result<(), Error> {
		if self.origin.is_none() {
			return Ok(());
		}

		let Some(bandwidth) = &self.recv_bandwidth else {
			return Ok(());
		};

		loop {
			// Wait until at least one consumer is interested in the estimate.
			if bandwidth.used().await.is_err() {
				return Ok(());
			}

			tokio::select! {
				res = bandwidth.unused() => {
					if res.is_err() {
						return Ok(());
					}
					// Loop back: a new consumer may arrive later.
				}
				res = self.run_probe_stream(bandwidth) => {
					match res {
						Ok(()) => tracing::debug!("probe stream closed"),
						Err(err) => tracing::warn!(%err, "probe stream error"),
					}
					// Stream ended (peer FIN'd or errored). Don't hammer an
					// uncooperative peer; give up for the rest of the session.
					return Ok(());
				}
			}
		}
	}

	async fn run_probe_stream(&self, bandwidth: &BandwidthProducer) -> Result<(), Error> {
		let mut stream = Stream::open(&self.session, self.version).await?;
		stream.writer.encode(&lite::ControlType::Probe).await?;

		while let Some(probe) = stream.reader.decode_maybe::<lite::Probe>().await? {
			bandwidth.set(Some(probe.bitrate))?;
		}

		Ok(())
	}

	/// Returns `Ok(true)` if the announce was accepted (and the broadcast was
	/// published into the origin), `Ok(false)` if it was dropped as a
	/// reflected loop.
	fn start_announce(
		&mut self,
		path: PathOwned,
		hops: crate::OriginList,
		producers: &mut HashMap<PathOwned, BroadcastProducer>,
	) -> Result<bool, Error> {
		// Drop announces that already passed through us — this connection is
		// a reflection, not a new path. Peers should be filtering via
		// AnnounceInterest.exclude_hop, but Lite03 peers can't, so this is
		// the authoritative cluster-loop check on the receiver.
		if hops.contains(&self.self_origin) {
			tracing::debug!(broadcast = %self.log_path(&path), "dropping reflected announce");
			return Ok(false);
		}

		tracing::debug!(broadcast = %self.log_path(&path), hops = hops.len(), "announce");

		let broadcast = Broadcast { hops }.produce();

		// Make sure the peer doesn't double announce.
		match producers.entry(path.to_owned()) {
			Entry::Occupied(_) => return Err(Error::Duplicate),
			Entry::Vacant(entry) => entry.insert(broadcast.clone()),
		};

		// Create the dynamic handler BEFORE publishing, so that consumers
		// see dynamic >= 1 immediately when they receive the announcement.
		// Otherwise there's a race on multi-threaded runtimes where a consumer
		// can call subscribe_track() before dynamic is incremented, getting NotFound.
		let dynamic = broadcast.dynamic();

		// Run the broadcast in the background until all consumers are dropped.
		self.origin
			.as_mut()
			.unwrap()
			.publish_broadcast(path.clone(), broadcast.consume());

		web_async::spawn(self.clone().run_broadcast(path, dynamic));

		Ok(true)
	}

	async fn run_broadcast(self, path: PathOwned, mut broadcast: BroadcastDynamic) {
		// Actually start serving subscriptions.
		loop {
			// Keep serving requests until there are no more consumers.
			// This way we'll clean up the task when the broadcast is no longer needed.
			let track = tokio::select! {
				producer = broadcast.requested_track() => match producer {
					Ok(producer) => producer,
					Err(err) => {
						tracing::debug!(%err, "broadcast closed");
						break;
					}
				},
				_ = self.session.closed() => break,
			};

			let mut this = self.clone();
			let path = path.clone();
			let broadcast = broadcast.clone();
			web_async::spawn(async move {
				this.run_subscribe(path, broadcast, track).await;
			});
		}
	}

	/// Drive one upstream subscription end-to-end, including linger across consumer churn.
	///
	/// On linger entry (last consumer drops) we send `SubscribeUpdate(priority=0,
	/// end_group=Some(latest))`. The publisher treats `end_group` as a serving cap,
	/// not a terminator: it holds any groups beyond the cap and resumes when we
	/// raise it. On resume (a new consumer arrives) we send `SubscribeUpdate(end_group=None)`
	/// to uncap. The stream stays open across the whole lifecycle — only a timeout
	/// or a publisher-side close ends it. This avoids the stream-churn / duplicate-fetch
	/// race that an unsubscribe-and-reissue approach would have.
	async fn run_subscribe(&mut self, path: PathOwned, broadcast: BroadcastDynamic, mut track: TrackProducer) {
		// Subscriber-side track stats; counters bump as frames/bytes/groups arrive.
		// Drop on subscription end records `subscriber.subscriptions_closed`. We use
		// subscriber_track to avoid double-counting broadcasts: the broadcast lifetime
		// is tracked separately by the announce loop's `stats_guards`.
		let abs = self.origin.as_ref().unwrap().absolute(&path);
		let track_stats = Arc::new(self.stats.broadcast(&abs).subscriber_track(&track.name));

		let id = self.next_id.fetch_add(1, atomic::Ordering::Relaxed);

		// Resolved once SUBSCRIBE_OK arrives; group streams wait on the receiver.
		let (compression_tx, compression_rx) = tokio::sync::watch::channel(None);
		self.subscribes.lock().insert(
			id,
			TrackEntry {
				producer: track.clone(),
				stats: track_stats.clone(),
				compression: compression_rx,
			},
		);

		let msg = lite::Subscribe {
			id,
			broadcast: path.as_path(),
			track: (&track.name).into(),
			priority: track.priority,
			ordered: true,
			max_latency: Duration::ZERO,
			start_group: None,
			end_group: None,
		};

		tracing::info!(
			id,
			broadcast = %self.log_path(&path),
			track = %track.name,
			"subscribe started"
		);

		let result = tokio::select! {
			err = broadcast.closed() => SessionOutcome::BroadcastClosed(err),
			res = self.run_subscribe_session(&track, msg, compression_tx) => res,
		};

		self.subscribes.lock().remove(&id);

		match result {
			SessionOutcome::Complete => {
				tracing::info!(broadcast = %self.log_path(&path), track = %track.name, "subscribe complete");
				let _ = track.finish();
			}
			SessionOutcome::Cancelled => {
				tracing::info!(broadcast = %self.log_path(&path), track = %track.name, "subscribe cancelled");
				let _ = track.abort(Error::Cancel);
			}
			SessionOutcome::BroadcastClosed(err) => {
				tracing::info!(broadcast = %self.log_path(&path), track = %track.name, %err, "broadcast closed");
				let _ = track.abort(err);
			}
			SessionOutcome::Error(err) => {
				tracing::warn!(broadcast = %self.log_path(&path), track = %track.name, %err, "subscribe error");
				let _ = track.abort(err);
			}
		}
	}

	async fn run_subscribe_session(
		&self,
		track: &TrackProducer,
		msg: lite::Subscribe<'_>,
		compression_tx: tokio::sync::watch::Sender<Option<Compression>>,
	) -> SessionOutcome {
		// Stash the original parameters so SubscribeUpdate messages can echo them
		// while only varying the linger-related fields (priority, end_group).
		let original_priority = msg.priority;
		let ordered = msg.ordered;
		let max_latency = msg.max_latency;
		let start_group = msg.start_group;

		// SubscribeUpdate only exists on Lite03+; older versions take the
		// immediate-FIN path with no linger.
		let supports_linger = !matches!(self.version, Version::Lite01 | Version::Lite02);

		let mut stream = match Stream::open(&self.session, self.version).await {
			Ok(s) => s,
			Err(err) => return SessionOutcome::Error(err),
		};

		if let Err(err) = stream.writer.encode(&lite::ControlType::Subscribe).await {
			return SessionOutcome::Error(err);
		}

		if let Err(err) = stream.writer.encode(&msg).await {
			stream.writer.abort(&err);
			return SessionOutcome::Error(err);
		}

		let resp = match stream.reader.decode::<lite::SubscribeResponse>().await {
			Ok(r) => r,
			Err(err) => {
				stream.writer.abort(&err);
				return SessionOutcome::Error(err);
			}
		};
		let lite::SubscribeResponse::Ok(info) = resp else {
			let err = Error::ProtocolViolation;
			stream.writer.abort(&err);
			return SessionOutcome::Error(err);
		};

		// Unblock any group streams waiting to learn how to decode frames. A send
		// error just means every receiver already left, which is harmless here.
		let _ = compression_tx.send(Some(info.compression));

		// Lifecycle loop: serve → linger → resume → serve → ... → FIN.
		loop {
			// Phase 1 — serving. Wait for either:
			// - the last consumer to drop (enter linger), or
			// - the upstream to close the stream (subscription is over).
			tokio::select! {
				_ = track.unused() => {}
				res = stream.reader.closed() => {
					if let Err(err) = res {
						return SessionOutcome::Error(err);
					}
					let _ = stream.writer.finish();
					return SessionOutcome::Complete;
				}
			}

			// No linger on Lite01/02: FIN and report cancellation.
			if !supports_linger {
				let _ = stream.writer.finish();
				return SessionOutcome::Cancelled;
			}

			// Phase 2 — linger. Send SubscribeUpdate that caps the publisher's
			// serving cursor at the latest group we've cached, and drops priority
			// to 0. The publisher holds any group beyond the cap until we resume
			// or FIN. `unwrap_or(0)` handles the corner case where we subscribed
			// but haven't received a group yet — capping at 0 is conservative.
			let cap = track.latest().unwrap_or(0);
			let pause = lite::SubscribeUpdate {
				priority: 0,
				ordered,
				max_latency,
				start_group,
				end_group: Some(cap),
			};
			if let Err(err) = stream.writer.encode(&pause).await {
				stream.writer.abort(&err);
				return SessionOutcome::Error(err);
			}

			// Race three outcomes during the linger window:
			// - publisher closes the stream (it's done): Complete
			// - timeout expires: FIN and report Cancelled
			// - a new consumer arrives: send the uncap update and re-enter Phase 1
			let resume = tokio::select! {
				res = stream.reader.closed() => {
					if let Err(err) = res {
						return SessionOutcome::Error(err);
					}
					return SessionOutcome::Complete;
				}
				_ = tokio::time::sleep(LINGER_TIMEOUT) => {
					let _ = stream.writer.finish();
					return SessionOutcome::Cancelled;
				}
				res = track.used() => res,
			};
			if let Err(err) = resume {
				return SessionOutcome::Error(err);
			}

			tracing::info!(track = %track.name, "subscribe resumed");

			let uncap = lite::SubscribeUpdate {
				priority: original_priority,
				ordered,
				max_latency,
				start_group,
				end_group: None,
			};
			if let Err(err) = stream.writer.encode(&uncap).await {
				stream.writer.abort(&err);
				return SessionOutcome::Error(err);
			}
			// Loop back to Phase 1.
		}
	}

	pub async fn recv_group(&mut self, stream: &mut Reader<S::RecvStream, Version>) -> Result<(), Error> {
		let hdr: lite::Group = stream.decode().await?;

		let (mut group, track, track_stats, mut compression_rx) = {
			let mut subs = self.subscribes.lock();
			let entry = subs.get_mut(&hdr.subscribe).ok_or(Error::Cancel)?;

			let group_info = Group { sequence: hdr.sequence };
			let group = entry.producer.create_group(group_info)?;
			(
				group,
				entry.producer.clone(),
				entry.stats.clone(),
				entry.compression.clone(),
			)
		};

		// Bump groups counter for this incoming group on the subscriber side.
		track_stats.group();

		// Block until SUBSCRIBE_OK tells us the codec. The group's QUIC stream can
		// arrive before SUBSCRIBE_OK lands on the subscribe stream, so we can't
		// decode frames until this resolves. A receiver error means the
		// subscription ended before SUBSCRIBE_OK, so treat it as cancelled.
		let compression = {
			let guard = compression_rx
				.wait_for(Option::is_some)
				.await
				.map_err(|_| Error::Cancel)?;
			(*guard).expect("present after wait_for")
		};

		let res = tokio::select! {
			err = track.closed() => Err(err),
			err = group.closed() => Err(err),
			res = self.run_group(stream, group.clone(), track_stats.clone(), compression) => res,
		};

		match res {
			Err(Error::Cancel) => {
				let _ = group.abort(Error::Cancel);
			}
			Err(err) => {
				tracing::debug!(%err, group = %group.sequence, "group error");
				let _ = group.abort(err);
			}
			_ => {
				let _ = group.finish();
			}
		}

		Ok(())
	}

	async fn run_group(
		&mut self,
		stream: &mut Reader<S::RecvStream, Version>,
		mut group: GroupProducer,
		track_stats: Arc<SubscriberTrack>,
		compression: Compression,
	) -> Result<(), Error> {
		while let Some(size) = stream.decode_maybe::<u64>().await? {
			if size > MAX_FRAME_SIZE {
				return Err(Error::FrameTooLarge);
			}

			match compression {
				Compression::None => {
					let mut frame = group.create_frame(Frame { size })?;
					track_stats.frame();

					if let Err(err) = self.run_frame(stream, &mut frame, &track_stats).await {
						let _ = frame.abort(err.clone());
						return Err(err);
					}

					frame.finish()?;
				}
				compression => {
					// `size` is the compressed length; pull it off the wire, then
					// inflate. The frame the consumer sees carries the original size.
					let packed = stream.read_exact(size as usize).await?;
					track_stats.frame();
					track_stats.bytes(size);

					let payload = compression.decompress(&packed)?;
					let mut frame = group.create_frame(Frame {
						size: payload.len() as u64,
					})?;
					frame.write(bytes::Bytes::from(payload))?;
					frame.finish()?;
				}
			}
		}

		Ok(())
	}

	async fn run_frame(
		&mut self,
		stream: &mut Reader<S::RecvStream, Version>,
		frame: &mut FrameProducer,
		track_stats: &SubscriberTrack,
	) -> Result<(), Error> {
		// FrameProducer impls BufMut over its pre-allocated per-frame buffer, so
		// read_buf writes QUIC stream bytes directly into the frame — no
		// intermediate Bytes allocations, and quinn's reassembly arena is freed
		// as we drain it.
		while bytes::BufMut::has_remaining_mut(frame) {
			match stream.read_buf(frame).await? {
				Some(n) if n > 0 => {
					track_stats.bytes(n as u64);
				}
				_ => return Err(Error::WrongSize),
			}
		}
		Ok(())
	}

	fn log_path(&self, path: impl AsPath) -> Path<'_> {
		self.origin.as_ref().unwrap().root().join(path)
	}
}
