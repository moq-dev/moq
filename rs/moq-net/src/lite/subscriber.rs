use std::{
	collections::{HashMap, hash_map::Entry},
	sync::{Arc, atomic},
	task::Poll,
	time::Duration,
};

use futures::{StreamExt, stream::FuturesUnordered};

use crate::{
	AsPath, BandwidthProducer, Broadcast, BroadcastDynamic, Compression, Error, Frame, FrameProducer, Group,
	GroupProducer, MAX_FRAME_SIZE, OriginProducer, Path, PathOwned, StatsHandle, SubscriberStats, SubscriberTrack,
	Timescale, Timestamp, Track, TrackProducer, TrackRequest,
	coding::{Reader, Stream},
	lite,
	model::BroadcastProducer,
};

use super::{ConnectingProducer, Version};

use web_async::Lock;

/// Keep an upstream subscription alive briefly after the last consumer leaves,
/// so a returning subscriber reuses the same TrackProducer instead of forcing a
/// fresh fetch (and the publisher re-serving the latest cached group).
const LINGER_TIMEOUT: Duration = Duration::from_secs(5);

pub(super) struct SubscriberConfig<S: web_transport_trait::Session> {
	pub session: S,
	/// The origin into which remote broadcasts are inserted.
	pub origin: OriginProducer,
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

	origin: OriginProducer,
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
	/// The SUBSCRIBE_OK for this subscription. `None` until it arrives; group
	/// streams block on it before decoding any frame, since a group can race
	/// ahead of SUBSCRIBE_OK on its own QUIC stream.
	subscribe_ok: kio::Consumer<Option<lite::SubscribeOk>>,
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
		// loop detection.
		let self_origin = *config.origin;
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

