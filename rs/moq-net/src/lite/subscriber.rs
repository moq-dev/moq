use std::{
	collections::{HashMap, hash_map::Entry},
	sync::{Arc, atomic},
};

use futures::{StreamExt, stream::FuturesUnordered};

use crate::{
	AsPath, BandwidthProducer, Broadcast, BroadcastDynamic, Error, Frame, FrameProducer, Group, GroupProducer,
	MAX_FRAME_SIZE, OriginProducer, Path, PathOwned, StatsHandle, SubscriberStats, SubscriberTrack, TrackProducer,
	coding::{Reader, Stream},
	lite,
	model::BroadcastProducer,
};

use super::Version;

use web_async::Lock;

pub(super) struct SubscriberConfig<S: web_transport_trait::Session> {
	pub session: S,
	/// The origin into which remote broadcasts are inserted.
	pub origin: Option<OriginProducer>,
	/// Receiver-side bandwidth producer for PROBE feedback. None disables the
	/// feature (used by versions that don't carry probe streams).
	pub recv_bandwidth: Option<BandwidthProducer>,
	/// Stats aggregator for this session's ingress. Use [`StatsHandle::default`]
	/// to opt out.
	pub stats: StatsHandle,
	pub version: Version,
}

#[derive(Clone)]
pub(super) struct Subscriber<S: web_transport_trait::Session> {
	session: S,

	origin: Option<OriginProducer>,
	stats: StatsHandle,
	/// Per-session ingress broadcast-subscription tracker. Each upstream
	/// subscription holds a guard so `broadcasts - broadcasts_closed` counts the
	/// distinct upstream sessions feeding each broadcast.
	broadcasts: crate::SessionBroadcasts,
	recv_bandwidth: Option<BandwidthProducer>,
	// Session-level origin id shared with the Publisher. Used to filter out
	// reflected announces: we ask the peer (via AnnounceInterest.exclude_hop)
	// to skip broadcasts whose hop chain already passed through us, and we
	// double-check incoming announces against it as defense in depth.
	self_origin: crate::Origin,
	// A random per-connection origin stamped into the hop chain of broadcasts
	// from versions that don't carry real hop ids on the wire (Lite01/02/03).
	// It gives each upstream session a stable, unique identity in the hop list
	// so two sessions publishing the same path resolve as distinct routes
	// instead of colliding on an empty/placeholder chain.
	session_origin: crate::Origin,
	subscribes: Lock<HashMap<u64, TrackEntry>>,
	next_id: Arc<atomic::AtomicU64>,
	version: Version,
}

#[derive(Clone)]
struct TrackEntry {
	producer: TrackProducer,
	stats: Arc<SubscriberTrack>,
}

