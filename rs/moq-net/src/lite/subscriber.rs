use std::{
	collections::HashMap,
	pin::Pin,
	sync::{Arc, atomic},
	task::Poll,
	time::Duration,
};

use futures::{StreamExt, stream::FuturesUnordered};

use crate::util::{MaybeBoxedExt, MaybeSendBox};

use crate::{
	AsPath, BandwidthProducer, BroadcastDynamic, BroadcastInfo, Error, FrameInfo, FrameProducer, GroupInfo,
	GroupProducer, GroupRequest, OriginProducer, OriginPublish, Path, PathOwned, StatsHandle, SubscriberStats,
	SubscriberTrack, Subscription, Timescale, Timestamp, TrackInfo, TrackProducer, TrackRequest,
	coding::{Reader, Stream},
	lite,
};

use super::{ConnectingProducer, Version};

use web_async::Lock;

/// Keep an upstream subscription alive briefly after the track goes idle (no
/// subscriber, no fetch, no consumers), so a returning consumer reuses the same
/// TrackProducer instead of forcing a fresh fetch (and the publisher re-serving
/// the latest cached group).
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
	/// Shared slot for the peer's SETUP (lite-05+). Written when the peer's Setup
	/// stream is read; the probe stream waits on it before opening.
	pub peer_setup: super::PeerSetup,
}

#[derive(Clone)]
pub(super) struct Subscriber<S: web_transport_trait::Session> {
	session: S,

	origin: OriginProducer,
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
	/// The peer's advertised SETUP (lite-05+), set when its Setup stream is read.
	peer_setup: super::PeerSetup,
}

#[derive(Clone)]
struct TrackEntry {
	producer: TrackProducer,
	stats: Arc<SubscriberTrack>,
	/// Timestamp scale from this track's TRACK_INFO, known before the SUBSCRIBE is
	/// even opened, so group streams decode frames without blocking.
	timescale: Option<Timescale>,
}

impl<S: web_transport_trait::Session> Subscriber<S> {
	pub fn new(config: SubscriberConfig<S>) -> Self {
		// Identity for incoming-hop loop detection. Derived from the local
		// origin we publish into so it matches the relay identity across
		// every session sharing that origin, required for cross-session
		// loop detection.
		let self_origin = *config.origin;
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
			peer_setup: config.peer_setup,
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
			lite::DataType::Setup => self.recv_setup(&mut stream).await,
		};

		if let Err(err) = res {
			stream.abort(&err);
		}