	/// `connecting` is the connection-progress producer for this session (None for
	/// versions with no initial-set boundary). It is threaded through the announce path
	/// rather than stored on `Subscriber`: the struct is cloned for several long-lived
	/// tasks (`bw`, `run_uni`), and any clone retaining a producer would keep the channel
	/// open and hang `connect()`.
	pub async fn run(self, connecting: Option<ConnectingProducer>) -> Result<(), Error> {
		let bw = self.clone();
		tokio::select! {
			Err(err) = self.clone().run_announce(connecting) => Err(err),
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

	async fn run_announce(self, connecting: Option<ConnectingProducer>) -> Result<(), Error> {
		let prefixes: Vec<PathOwned> = self.origin.allowed().map(|p| p.to_owned()).collect();

		let mut tasks = FuturesUnordered::new();
		for prefix in prefixes {
			tasks.push(self.clone().run_announce_prefix(prefix, connecting.clone()));
		}

		// Each prefix holds its own producer clone; drop ours so the channel closes (and
		// connect() unblocks) once the last prefix finishes its initial set. With no
		// prefixes, this is the only producer, so the session is connected now.
		drop(connecting);

		while let Some(result) = tasks.next().await {
			result?;
		}

		Ok(())
	}

	async fn run_announce_prefix(
		mut self,
		prefix: PathOwned,
		mut connecting: Option<ConnectingProducer>,
	) -> Result<(), Error> {
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

		// Lite05+: the publisher reports its own origin id (which we stamp onto every
		// received Announce's hop chain, since it no longer does so itself) plus the
		// count of initial active announces that follow immediately.
		let (responder_origin, initial_count) = match self.version {
			Version::Lite05Wip => {
				let ok: lite::AnnounceOk = stream.reader.decode().await?;
				(Some(ok.origin), ok.active)
			}
			_ => (None, 0),
		};

		let mut producers = HashMap::new();
		// Per-broadcast subscriber-side stats guards. Dropping the guard records
		// `subscriber.broadcasts_closed`. We only insert a guard when start_announce
		// actually accepted the announcement (it may drop reflected loops), so the
		// guard set tracks `producers` exactly.
		let mut stats_guards: HashMap<PathOwned, SubscriberStats> = HashMap::new();

		// Stats keys are absolute paths (matching the publisher side) so the
		// fanned-out level keys line up with the absolute broadcast paths a
		// dashboard sees on the origin.

		// `connecting` is a local (a param), not a `self` field, so the `self.clone()` that
		// start_announce uses to spawn long-lived broadcast tasks doesn't carry the producer
		// (which would keep the channel open for the broadcast's lifetime). Dropping it marks
		// this prefix connected; on an early error it drops via scope exit, so a failed prefix
		// can't hang connect().

		match self.version {
			Version::Lite01 | Version::Lite02 => {
				let msg: lite::AnnounceInit = stream.reader.decode().await?;
				for suffix in msg.suffixes {
					let path = prefix.join(&suffix);
					let abs = self.origin.absolute(&path).to_owned();
					// Lite01/02 don't carry hop information; the broadcast starts with an empty chain.
					if self.start_announce(path.clone(), crate::OriginList::new(), responder_origin, &mut producers)? {
						stats_guards.insert(abs.clone(), self.stats.broadcast(&abs).subscriber());
					}
				}
			}
			_ => {
				// Lite03+: no AnnounceInit, initial state comes via Announce messages.
			}
		}

		// Release the producer once this prefix's initial set is in. Lite01/02 delivered it
		// via AnnounceInit (consumed just above); Lite05 delivers `initial_count`
		// Announce::Active counted in the loop below; Lite03/04 have no boundary (already None).
		let mut initial_remaining = match self.version {
			Version::Lite01 | Version::Lite02 => {
				connecting.take();
				0
			}
			Version::Lite05Wip => {
				if initial_count == 0 {
					connecting.take();
				}
				initial_count
			}
			_ => {
				connecting.take();
				0
			}
		};

		while let Some(announce) = stream.reader.decode_maybe::<lite::Announce>().await? {
			match announce {
				lite::Announce::Active { suffix, hops } => {
					let path = prefix.join(&suffix);
					let abs = self.origin.absolute(&path).to_owned();
					if lite::restart_supported(self.version) && producers.contains_key(&path) {
						// lite-05+ only: a duplicate ANNOUNCE for an already-announced path is a RESTART;
						// atomically replace the broadcast. Older versions fall through to start_announce,
						// which rejects the duplicate (Error::Duplicate).
						if self.restart_announce(path.clone(), hops, responder_origin, &mut producers)? {
							// Continuity: keep the existing stats guard if present.
							stats_guards
								.entry(abs.clone())
								.or_insert_with(|| self.stats.broadcast(&abs).subscriber());
						} else {
							stats_guards.remove(&abs);
						}
					} else if self.start_announce(path.clone(), hops, responder_origin, &mut producers)? {
						stats_guards.insert(abs.clone(), self.stats.broadcast(&abs).subscriber());
					}
					// The first `initial_count` Active messages are the initial set; once
					// they're all in, drop our producer to mark this prefix connected.
					if initial_remaining > 0 {
						initial_remaining -= 1;
						if initial_remaining == 0 {
							connecting.take();
						}
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
						let abs = self.origin.absolute(&path).to_owned();
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
	/// Loops forever: wait for a consumer, race the probe stream against
	/// the consumer leaving, then loop back. Probe is best-effort, so stream
	/// errors are logged but never tear down the session.
	async fn run_recv_bandwidth(self) -> Result<(), Error> {
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
		// Lite05+: the announce sender's origin id (from AnnounceOk). The sender no
		// longer stamps itself onto the chain, so we append it here to reconstruct
		// the full `[src...sender]` chain Lite04 stored. None for older versions,
		// where the sender already appended itself.
		responder_origin: Option<crate::Origin>,
		producers: &mut HashMap<PathOwned, BroadcastProducer>,
	) -> Result<bool, Error> {
		if let Some(responder) = responder_origin {
			// If the chain is already full, drop the announce — the same decision
			// the Lite04 sender makes at its push site.
			if hops.push(responder).is_err() {
				tracing::warn!(
					broadcast = %self.log_path(&path),
					"dropping announce; hop chain at MAX_HOPS (possible loop)",
				);
				return Ok(false);
			}
		}

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
		// can call consume_track() before dynamic is incremented, getting NotFound.
		let dynamic = broadcast.dynamic();

		// Run the broadcast in the background until all consumers are dropped.
		self.origin.publish_broadcast(path.clone(), broadcast.consume());

		web_async::spawn(self.clone().run_broadcast(path, dynamic));

		Ok(true)
	}

	/// Handle a RESTART (a duplicate ANNOUNCE): atomically replace the broadcast at `path`.
	///
	/// Publishing the replacement before retiring the old producer lets the origin demote the old
	/// broadcast to a backup and emit a single restart downstream, rather than an
	/// unannounce/announce pair with a visible gap. Returns `Ok(false)` if the replacement was a
	/// reflected loop (the broadcast is now gone), `Ok(true)` otherwise.
	fn restart_announce(
		&mut self,
		path: PathOwned,
		mut hops: crate::OriginList,
		// Lite05+: the announce sender's origin id (from AnnounceOk), appended here to
		// rebuild the full chain since the sender no longer stamps itself. None for older
		// versions. See `start_announce`.
		responder_origin: Option<crate::Origin>,
		producers: &mut HashMap<PathOwned, BroadcastProducer>,
	) -> Result<bool, Error> {
		// Reflected loop (or a full chain): the replacement can't be used here. Retire the broadcast.
		let reflected = match responder_origin {
			Some(responder) => hops.push(responder).is_err() || hops.contains(&self.self_origin),
			None => hops.contains(&self.self_origin),
		};
		if reflected {
			tracing::debug!(broadcast = %self.log_path(&path), "dropping reflected restart");
			if let Some(mut old) = producers.remove(&path) {
				old.abort(Error::Cancel).ok();
			}
			return Ok(false);
		}

		tracing::debug!(broadcast = %self.log_path(&path), hops = hops.len(), "restart");

		let broadcast = Broadcast { hops }.produce();
		let dynamic = broadcast.dynamic();

		// Publish the replacement first so the origin restarts atomically; the old broadcast is
		// demoted to a backup and dropped silently when we abort it below.
		self.origin.publish_broadcast(path.clone(), broadcast.consume());

		let old = producers.insert(path.clone(), broadcast.clone());
		web_async::spawn(self.clone().run_broadcast(path.clone(), dynamic));

		if let Some(mut old) = old {
			old.abort(Error::Cancel).ok();
		}

		Ok(true)
	}

	async fn run_broadcast(self, path: PathOwned, mut broadcast: BroadcastDynamic) {
		// Actually start serving subscriptions.
		loop {
			// Keep serving requests until there are no more consumers.
			// This way we'll clean up the task when the broadcast is no longer needed.
			let request = tokio::select! {
				request = broadcast.requested_track() => match request {
					Ok(request) => request,
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
				this.run_subscribe(path, broadcast, request).await;
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
	async fn run_subscribe(&mut self, path: PathOwned, broadcast: BroadcastDynamic, request: TrackRequest) {
		// Subscriber-side track stats; counters bump as frames/bytes/groups arrive.
		// Drop on subscription end records `subscriber.subscriptions_closed`. We use
		// subscriber_track to avoid double-counting broadcasts: the broadcast lifetime
		// is tracked separately by the announce loop's `stats_guards`.
		let name = request.name().to_string();
		let abs = self.origin.absolute(&path);
		let track_stats = Arc::new(self.stats.broadcast(&abs).subscriber_track(&name));

		let id = self.next_id.fetch_add(1, atomic::Ordering::Relaxed);

		// Forward the aggregate of every downstream subscriber's preferences upstream.
		let subscription = request.subscription().clone();
		let msg = lite::Subscribe {
			id,
			broadcast: path.as_path(),
			track: (&name).into(),
			priority: subscription.priority,
			ordered: subscription.ordered,
			max_latency: subscription.stale,
			start_group: subscription.group_start,
			end_group: subscription.group_end,
		};

		tracing::info!(id, broadcast = %self.log_path(&path), track = %name, "subscribe started");

		let result = self
			.run_subscribe_session(id, &name, request, track_stats, &broadcast, msg)
			.await;

		self.subscribes.lock().remove(&id);

		match result {
			SessionOutcome::Complete => {
				tracing::info!(broadcast = %self.log_path(&path), track = %name, "subscribe complete");
			}
			SessionOutcome::Cancelled => {
				tracing::info!(broadcast = %self.log_path(&path), track = %name, "subscribe cancelled");
			}
			SessionOutcome::BroadcastClosed(err) => {
				tracing::info!(broadcast = %self.log_path(&path), track = %name, %err, "broadcast closed");
			}
			SessionOutcome::Error(err) => {
				tracing::warn!(broadcast = %self.log_path(&path), track = %name, %err, "subscribe error");
			}
		}
	}

	/// Open the upstream subscribe stream, wait for SUBSCRIBE_OK, then accept the
	/// pending request (unblocking the downstream subscriber) and run the linger
	/// lifecycle. The producer is created only after SUBSCRIBE_OK, so a downstream
	/// a downstream `subscribe` resolves exactly when the upstream confirms.
	async fn run_subscribe_session(
		&self,
		id: u64,
		name: &str,
		request: TrackRequest,
		track_stats: Arc<SubscriberTrack>,
		broadcast: &BroadcastDynamic,
		msg: lite::Subscribe<'_>,
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
			Err(err) => {
				request.deny(err.clone());
				return SessionOutcome::Error(err);
			}
		};

		if let Err(err) = stream.writer.encode(&lite::ControlType::Subscribe).await {
			request.deny(err.clone());
			return SessionOutcome::Error(err);
		}

		if let Err(err) = stream.writer.encode(&msg).await {
			stream.writer.abort(&err);
			request.deny(err.clone());
			return SessionOutcome::Error(err);
		}

		// The first response MUST be a SUBSCRIBE_OK. Bail if the broadcast dies first.
		let resp = tokio::select! {
			err = broadcast.closed() => {
				request.deny(err.clone());
				return SessionOutcome::BroadcastClosed(err);
			}
			resp = stream.reader.decode::<lite::SubscribeResponse>() => match resp {
				Ok(r) => r,
				Err(err) => {
					stream.writer.abort(&err);
					request.deny(err.clone());
					return SessionOutcome::Error(err);
				}
			}
		};
		let lite::SubscribeResponse::Ok(info) = resp else {
			let err = Error::ProtocolViolation;
			stream.writer.abort(&err);
			request.deny(err.clone());
			return SessionOutcome::Error(err);
		};

		// The publisher accepted: create the producer (unblocking the downstream
		// subscriber) and start routing incoming groups to it. SUBSCRIBE_OK is known
		// now, so the group streams never have to wait; they still read it through a
		// kio channel (a group's QUIC stream can otherwise race ahead of SUBSCRIBE_OK).
		//
		// Stamp the negotiated timescale onto the local Track so groups inherit
		// it and downstream consumers (including this subscriber's frame decode
		// path) can validate per-frame timestamps at the model layer.
		let mut local_info = Track::new(name);
		local_info.timescale = info.timescale;
		let mut track = match request.accept(local_info) {
			Ok(track) => track,
			Err(err) => {
				stream.writer.abort(&err);
				return SessionOutcome::Error(err);
			}
		};
		let subscribe_ok = kio::Producer::new(Some(info)).consume();
		self.subscribes.lock().insert(
			id,
			TrackEntry {
				producer: track.clone(),
				stats: track_stats,
				subscribe_ok,
			},
		);

		// Lifecycle loop: serve → linger → resume → serve → ... → FIN.
		let outcome = 'lifecycle: loop {
			// Phase 1 — serving. Wait for the last consumer to drop (enter linger),
			// the broadcast to die, or the upstream to close the stream.
			tokio::select! {
				_ = track.unused() => {}
				err = broadcast.closed() => break 'lifecycle SessionOutcome::BroadcastClosed(err),
				res = stream.reader.closed() => match res {
					Ok(()) => {
						let _ = stream.writer.finish();
						break 'lifecycle SessionOutcome::Complete;
					}
					Err(err) => break 'lifecycle SessionOutcome::Error(err),
				},
			}

			// No linger on Lite01/02: FIN and report cancellation.
			if !supports_linger {
				let _ = stream.writer.finish();
				break 'lifecycle SessionOutcome::Cancelled;
			}

			// Phase 2 — linger. Cap the publisher's serving cursor at the latest
			// group we've cached and drop priority to 0; the publisher holds any
			// group beyond the cap until we resume or FIN. `unwrap_or(0)` handles
			// the corner case where we subscribed but haven't received a group yet.
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
				break 'lifecycle SessionOutcome::Error(err);
			}

			// Race during the linger window: publisher closes (Complete), broadcast
			// dies (BroadcastClosed), timeout (Cancelled), or a new consumer arrives
			// (uncap and re-enter Phase 1).
			let resume = tokio::select! {
				err = broadcast.closed() => break 'lifecycle SessionOutcome::BroadcastClosed(err),
				res = stream.reader.closed() => match res {
					Ok(()) => break 'lifecycle SessionOutcome::Complete,
					Err(err) => break 'lifecycle SessionOutcome::Error(err),
				},
				_ = tokio::time::sleep(LINGER_TIMEOUT) => {
					let _ = stream.writer.finish();
					break 'lifecycle SessionOutcome::Cancelled;
				}
				res = track.used() => res,
			};
			if let Err(err) = resume {
				break 'lifecycle SessionOutcome::Error(err);
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
				break 'lifecycle SessionOutcome::Error(err);
			}
			// Loop back to Phase 1.
		};

		// Apply the outcome to the producer that downstream consumers read from.
		match &outcome {
			SessionOutcome::Complete => {
				let _ = track.finish();
			}
			SessionOutcome::Cancelled => {
				let _ = track.abort(Error::Cancel);
			}
			SessionOutcome::BroadcastClosed(err) | SessionOutcome::Error(err) => {
				let _ = track.abort(err.clone());
			}
		}

		outcome
	}

	pub async fn recv_group(&mut self, stream: &mut Reader<S::RecvStream, Version>) -> Result<(), Error> {
		let hdr: lite::Group = stream.decode().await?;

		let (mut group, track, track_stats, subscribe_ok) = {
			let mut subs = self.subscribes.lock();
			let entry = subs.get_mut(&hdr.subscribe).ok_or(Error::Cancel)?;

			let group_info = Group { sequence: hdr.sequence };
			let group = entry.producer.create_group(group_info)?;
			(
				group,
				entry.producer.clone(),
				entry.stats.clone(),
				entry.subscribe_ok.clone(),
			)
		};

		// Bump groups counter for this incoming group on the subscriber side.
		track_stats.group();

		// Block until SUBSCRIBE_OK arrives. The group's QUIC stream can arrive
		// before SUBSCRIBE_OK lands on the subscribe stream, so we can't decode
		// frames until this resolves. A closed channel means the subscription
		// ended before SUBSCRIBE_OK, so treat it as cancelled.
		//
		// Map the closed `Ref` to `None` inside the poll closure (rather than using
		// `Consumer::wait`) so the `!Send` guard never enters this spawned future.
		let (compression, timescale) = kio::wait(|waiter| {
			let poll = subscribe_ok.poll(waiter, |ok| match &**ok {
				Some(ok) => Poll::Ready((ok.compression, ok.timescale)),
				None => Poll::Pending,
			});
			match poll {
				Poll::Ready(Ok(pair)) => Poll::Ready(Some(pair)),
				Poll::Ready(Err(_closed)) => Poll::Ready(None),
				Poll::Pending => Poll::Pending,
			}
		})
		.await
		.ok_or(Error::Cancel)?;

		let res = tokio::select! {
			err = track.closed() => Err(err),
			err = group.closed() => Err(err),
			res = self.run_group(stream, group.clone(), track_stats.clone(), compression, timescale) => res,
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
		timescale: Option<Timescale>,
	) -> Result<(), Error> {
		// Previous frame's raw timestamp value (in `timescale` units), for the
		// zigzag-delta decode when timestamps are negotiated. The first frame's
		// delta is its absolute value (prev_ts = 0 implicitly).
		let mut prev_ts: u64 = 0;

		loop {
			let timestamp = if let Some(scale) = timescale {
				// Publisher advertised a timescale, so every frame on this stream
				// is prefixed with a zigzag-delta timestamp varint.
				let Some(zz) = stream.decode_maybe::<crate::coding::VarInt>().await? else {
					break;
				};
				let delta = zz.to_zigzag();
				let next: u64 = (prev_ts as i128 + delta as i128)
					.try_into()
					.map_err(|_| Error::BoundsExceeded(crate::coding::BoundsExceeded))?;
				prev_ts = next;
				Some(Timestamp::new(next, scale).map_err(|_| Error::BoundsExceeded(crate::coding::BoundsExceeded))?)
			} else {
				None
			};

			let Some(size) = stream.decode_maybe::<u64>().await? else {
				break;
			};
			if size > MAX_FRAME_SIZE {
				return Err(Error::FrameTooLarge);
			}

			match compression {
				Compression::None => {
					let mut frame = group.create_frame(Frame { size, timestamp })?;
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
						timestamp,
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
		self.origin.root().join(path)
	}
}