impl<S: web_transport_trait::Session> Subscriber<S> {
	pub fn new(config: SubscriberConfig<S>) -> Self {
		// Identity for incoming-hop loop detection. Derived from the local
		// origin we publish into so it matches the relay identity across
		// every session sharing that origin, required for cross-session
		// loop detection. If no origin is attached (the announce loop is
		// inert anyway), fall back to a random session-local id.
		let self_origin = config.origin.as_deref().copied().unwrap_or_else(crate::Origin::random);
		let broadcasts = config.stats.subscriber_broadcasts();
		Self {
			session: config.session,
			origin: config.origin,
			stats: config.stats,
			broadcasts,
			recv_bandwidth: config.recv_bandwidth,
			self_origin,
			session_origin: crate::Origin::random(),
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
					// Count every received name, even ones start_announce drops as loops.
					self.stats
						.broadcast(&abs)
						.subscriber_announced_bytes(abs.as_str().len() as u64);
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
					// Count the broadcast name length (not the encoded message size) for
					// every received announce, even ones start_announce drops as loops.
					self.stats
						.broadcast(&abs)
						.subscriber_announced_bytes(abs.as_str().len() as u64);
					if self.start_announce(path.clone(), hops, &mut producers)? {
						stats_guards.insert(abs.clone(), self.stats.broadcast(&abs).subscriber());
					}
				}
				lite::Announce::Ended { suffix, .. } => {
					let path = prefix.join(&suffix);
					tracing::debug!(broadcast = %self.log_path(&path), "unannounced");
					let abs = self.origin.as_ref().unwrap().absolute(&path).to_owned();
					// Count the unannounce name length whether or not a matching guard exists.
					self.stats
						.broadcast(&abs)
						.subscriber_announced_bytes(abs.as_str().len() as u64);

					// The matching Active may have been silently dropped by
					// start_announce as a reflected loop, in which case
					// `producers` has no entry; that's expected, not an error.
					if let Some(mut producer) = producers.remove(&path) {
						producer.abort(Error::Cancel).ok();
						stats_guards.remove(&abs);
					}
				}
			}
		}

		// The read loop ended because the publisher FINed: it has nothing (more) to announce
		// for this prefix (e.g. a publish-only peer). That's a clean completion of this
		// announce stream, not a session error, so finish our side and return Ok. Tearing
		// down only the announce stream is correct since no further progress can be made,
		// but we must not propagate an error that would kill the whole connection.
		stream.writer.finish().ok();
		Ok(())
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
		mut hops: crate::OriginList,
		producers: &mut HashMap<PathOwned, BroadcastProducer>,
	) -> Result<bool, Error> {
		// Drop announces that already passed through us. This connection is
		// a reflection, not a new path. Peers should be filtering via
		// AnnounceInterest.exclude_hop, but Lite03 peers can't, so this is
		// the authoritative cluster-loop check on the receiver.
		if hops.contains(&self.self_origin) {
			tracing::debug!(broadcast = %self.log_path(&path), "dropping reflected announce");
			return Ok(false);
		}

		// Lite03 carries its hop count as UNKNOWN placeholders rather than real
		// ids. Rewrite the first placeholder with this connection's origin so
		// the route is attributable to the upstream session, without changing
		// the hop count (shortest-path selection and the MAX_HOPS limit stay
		// accurate). Lite01/02 send no placeholders; they're covered below.
		if self.version_lacks_hops() {
			hops.replace_first(crate::Origin::UNKNOWN, self.session_origin);
		}

		// Guarantee at least one hop we control. A peer is meant to stamp its
		// own origin (Lite04+) or have one filled in above, but we don't trust
		// an empty chain: a peer that sends zero hops would otherwise be
		// indistinguishable from any other, so two empty-chain routes to the
		// same path would collide. Insert our session origin so every broadcast
		// stays attributable. The list is empty here, so this can't overflow.
		if hops.is_empty() {
			hops.push(self.session_origin)
				.expect("an empty hop chain always has room for one entry");
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

			let id = self.next_id.fetch_add(1, atomic::Ordering::Relaxed);
			let mut this = self.clone();

			let path = path.clone();
			let broadcast = broadcast.clone();
			web_async::spawn(async move {
				this.run_subscribe(id, path, broadcast, track).await;
				this.subscribes.lock().remove(&id);
			});
		}
	}

	async fn run_subscribe(&mut self, id: u64, path: PathOwned, broadcast: BroadcastDynamic, mut track: TrackProducer) {
		// Subscriber-side track stats; counters bump as frames/bytes/groups arrive.
		// Drop on subscription end records `subscriber.subscriptions_closed`. We use
		// subscriber_track to avoid double-counting broadcasts: the broadcast lifetime
		// is tracked separately by the announce loop's `stats_guards`.
		let abs = self.origin.as_ref().unwrap().absolute(&path);
		let track_stats = Arc::new(self.stats.broadcast(&abs).subscriber_track(&track.name));
		// The per-(session, broadcast) `broadcasts` sentinel is taken later, once
		// the upstream confirms with SUBSCRIBE_OK (see `run_track_stream`), so a
		// sub cancelled before then isn't counted as a feeding session.

		self.subscribes.lock().insert(
			id,
			TrackEntry {
				producer: track.clone(),
				stats: track_stats.clone(),
			},
		);

		let msg = lite::Subscribe {
			id,
			broadcast: path.as_path(),
			track: (&track.name).into(),
			priority: track.priority,
			ordered: true,
			max_latency: std::time::Duration::ZERO,
			start_group: None,
			end_group: None,
		};

		tracing::info!(id, broadcast = %self.log_path(&path), track = %track.name, "subscribe started");

		tokio::select! {
			_ = track.unused() => {
				tracing::info!(id, broadcast = %self.log_path(&path), track = %track.name, "subscribe cancelled");
				let _ = track.abort(Error::Cancel);
			}
			err = broadcast.closed() => {
				tracing::info!(id, broadcast = %self.log_path(&path), track = %track.name, "broadcast closed");
				let _ = track.abort(err);
			}
			res = self.run_track(msg) => match res {
				Ok(()) => {
					tracing::info!(id, broadcast = %self.log_path(&path), track = %track.name, "subscribe complete");
					let _ = track.finish();
				}
				Err(err) => {
					tracing::warn!(id, broadcast = %self.log_path(&path), track = %track.name, %err, "subscribe error");
					let _ = track.abort(err);
				}
			},
		}
	}

	async fn run_track(&mut self, msg: lite::Subscribe<'_>) -> Result<(), Error> {
		let mut stream = Stream::open(&self.session, self.version).await?;
		stream.writer.encode(&lite::ControlType::Subscribe).await?;

		if let Err(err) = self.run_track_stream(&mut stream, msg).await {
			stream.writer.abort(&err);
			return Err(err);
		}

		stream.writer.finish()?;
		stream.writer.closed().await
	}

	async fn run_track_stream(
		&mut self,
		stream: &mut Stream<S, Version>,
		msg: lite::Subscribe<'_>,
	) -> Result<(), Error> {
		stream.writer.encode(&msg).await?;

		// The first response MUST be a SUBSCRIBE_OK.
		let resp: lite::SubscribeResponse = stream.reader.decode().await?;
		let lite::SubscribeResponse::Ok(_info) = resp else {
			return Err(Error::ProtocolViolation);
		};

		// Upstream confirmed the subscription, so this session is now actively
		// feeding the broadcast: take the `broadcasts` sentinel. It drops with
		// this fn (subscription end / cancel), releasing `broadcasts_closed`.
		let abs = self.origin.as_ref().unwrap().absolute(&msg.broadcast);
		let _broadcast_sub = self.broadcasts.subscribe(&abs);

		// TODO handle additional SUBSCRIBE_OK and SUBSCRIBE_DROP messages.
		stream.reader.closed().await?;

		Ok(())
	}

	pub async fn recv_group(&mut self, stream: &mut Reader<S::RecvStream, Version>) -> Result<(), Error> {
		let hdr: lite::Group = stream.decode().await?;

		let (mut group, track, track_stats) = {
			let mut subs = self.subscribes.lock();
			let entry = subs.get_mut(&hdr.subscribe).ok_or(Error::Cancel)?;

			let group_info = Group { sequence: hdr.sequence };
			let group = entry.producer.create_group(group_info)?;
			(group, entry.producer.clone(), entry.stats.clone())
		};

		// Bump groups counter for this incoming group on the subscriber side.
		track_stats.group();

		let res = tokio::select! {
			err = track.closed() => Err(err),
			err = group.closed() => Err(err),
			res = self.run_group(stream, group.clone(), track_stats.clone()) => res,
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
	) -> Result<(), Error> {
		while let Some(size) = stream.decode_maybe::<u64>().await? {
			if size > MAX_FRAME_SIZE {
				return Err(Error::FrameTooLarge);
			}
			let mut frame = group.create_frame(Frame { size })?;
			track_stats.frame();

			if let Err(err) = self.run_frame(stream, &mut frame, &track_stats).await {
				let _ = frame.abort(err.clone());
				return Err(err);
			}

			frame.finish()?;
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

	/// True for versions that don't carry a real hop list on the wire, so the
	/// received chain is empty (Lite01/02) or anonymous placeholders (Lite03).
	fn version_lacks_hops(&self) -> bool {
		matches!(self.version, Version::Lite01 | Version::Lite02 | Version::Lite03)
	}
}