		Ok(())
	}

	/// Read the peer's single SETUP message off its Setup Stream and record it, so
	/// capability-gated streams (PROBE) can consult it. lite-05+ only.
	async fn recv_setup(&self, stream: &mut Reader<S::RecvStream, Version>) -> Result<(), Error> {
		if !self.version.has_setup_stream() {
			return Err(Error::UnexpectedStream);
		}
		let setup = stream.decode::<lite::Setup>().await?;
		tracing::debug!(?setup, "received peer setup");
		self.peer_setup.set(setup);
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
		let msg = lite::AnnounceRequest {
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
					// Lite01/02 don't carry hop information; the broadcast starts with
					// an empty chain.
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

		while let Some(announce) = stream.reader.decode_maybe::<lite::AnnounceBroadcast>().await? {
			match announce {
				lite::AnnounceBroadcast::Active { suffix, hops } => {
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
				lite::AnnounceBroadcast::Ended { suffix, .. } => {
					let path = prefix.join(&suffix);
					tracing::debug!(broadcast = %self.log_path(&path), "unannounced");

					// The matching Active may have been silently dropped by
					// start_announce as a reflected loop, in which case
					// `producers` has no entry; that's expected, not an error.
					// Dropping the entry drops its OriginPublish guard, which unannounces.
					if producers.remove(&path).is_some() {
						let abs = self.origin.absolute(&path).to_owned();
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
	/// Loops forever: wait for a consumer, race the probe stream against
	/// the consumer leaving, then loop back. Probe is best-effort, so stream
	/// errors are logged but never tear down the session.
	async fn run_recv_bandwidth(self) -> Result<(), Error> {
		let Some(bandwidth) = &self.recv_bandwidth else {
			return Ok(());
		};

		// lite-05+ negotiates probing: only open a PROBE stream if the peer advertised it
		// (Report or higher) in its SETUP. Older versions have no SETUP, so probe is always
		// available there.
		if self.version.has_setup_stream() && self.peer_setup.probe_level().await < lite::ProbeLevel::Report {
			tracing::debug!("peer does not support probing; skipping probe stream");
			return Ok(());
		}

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
		producers: &mut HashMap<PathOwned, OriginPublish>,
	) -> Result<bool, Error> {
		if let Some(responder) = responder_origin {
			// If the chain is already full, drop the announce. This is the same decision
			// the Lite04 sender makes at its push site.
			if hops.push(responder).is_err() {
				tracing::warn!(
					broadcast = %self.log_path(&path),
					"dropping announce; hop chain at MAX_HOPS (possible loop)",
				);
				return Ok(false);
			}
		}

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
		// own origin (Lite04), be reconstructed from the responder above (Lite05+),
		// or filled in above (Lite03), but we don't trust an empty chain: a peer
		// that sends zero hops would otherwise be indistinguishable from any other,
		// so two empty-chain routes to the same path would collide. Insert our
		// session origin so every broadcast stays attributable. The list is empty
		// here, so this can't overflow.
		if hops.is_empty() {
			hops.push(self.session_origin)
				.expect("an empty hop chain always has room for one entry");
		}

		// Make sure the peer doesn't double announce.
		if producers.contains_key(&path) {
			return Err(Error::Duplicate);
		}

		tracing::debug!(broadcast = %self.log_path(&path), hops = hops.len(), "announce");

		let broadcast = BroadcastInfo { hops }.produce();

		// Create the dynamic handler BEFORE publishing, so that consumers
		// see dynamic >= 1 immediately when they receive the announcement.
		// Otherwise there's a race on multi-threaded runtimes where a consumer
		// can call consume_track() before dynamic is incremented, getting NotFound.
		let dynamic = broadcast.dynamic();

		// Publish into the origin. An error means the path is outside our scope, so don't announce
		// or spawn a server for it. Reflections are already filtered above.
		let Ok(publish) = self.origin.publish_broadcast(path.clone(), &broadcast) else {
			return Ok(false);
		};

		producers.insert(path.clone(), publish);

		// Run the broadcast in the background until all consumers are dropped.
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
		producers: &mut HashMap<PathOwned, OriginPublish>,
	) -> Result<bool, Error> {
		// Reflected loop (or a full chain): the replacement can't be used here. Retire the broadcast.
		let reflected = match responder_origin {
			Some(responder) => hops.push(responder).is_err() || hops.contains(&self.self_origin),
			None => hops.contains(&self.self_origin),
		};
		if reflected {
			tracing::debug!(broadcast = %self.log_path(&path), "dropping reflected restart");
			// Dropping the entry drops its guard, unannouncing the broadcast.
			producers.remove(&path);
			return Ok(false);
		}

		tracing::debug!(broadcast = %self.log_path(&path), hops = hops.len(), "restart");

		let broadcast = BroadcastInfo { hops }.produce();
		let dynamic = broadcast.dynamic();

		// Publish the replacement first so the origin restarts atomically; the old broadcast is
		// demoted to a backup and removed silently when we drop its guard below.
		let Ok(publish) = self.origin.publish_broadcast(path.clone(), &broadcast) else {
			// Origin rejected the replacement; retire the existing broadcast.
			producers.remove(&path);
			return Ok(false);
		};

		let old = producers.insert(path.clone(), publish);
		web_async::spawn(self.clone().run_broadcast(path.clone(), dynamic));

		// Drop the replaced broadcast's guard last, unannouncing it now that the replacement is live.
		drop(old);

		Ok(true)
	}

	async fn run_broadcast(self, path: PathOwned, mut broadcast: BroadcastDynamic) {
		// Serve track requests until every consumer of the broadcast is gone.
		loop {
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

			let name = request.name().to_string();
			let abs = self.origin.absolute(&path);
			// Subscriber-side track stats; counters bump as frames/bytes/groups arrive.
			// subscriber_track avoids double-counting broadcasts: the broadcast lifetime
			// is tracked separately by the announce loop's `stats_guards`.
			let track_stats = Arc::new(self.stats.broadcast(&abs).subscriber_track(&name));

			let serve = TrackServe {
				subscriber: self.clone(),
				path: path.clone(),
				broadcast: broadcast.clone(),
				track_stats,
				name,
			};

			// One task per track serves its lone subscription and any number of
			// fetches concurrently, then lingers before tearing the upstream down.
			web_async::spawn(serve.run(request));
		}
	}

	pub async fn recv_group(&mut self, stream: &mut Reader<S::RecvStream, Version>) -> Result<(), Error> {
		let hdr: lite::Group = stream.decode().await?;

		let (mut group, track, track_stats, timescale) = {
			let mut subs = self.subscribes.lock();
			let entry = subs.get_mut(&hdr.subscribe).ok_or(Error::Cancel)?;

			let group_info = GroupInfo { sequence: hdr.sequence };
			let group = entry.producer.create_group(group_info)?;
			(group, entry.producer.clone(), entry.stats.clone(), entry.timescale)
		};

		// Bump groups counter for this incoming group on the subscriber side.
		track_stats.group();

		// The timescale came from TRACK_INFO (read before this subscription was even
		// registered), so frames decode immediately. No SUBSCRIBE_OK to wait on.

		let res = tokio::select! {
			err = track.closed() => Err(err),
			err = group.closed() => Err(err),
			res = self.run_group(stream, group.clone(), track_stats.clone(), timescale) => res,
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
		timescale: Option<Timescale>,
	) -> Result<(), Error> {
		// Previous frame's raw timestamp value (in `timescale` units), for the
		// zigzag-delta decode when timestamps are negotiated. The first frame's
		// delta is absolute (prev = 0 implicitly).
		let mut prev_ts: u64 = 0;

		loop {
			let timestamp = if let Some(scale) = timescale {
				// Publisher advertised a timescale, so every frame on this stream is
				// prefixed with a zigzag-delta timestamp. The timestamp delta doubles
				// as the per-frame sentinel: stream end here means the group has no
				// more frames.
				let Some(zz) = stream.decode_maybe::<crate::coding::VarInt>().await? else {
					break;
				};
				let next: u64 = (prev_ts as i128 + zz.to_zigzag() as i128)
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

			// `create_frame` is the allocation chokepoint and rejects an oversized
			// `size` before allocating, so no pre-check is needed. No wire timestamp
			// (pre-lite-05) means wall-clock at receive.
			let mut frame = match timestamp {
				Some(ts) => group.create_frame(FrameInfo { size, timestamp: ts })?,
				None => group.create_frame_now(size)?,
			};
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
		// read_buf writes QUIC stream bytes directly into the frame. No
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

	/// True for versions that don't carry a real hop list on the wire, so the
	/// received chain is empty (Lite01/02) or anonymous placeholders (Lite03).
	fn version_lacks_hops(&self) -> bool {
		matches!(self.version, Version::Lite01 | Version::Lite02 | Version::Lite03)
	}
}

/// The producer side of one track. On Lite05+ it's `Active` from the start (the
/// TRACK stream resolves the properties up front); on older drafts it stays
/// `Pending` until the first SUBSCRIBE_OK promotes it. Both observe subscription
/// demand, so [`TrackServe`] drives them uniformly. Fetches are served separately
/// via the [`TrackDynamic`] handle.
enum Track {
	Pending(TrackRequest),
	Active(TrackProducer),
}

impl Track {
	fn poll_subscription_changed(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Subscription>, Error>> {
		match self {
			Track::Pending(request) => request.poll_subscription_changed(waiter).map(Ok),
			Track::Active(producer) => producer.poll_subscription_changed(waiter),
		}
	}

	fn poll_unused(&self, waiter: &kio::Waiter) -> Poll<()> {
		match self {
			Track::Pending(request) => request.poll_unused(waiter),
			Track::Active(producer) => producer.poll_unused(waiter),
		}
	}

	/// Latest cached sequence, used to cap the upstream while pausing. `None` before
	/// any group has arrived.
	fn latest(&self) -> Option<u64> {
		match self {
			Track::Active(producer) => producer.latest(),
			Track::Pending(_) => None,
		}
	}
}

/// The at-most-one live upstream subscription: its control stream plus the params
/// echoed in every SUBSCRIBE_UPDATE.
struct SubStream<S: web_transport_trait::Session> {
	stream: Stream<S, Version>,
	id: u64,
	/// Capped at the latest group and dropped to priority 0 because the last
	/// downstream subscriber left. The stream stays open during the linger window
	/// so a returning consumer resumes without a fresh SUBSCRIBE.
	paused: bool,
	/// Original SUBSCRIBE params, echoed in every SUBSCRIBE_UPDATE; refreshed as the
	/// downstream aggregate changes.
	ordered: bool,
	max_latency: Duration,
	start_group: Option<u64>,
	priority: u8,
	/// Per-(session, broadcast) viewer sentinel, held for the subscription's life.
	_broadcast_sub: crate::BroadcastSubscription,
}

enum Sub<S: web_transport_trait::Session> {
	None,
	Active(SubStream<S>),
}

impl<S: web_transport_trait::Session> Sub<S> {
	fn is_active(&self) -> bool {
		matches!(self, Sub::Active(_))
	}
}

/// One step for the [`TrackServe`] loop, produced by racing track demand, the
/// upstream stream, the broadcast, and the linger timer.
enum Event {
	/// A consumer fetched a past group.
	Fetch(GroupRequest),
	/// The downstream aggregate subscription changed (`None` once the last subscriber leaves).
	Subscription(Option<Subscription>),
	/// Nothing left to serve (no subscription, no fetch, no consumers): start the linger countdown.
	Idle,
	/// An in-flight fetch finished.
	FetchDone,
	/// The upstream subscribe stream closed: `Ok` is a clean FIN, `Err` a transport error.
	SubClosed(Result<(), Error>),
	/// The whole broadcast went away.
	BroadcastClosed(Error),
	/// The linger window elapsed with nobody returning.
	LingerExpired,
}

/// Serves one track for a relay: drives the single upstream subscription (opened
/// lazily on the first downstream subscriber, paused/resumed across consumer churn)
/// concurrently with any number of one-shot fetches, then lingers before tearing
/// the upstream down so a returning consumer reuses the cache.
#[derive(Clone)]
struct TrackServe<S: web_transport_trait::Session> {
	subscriber: Subscriber<S>,
	path: PathOwned,
	broadcast: BroadcastDynamic,
	track_stats: Arc<SubscriberTrack>,
	name: String,
}

impl<S: web_transport_trait::Session> TrackServe<S> {
	async fn run(self, request: TrackRequest) {
		// SUBSCRIBE_UPDATE (and thus pause/resume linger) only exists on Lite03+.
		// Older versions tear the upstream down as soon as the track goes idle.
		let supports_linger = !matches!(self.subscriber.version, Version::Lite01 | Version::Lite02);

		// Mark the track as fetch-capable up front (before accept), so a consumer's
		// cache-miss fetch waits to be served rather than failing fast. Held for the
		// whole task; dropping it stops fetch serving.
		let dynamic = request.dynamic();

		// Lite05+ learns the track's immutable properties once, up front, via a TRACK
		// stream, and accepts the request immediately so downstream subscribers resolve.
		// The timescale then flows into every SUBSCRIBE and FETCH without a per-response
		// header. Older drafts have no TRACK stream: the request stays pending until the
		// first SUBSCRIBE_OK supplies the properties (see `establish`).
		let (mut track, timescale) = if self.subscriber.version.has_timestamps() {
			match self.track_info().await {
				Ok(info) => {
					// Lite05 carries per-frame timestamps on the wire at this scale; `Some`
					// tells `run_group` to decode them (vs. wall-clock-stamping locally).
					let timescale = Some(info.timescale);
					(Some(Track::Active(request.accept(info))), timescale)
				}
				Err(err) => {
					tracing::warn!(broadcast = %self.subscriber.log_path(&self.path), track = %self.name, %err, "track info failed");
					request.reject(err);
					return;
				}
			}
		} else {
			(Some(Track::Pending(request)), None)
		};

		let mut sub = Sub::None;
		// True once the upstream subscription FIN'd: a later subscriber must not reopen
		// it (the track is finished). Replaces the old "is the track still Pending" gate,
		// which no longer holds now that Lite05 accepts up front.
		let mut completed = false;
		let mut fetches: FuturesUnordered<MaybeSendBox<'static, ()>> = FuturesUnordered::new();
		let mut linger: Option<Pin<Box<web_async::time::Sleep>>> = None;

		loop {
			// Once nothing is in flight, the `poll_unused` check below confirms no
			// consumers remain (it never fires while a subscriber or fetch holds one)
			// and yields `Idle` to start the linger countdown.
			let idle_eligible = linger.is_none() && fetches.is_empty();

			let event = tokio::select! {
				biased;

				// (1) Track demand: a fetch, a subscription change, or full idle. One
				// `kio::wait` so the borrows of `dynamic` and `track` are held together.
				event = kio::wait(|waiter| {
					let track = track.as_mut().expect("track present while serving");

					// A fetch is cheap and one-shot, so serve it ahead of subscription churn.
					match dynamic.poll_requested_group(waiter) {
						Poll::Ready(Ok(req)) => return Poll::Ready(Event::Fetch(req)),
						Poll::Ready(Err(err)) => return Poll::Ready(Event::BroadcastClosed(err)),
						Poll::Pending => {}
					}
					match track.poll_subscription_changed(waiter) {
						Poll::Ready(Ok(pref)) => return Poll::Ready(Event::Subscription(pref)),
						Poll::Ready(Err(err)) => return Poll::Ready(Event::BroadcastClosed(err)),
						Poll::Pending => {}
					}
					if idle_eligible && track.poll_unused(waiter).is_ready() {
						return Poll::Ready(Event::Idle);
					}
					Poll::Pending
				}) => event,

				// (2) An in-flight fetch completed.
				Some(()) = fetches.next(), if !fetches.is_empty() => Event::FetchDone,

				// (3) The upstream subscribe stream closed, or carried a START/END/DROP.
				res = async {
					match &mut sub {
						Sub::Active(active) => active.stream.reader.decode_maybe::<lite::SubscribeResponse>().await,
						Sub::None => std::future::pending().await,
					}
				}, if sub.is_active() => match res {
					// START/END/DROP resolve the range; we don't drive delivery off them
					// (the producer already orders groups), so log and keep reading.
					Ok(Some(msg)) => {
						tracing::debug!(track = %self.name, ?msg, "subscribe response");
						continue;
					}
					Ok(None) => Event::SubClosed(Ok(())),
					Err(err) => Event::SubClosed(Err(err)),
				},

				// (4) The whole broadcast went away on the publisher side.
				err = self.broadcast.closed() => Event::BroadcastClosed(err),

				// (5) The linger window elapsed.
				_ = async {
					match linger.as_mut() {
						Some(timer) => timer.as_mut().await,
						None => std::future::pending::<()>().await,
					}
				}, if linger.is_some() => Event::LingerExpired,
			};

			match event {
				Event::Fetch(req) => {
					linger = None;
					fetches.push(self.clone().serve_fetch(req, timescale).maybe_boxed());
				}
				Event::Subscription(pref) => {
					linger = None;
					if let Err(err) = self
						.handle_subscription(&mut track, &mut sub, pref, supports_linger, completed, timescale)
						.await
					{
						return self.finish_track(track.take().unwrap(), sub, err);
					}
				}
				Event::Idle => {
					if supports_linger {
						linger = Some(Box::pin(web_async::time::sleep(LINGER_TIMEOUT)));
					} else {
						// No SUBSCRIBE_UPDATE to pause with, so there's nothing to keep
						// open: tear down as soon as the last consumer leaves.
						tracing::info!(broadcast = %self.subscriber.log_path(&self.path), track = %self.name, "subscribe cancelled");
						return self.finish_track(track.take().unwrap(), sub, Error::Cancel);
					}
				}
				Event::FetchDone => {}
				Event::SubClosed(Ok(())) => {
					tracing::info!(broadcast = %self.subscriber.log_path(&self.path), track = %self.name, "subscribe complete");
					// Upstream FIN'd the live subscription. Finish the producer (no more
					// live groups), but keep the task alive: past groups can still be
					// fetched, and the linger countdown eventually stops us.
					completed = true;
					if let Sub::Active(active) = &mut sub {
						self.subscriber.subscribes.lock().remove(&active.id);
						let _ = active.stream.writer.finish();
					}
					if let Some(Track::Active(producer)) = track.as_mut() {
						let _ = producer.finish();
					}
					sub = Sub::None;
				}
				Event::SubClosed(Err(err)) => {
					tracing::warn!(broadcast = %self.subscriber.log_path(&self.path), track = %self.name, %err, "subscribe error");
					return self.finish_track(track.take().unwrap(), sub, err);
				}
				Event::BroadcastClosed(err) => {
					tracing::info!(broadcast = %self.subscriber.log_path(&self.path), track = %self.name, %err, "broadcast closed");
					return self.finish_track(track.take().unwrap(), sub, err);
				}
				Event::LingerExpired => {
					tracing::info!(broadcast = %self.subscriber.log_path(&self.path), track = %self.name, "subscribe cancelled");
					return self.finish_track(track.take().unwrap(), sub, Error::Cancel);
				}
			}
		}
	}

	/// Open a TRACK stream, read the single TRACK_INFO, and map it to the model's
	/// [`crate::TrackInfo`]. Lite05+ only. Bails if the broadcast dies meanwhile.
	async fn track_info(&self) -> Result<crate::TrackInfo, Error> {
		let mut stream = Stream::open(&self.subscriber.session, self.subscriber.version).await?;
		stream.writer.encode(&lite::ControlType::Track).await?;
		stream
			.writer
			.encode(&lite::Track {
				broadcast: self.path.as_path(),
				track: self.name.as_str().into(),
			})
			.await?;

		let info = tokio::select! {
			err = self.broadcast.closed() => return Err(err),
			info = stream.reader.decode::<lite::TrackInfo>() => info?,
		};
		// The publisher FINs after TRACK_INFO; FIN our side too and let the stream drop.
		let _ = stream.writer.finish();

		// Publisher Max Latency rides on the wire, so the local retention window
		// matches what the upstream advertises (relays re-serve with the same bound).
		let model = crate::TrackInfo {
			timescale: info.timescale,
			cache: info.cache,
			priority: info.priority,
			ordered: info.ordered,
		};
		Ok(model)
	}

	/// Apply a subscription-demand change: open the upstream SUBSCRIBE on the first
	/// subscriber, resume/update it while live, or pause it when the last leaves.
	#[allow(clippy::too_many_arguments)]
	async fn handle_subscription(
		&self,
		track: &mut Option<Track>,
		sub: &mut Sub<S>,
		pref: Option<Subscription>,
		supports_linger: bool,
		completed: bool,
		timescale: Option<Timescale>,
	) -> Result<(), Error> {
		match pref {
			Some(subscription) => match sub {
				Sub::None => {
					// Open an upstream SUBSCRIBE for the first subscriber, unless the
					// track already finished. On Lite05 the producer is `Active` from
					// the start, so gate on `completed`; on older drafts a `Pending`
					// track means it was never subscribed.
					let establish = match track.as_ref() {
						Some(Track::Pending(_)) => true,
						Some(Track::Active(_)) => self.subscriber.version.has_timestamps() && !completed,
						None => false,
					};
					if establish {
						self.establish(track, sub, subscription, timescale).await?;
					}
				}
				Sub::Active(active) if active.paused => {
					// A consumer returned during linger: resume by uncapping.
					active.paused = false;
					active.priority = subscription.priority;
					active.ordered = subscription.ordered;
					active.max_latency = subscription.stale;
					active.start_group = subscription.group_start;
					self.send_update(active, subscription.group_end).await?;
					tracing::info!(track = %self.name, "subscribe resumed");
				}
				Sub::Active(active) => {
					// Downstream preferences changed: forward them upstream as a
					// SUBSCRIBE_UPDATE (Lite03+ only; older peers can't carry one).
					active.priority = subscription.priority;
					active.ordered = subscription.ordered;
					active.max_latency = subscription.stale;
					active.start_group = subscription.group_start;
					if supports_linger {
						self.send_update(active, subscription.group_end).await?;
					}
				}
			},
			None => {
				// Last subscriber left: pause the upstream (cap at the latest cached
				// group, priority 0) but keep the stream open for the linger window.
				// Older versions have no SUBSCRIBE_UPDATE, so they skip the pause and
				// tear down on `Idle` instead.
				if supports_linger {
					if let Sub::Active(active) = sub {
						if !active.paused {
							active.paused = true;
							active.start_group = None;
							let cap = track.as_ref().and_then(Track::latest).unwrap_or(0);
							let update = lite::SubscribeUpdate {
								priority: 0,
								ordered: active.ordered,
								max_latency: active.max_latency,
								start_group: active.start_group,
								end_group: Some(cap),
							};
							active.stream.writer.encode(&update).await?;
						}
					}
				}
			}
		}
		Ok(())
	}

	/// Open the upstream SUBSCRIBE and start routing groups into the producer.
	///
	/// On Lite05+ the producer already exists (accepted from TRACK_INFO) and the
	/// subscription is accepted implicitly. On older drafts this waits for the first
	/// SUBSCRIBE_OK and promotes the pending request to a producer.
	async fn establish(
		&self,
		track: &mut Option<Track>,
		sub: &mut Sub<S>,
		subscription: Subscription,
		timescale: Option<Timescale>,
	) -> Result<(), Error> {
		let id = self.subscriber.next_id.fetch_add(1, atomic::Ordering::Relaxed);

		let msg = lite::Subscribe {
			id,
			broadcast: self.path.as_path(),
			track: self.name.as_str().into(),
			priority: subscription.priority,
			ordered: subscription.ordered,
			max_latency: subscription.stale,
			start_group: subscription.group_start,
			end_group: subscription.group_end,
		};

		tracing::info!(id, broadcast = %self.subscriber.log_path(&self.path), track = %self.name, "subscribe started");

		let mut stream = Stream::open(&self.subscriber.session, self.subscriber.version).await?;
		stream.writer.encode(&lite::ControlType::Subscribe).await?;
		stream.writer.encode(&msg).await?;

		let producer = if self.subscriber.version.has_timestamps() {
			// Lite05+: implicit acceptance, no SUBSCRIBE_OK. The producer already exists.
			let Some(Track::Active(producer)) = track.as_ref() else {
				unreachable!("lite05 track is active before establish");
			};
			producer.clone()
		} else {
			// Older drafts: the first SUBSCRIBE_OK promotes the pending request. Bail if
			// the broadcast dies meanwhile.
			let resp = tokio::select! {
				err = self.broadcast.closed() => return Err(err),
				resp = stream.reader.decode::<lite::SubscribeResponse>() => resp?,
			};
			if !matches!(resp, lite::SubscribeResponse::Ok(_)) {
				return Err(Error::ProtocolViolation);
			}

			// Accept with defaults: pre-lite-05 carries no timescale, and the cache
			// window falls back to the model default.
			let Some(Track::Pending(request)) = track.take() else {
				unreachable!("establish called without a pending track");
			};
			let mut producer = request.accept(crate::TrackInfo::default());
			// The accepted producer starts with a fresh subscription cursor, so its first
			// poll would re-report the subscription we just sent as a "change". Prime it
			// now (the params are already on the wire) so only genuine later changes fire.
			let _ = producer.poll_subscription_changed(&kio::Waiter::noop());
			*track = Some(Track::Active(producer.clone()));
			producer
		};

		// This session is now actively feeding the broadcast, so take the per-(session,
		// broadcast) viewer sentinel for the subscription's life.
		let abs = self.subscriber.origin.absolute(&self.path).to_owned();
		let broadcast_sub = self.subscriber.broadcasts.subscribe(&abs);

		self.subscriber.subscribes.lock().insert(
			id,
			TrackEntry {
				producer,
				stats: self.track_stats.clone(),
				timescale,
			},
		);

		*sub = Sub::Active(SubStream {
			stream,
			id,
			paused: false,
			ordered: subscription.ordered,
			max_latency: subscription.stale,
			start_group: subscription.group_start,
			priority: subscription.priority,
			_broadcast_sub: broadcast_sub,
		});

		Ok(())
	}

	/// Echo the current params upstream as a SUBSCRIBE_UPDATE, varying only `end_group`.
	async fn send_update(&self, active: &mut SubStream<S>, end_group: Option<u64>) -> Result<(), Error> {
		let update = lite::SubscribeUpdate {
			priority: active.priority,
			ordered: active.ordered,
			max_latency: active.max_latency,
			start_group: active.start_group,
			end_group,
		};
		active.stream.writer.encode(&update).await
	}

	/// Serve one downstream fetch end-to-end on its own bidi stream: send FETCH, then
	/// fill the group from the bare FRAME messages that follow. The timescale comes
	/// from this track's TRACK_INFO (already known), and the group sequence is
	/// implicit from the request. Runs to completion as an independent future in the
	/// serve loop's `FuturesUnordered`.
	async fn serve_fetch(self, request: GroupRequest, timescale: Option<Timescale>) {
		let TrackServe {
			mut subscriber,
			path,
			broadcast: _,
			track_stats,
			name,
		} = self;
		let group = request.sequence();

		tracing::info!(broadcast = %subscriber.log_path(&path), track = %name, group, "fetch started");

		let mut stream = match Stream::open(&subscriber.session, subscriber.version).await {
			Ok(stream) => stream,
			Err(err) => {
				tracing::warn!(track = %name, %err, "fetch stream open failed");
				return;
			}
		};

		let send = async {
			let msg = lite::Fetch {
				broadcast: path.as_path(),
				track: name.as_str().into(),
				priority: request.priority(),
				group,
				frame_start: 0,
			};
			stream.writer.encode(&lite::ControlType::Fetch).await?;
			stream.writer.encode(&msg).await
		};
		if let Err(err) = send.await {
			stream.writer.abort(&err);
			return;
		}

		// Make the group available (resolving the downstream fetch) and fill it. The
		// TrackInfo only takes effect if the track isn't accepted yet (a fetch with no
		// live subscription); otherwise the group inherits the accepted timescale.
		let group_info = TrackInfo {
			// FETCH is lite-05+, so `timescale` is `Some`; fall back to the default scale
			// defensively rather than panicking.
			timescale: timescale.unwrap_or_default(),
			..Default::default()
		};
		let mut producer = match request.accept(group_info) {
			Ok(producer) => producer,
			Err(err) => {
				// Already served (a concurrent fetch) or the track closed.
				tracing::debug!(track = %name, group, %err, "fetch not served");
				stream.writer.abort(&err);
				return;
			}
		};

		let res = subscriber
			.run_group(&mut stream.reader, producer.clone(), track_stats, timescale)
			.await;
		match res {
			Ok(()) => {
				let _ = producer.finish();
			}
			Err(err) => {
				let _ = producer.abort(err);
			}
		}
	}

	/// Tear the track down: drop the subscribes-map entry, FIN the upstream if open,
	/// and abort the producer (or reject the still-pending request) with `err`.
	fn finish_track(&self, track: Track, mut sub: Sub<S>, err: Error) {
		if let Sub::Active(active) = &mut sub {
			self.subscriber.subscribes.lock().remove(&active.id);
			let _ = active.stream.writer.finish();
		}

		match track {
			Track::Active(mut producer) => {
				let _ = producer.abort(err);
			}
			Track::Pending(request) => request.reject(err),
		}
		// Dropping `sub` releases the BroadcastSubscription viewer sentinel.
	}
}
