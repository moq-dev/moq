use crate::{frame, group, origin, track};
use std::{
	collections::HashMap,
	task::{Poll, ready},
	time::Duration,
};

use web_transport_trait::{MaybeSend, MaybeSync, poll::SendStream as _};

use crate::{
	AsPath, Error, Timescale,
	coding::{Stream, Writer},
	ietf::{self, Control, FetchHeader, FetchType, FilterType, GroupOrder, Location, RequestId},
	track::Subscription,
	util::{MaybeBoxedExt, MaybeSendBox},
};

use super::{Message, Version, cluster, peer};

/// Largest millisecond duration every implementation can carry losslessly.
const MAX_SAFE_AGE_MS: u64 = (1_u64 << 53) - 1;

/// Build the serving-side subscription for a peer whose wire protocol carries no
/// max age preference. The receiver applies its own budget after the transfer.
fn serving_subscription(subscriber_priority: u8) -> Subscription {
	Subscription {
		priority: super::priority::from_wire(subscriber_priority),
		// Demand can cross a Lite hop before the producer's retention bound is
		// known, so use the largest duration that remains wire-encodable.
		max_age: Duration::from_millis(MAX_SAFE_AGE_MS),
		..Default::default()
	}
}

/// A broadcast whose route table is watched for changes in what we advertise: the
/// namespace becoming (un)advertisable, or its path or cost moving.
struct Watched {
	broadcast: crate::broadcast::Consumer,
	/// Demand edges re-price the serving route without a route change (see
	/// [`crate::broadcast::outgoing_cost`]), so the loop watches this too.
	demand: crate::broadcast::Demand,
	/// What the peer currently holds for this namespace, or [`Advert::None`] while it
	/// is filtered. A selection that differs is worth a wire message; one that matches
	/// is not.
	sent: Advert,
	/// When demand drained while a zero cost was advertised. Restoring the cold cost is
	/// deferred by [`crate::broadcast::COST_LINGER`] past this, so viewer churn does not
	/// flap routing across the mesh; demand returning in the window cancels it.
	idle_at: Option<crate::runtime::Instant>,
	/// Set once the broadcast errors, so a dead entry stops being polled.
	dead: bool,
	/// The peer should hold this namespace but does not: it refused the request, or we
	/// could not get a stream to make it on. Nothing about that clears on its own, so the
	/// loop comes back to it on a timer.
	deferred: bool,
	/// What the peer's refusal said about coming back, which outranks that timer.
	refused: Refused,
}

/// What a refusal said about re-offering the namespace.
///
/// A peer answers a request it declines with a retry interval ({{moqt}} REQUEST_ERROR),
/// and ignoring it is how a permanent refusal (unauthorized, uninterested) turns into a
/// request every few seconds for the life of the session.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Refused {
	/// Never refused, or refused on a draft whose error carries no interval, so our own
	/// backoff is the only guidance there is.
	#[default]
	No,
	/// Refused with a minimum wait before re-offering.
	Until(crate::runtime::Instant),
	/// Refused with an interval of 0: the peer does not want this offered again.
	Never,
}

impl Refused {
	/// Whether a fresh offer may go out now.
	///
	/// The single gate, consulted on every reconciliation rather than only on the retry
	/// sweep: a route change re-prices an advertisement but does not excuse us from a
	/// wait the peer asked for, nor make a refused namespace a different one.
	fn offerable(&self, now: crate::runtime::Instant) -> bool {
		match self {
			Self::No => true,
			Self::Until(at) => now >= *at,
			Self::Never => false,
		}
	}

	/// Whether the loop should keep coming back at all. Only a refusal that forbids
	/// retrying ends it; a wait still has to arm the timer that counts it out.
	fn pending(&self) -> bool {
		*self != Self::Never
	}
}

impl Watched {
	fn new(broadcast: crate::broadcast::Consumer) -> Self {
		Self {
			demand: broadcast.demand(),
			broadcast,
			sent: Advert::None,
			idle_at: None,
			dead: false,
			deferred: false,
			refused: Refused::No,
		}
	}

	/// Record what the peer now holds, retiring any linger the old advertisement armed.
	///
	/// Only a discounted advertisement has a cost to restore. Leaving `idle_at` set past
	/// one that isn't would keep [`Publisher::linger_deadline`] handing back an expired
	/// instant that nothing ever clears, and the announce loops would spin on it.
	fn set_sent(&mut self, sent: Advert) {
		if !sent.discounted() {
			self.idle_at = None;
		}
		self.sent = sent;
	}
}

/// What to advertise to this peer for one broadcast.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum Advert {
	/// Nothing: every route loops through the peer or us, or none is announced.
	#[default]
	None,
	/// The namespace, with no routing information: the peer did not negotiate the MoQ
	/// Cluster extension, so there is nowhere to put a path or a cost.
	Plain,
	/// The namespace, with the path it traversed and that path's accumulated cost.
	Cluster(cluster::Advert),
}

impl Advert {
	/// Whether the peer should hold this namespace at all.
	fn wanted(&self) -> bool {
		!matches!(self, Self::None)
	}

	/// The parameters to put on the wire, as the message structs carry them.
	fn params(&self) -> Option<cluster::Advert> {
		match self {
			Self::Cluster(advert) => Some(advert.clone()),
			_ => None,
		}
	}

	/// Whether this advertisement carries a zero cost, which is what the serving-route
	/// discount produces and what the linger is watching to restore.
	fn discounted(&self) -> bool {
		matches!(self, Self::Cluster(advert) if advert.cost == 0)
	}
}

/// What a watched broadcast reported.
enum Watch {
	/// Its route table or demand moved; re-run the selection.
	Changed(crate::PathOwned),
	/// Demand just drained while a discounted cost was advertised. The cold cost is
	/// restored once the linger expires, not now, so viewer churn does not flap routing.
	Idle(crate::PathOwned),
}

/// How long to wait for a stream to advertise one namespace on.
///
/// Only reached when the peer has granted no more, which on this path means it is holding
/// every advertisement we already sent. Long enough that a merely slow peer is not given
/// up on, short enough that the loop resumes and can retire something.
const ADVERTISE_TIMEOUT: Duration = Duration::from_secs(5);

/// First wait before re-offering a namespace we could not get up.
const RETRY_BASE: Duration = Duration::from_millis(100);

/// Ceiling on that wait. The loop retries for the life of the session, so it must settle
/// into a slow poll rather than a spin.
const RETRY_MAX: Duration = Duration::from_secs(5);

/// Spread a retry over half its window, so every namespace on a busy relay does not come
/// back at the same instant.
fn jitter(delay: Duration) -> Duration {
	use rand::RngExt;
	delay.mul_f64(0.5 + rand::rng().random::<f64>() / 2.0)
}

/// Where one announce loop's advertisements go.
enum Target<S: crate::transport::poll::Session> {
	/// Inline NAMESPACE entries on the SUBSCRIBE_NAMESPACE stream that asked for them
	/// (draft-16+).
	Inline(Stream<S, Version>),
	/// Each advertisement on its own PUBLISH_NAMESPACE request. Unsolicited when there
	/// is no stream; draft-14/15 answer a SUBSCRIBE_NAMESPACE this way, since they
	/// predate NAMESPACE, and hold onto its stream to end with the subscription.
	Requests(Option<Stream<S, Version>>),
}

impl<S: crate::transport::poll::Session> Target<S> {
	/// The SUBSCRIBE_NAMESPACE stream this loop answers, if any.
	fn stream(&mut self) -> Option<&mut Stream<S, Version>> {
		match self {
			Self::Inline(stream) | Self::Requests(Some(stream)) => Some(stream),
			Self::Requests(None) => None,
		}
	}

	/// Ready when the peer ends this loop by closing the stream it asked on.
	///
	/// An unsolicited loop has no stream of its own to watch and parks here: the
	/// session driver polling it is what drops it when the session ends.
	fn poll_closed(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Error>> {
		match self.stream() {
			Some(stream) => stream.reader.poll_closed(cx),
			None => Poll::Pending,
		}
	}
}

/// One announce loop's state: where its advertisements go, and what the peer holds.
struct Namespaces<S: crate::transport::poll::Session> {
	/// What the peer declared in its SETUP, which decides what an advertisement carries.
	peer: cluster::Peer,
	target: Target<S>,
	/// Every announced broadcast under this loop's prefix.
	watched: HashMap<crate::PathOwned, Watched>,
	/// The open PUBLISH_NAMESPACE request carrying each advertised namespace. Empty when
	/// the entries ride a SUBSCRIBE_NAMESPACE stream inline.
	requests: HashMap<crate::PathOwned, NamespaceRequest<S>>,
}

impl<S: crate::transport::poll::Session> Namespaces<S> {
	fn new(peer: cluster::Peer, target: Target<S>) -> Self {
		Self {
			peer,
			target,
			watched: HashMap::new(),
			requests: HashMap::new(),
		}
	}
}

/// What woke an announce-forwarding loop.
enum NamespaceEvent {
	/// The session or stream ended, with the result to surface.
	Closed(Result<(), Error>),
	/// An origin-level (un)announce, `None` once the announce stream ends.
	Update(Option<crate::announce::Update>),
	/// A watched broadcast's route table or demand moved; re-run the selection.
	Routes(crate::PathOwned),
	/// A watched broadcast's demand drained; start its linger.
	Idle(crate::PathOwned),
	/// The linger sleep fired without an expired entry (it was canceled, or a later
	/// deadline remains): restart the turn so the next deadline arms a fresh sleep.
	Linger,
	/// The retry sleep fired: re-offer whatever the peer should be holding and isn't.
	Retry,
}

#[derive(Clone)]
pub(super) struct Publisher<S: crate::transport::poll::Session, R: crate::runtime::Runtime> {
	// Arms the advertise, retry, and linger timers.
	runtime: R,
	session: S,
	// Traffic stats are attributed through this tagged origin handle.
	origin: origin::Consumer,
	control: Control,
	// Our own Hop ID, stamped onto every advertisement we forward. Taken from the
	// origin we consume so it matches the local relay identity across every session,
	// which is what makes cross-session loop detection work.
	self_origin: crate::Origin,
	// The identity assigned to the peer by `Client::with_peer_origin`, used when the
	// peer declares none itself. A peer that negotiates the MoQ Cluster extension
	// declares its own, which wins.
	peer_origin: Option<crate::Origin>,
	// What the peer declared in its SETUP, filled when that stream is read.
	peer_setup: peer::PeerSetup,
	version: Version,
}

impl<S, R> Publisher<S, R>
where
	S: crate::transport::poll::Session,
	R: crate::runtime::Runtime + MaybeSend + MaybeSync + 'static,
	R::Timer: MaybeSend,
{
	pub fn new(
		runtime: R,
		session: S,
		origin: origin::Consumer,
		control: Control,
		peer_origin: Option<crate::Origin>,
		peer_setup: peer::PeerSetup,
		version: Version,
	) -> Self {
		Self {
			runtime,
			session,
			self_origin: *origin,
			origin,
			control,
			peer_origin,
			peer_setup,
			version,
		}
	}

	/// What the peer declared in its SETUP, or the default (extension off) on a version
	/// that cannot negotiate it.
	///
	/// Blocks until the peer's SETUP arrives, because the extension changes the NAMESPACE
	/// encoding: nothing can be advertised until we know whether the peer speaks it.
	async fn peer(&self) -> cluster::Peer {
		match cluster::supported(self.version) {
			true => self.peer_setup.get().await.cluster,
			false => cluster::Peer::default(),
		}
	}

	/// Whether the peer requires advertisements to be solicited, from the same SETUP.
	///
	/// Blocks on it for the same reason [`Self::peer`] does: this decides whether the
	/// first advertisement is sent unasked, so it cannot be guessed and corrected later.
	async fn requires_solicitation(&self) -> bool {
		self.peer_setup.get().await.solicit.unwrap_or(false)
	}

	/// The origin to serve this peer's subscriptions from: sources whose hop chain flows
	/// through the peer are excluded, so a subscription is never handed data that already
	/// flowed through the subscriber.
	///
	/// The same exclusion the announce path applies (see [`Self::select`]), which is what
	/// keeps advertised paths truthful and prevents subscription cycles of any length.
	async fn serving_origin(&self) -> origin::Consumer {
		let peer = self.peer().await;
		match self.exclude(&peer) {
			crate::Origin::UNKNOWN => self.origin.clone(),
			exclude => self.origin.clone().excluding(exclude),
		}
	}

	/// The Hop ID whose paths must not be advertised (or served) back to this peer.
	///
	/// A peer that negotiated the extension declares its own; otherwise fall back to
	/// one the caller assigned out of band (`Client::with_peer_origin`), since
	/// moq-transport itself carries no identity.
	fn exclude(&self, peer: &cluster::Peer) -> crate::Origin {
		match peer.negotiated() {
			true => peer.exclude(),
			false => self.peer_origin.unwrap_or(crate::Origin::UNKNOWN),
		}
	}

	/// Pick what to advertise to this peer for one broadcast.
	///
	/// A same-path source can splice in or detach without an origin-level (un)announce,
	/// and demand can re-price the serving route in place, so the announce loops watch
	/// every announced broadcast (see [`Watched`]) and re-run this when either moves.
	fn select(&self, watch: &Watched, peer: &cluster::Peer) -> Advert {
		let routes = watch.broadcast.routes();
		let exclude = self.exclude(peer);

		for (route, serving) in crate::broadcast::advertisable_routes(&routes, self.self_origin, exclude) {
			if !peer.negotiated() {
				return Advert::Plain;
			}

			// The Cluster extension has room for the warm cost only; a peer on this
			// wire learns nothing about the cold path (see `cluster::Advert::route`).
			let cost = crate::broadcast::outgoing_cost(&watch.demand, route, serving).warm;
			// Our own Hop ID is always the last entry, so the peer reconstructs the full
			// path. A chain with no room left is a loop in all but name; try the next route.
			match cluster::Advert::forward(&route.hops, cost, self.self_origin) {
				Ok(advert) => return Advert::Cluster(advert),
				Err(_) => continue,
			}
		}

		Advert::None
	}

	/// Poll every watched broadcast for a change in what it should advertise, reporting
	/// the first changed path.
	///
	/// Two things move it: the route table (a failover, a standby attaching, a
	/// re-price) and demand on the serving route (which switches the discount on and
	/// off). `fired` is the linger deadline's verdict for this turn.
	fn poll_watched(
		watched: &mut HashMap<crate::PathOwned, Watched>,
		fired: Option<crate::runtime::Instant>,
		waiter: &kio::Waiter,
	) -> Poll<Watch> {
		for (path, watch) in watched.iter_mut() {
			if watch.dead {
				continue;
			}
			match watch.broadcast.poll_routes_changed(waiter) {
				Poll::Ready(Ok(())) => return Poll::Ready(Watch::Changed(path.clone())),
				// A dying broadcast has no further route changes; the origin's
				// unannounce is what removes the entry.
				Poll::Ready(Err(_)) => {
					watch.dead = true;
					continue;
				}
				Poll::Pending => {}
			}

			// Only a cost-carrying advertisement re-prices on demand; a plain one has
			// nowhere to put the discount, so watching demand would fire forever.
			if !matches!(watch.sent, Advert::Cluster(_)) {
				continue;
			}

			if !watch.sent.discounted() {
				// Not discounted: demand arriving is what applies the discount.
				if let Poll::Ready(Ok(())) = watch.demand.poll_used(waiter) {
					return Poll::Ready(Watch::Changed(path.clone()));
				}
				continue;
			}

			match watch.idle_at {
				// Demand coming back within the linger cancels the restore; fall through
				// to re-arm the unused watch.
				Some(_) if watch.demand.is_used() => watch.idle_at = None,
				// The linger expired: restore the cold cost.
				Some(at) if fired.is_some_and(|now| now >= at + crate::broadcast::COST_LINGER) => {
					watch.idle_at = None;
					return Poll::Ready(Watch::Changed(path.clone()));
				}
				// Still lingering: the sleep owns the wakeup, and `poll_used` re-arms
				// the cancel check above.
				Some(_) => {
					let _ = watch.demand.poll_used(waiter);
					continue;
				}
				None => {}
			}

			// Demand just drained. Start the linger rather than re-pricing now, and end
			// the turn so the caller arms a deadline for it.
			if let Poll::Ready(Ok(())) = watch.demand.poll_unused(waiter) {
				return Poll::Ready(Watch::Idle(path.clone()));
			}
		}
		Poll::Pending
	}

	/// The earliest deferred cost-restore across every watched broadcast.
	fn linger_deadline(watched: &HashMap<crate::PathOwned, Watched>) -> Option<crate::runtime::Instant> {
		watched
			.values()
			.filter_map(|watch| watch.idle_at)
			.min()
			.map(|at| at + crate::broadcast::COST_LINGER)
	}

	/// Handle an incoming bidi stream dispatched by the session.
	pub fn handle_stream(
		&self,
		id: u64,
		mut data: bytes::Bytes,
		stream: Stream<S, Version>,
	) -> Result<MaybeSendBox<'static, ()>, Error> {
		let this = self.clone();
		let task = match id {
			ietf::Subscribe::ID => {
				let msg = ietf::Subscribe::decode_msg(&mut data, this.version)?;
				if !data.is_empty() {
					return Err(Error::WrongSize);
				}
				tracing::debug!(message = ?msg, "received subscribe");
				async move {
					if let Err(err) = this.run_subscribe_stream(stream, msg).await {
						tracing::debug!(%err, "subscribe stream error");
					}
				}
				.maybe_boxed()
			}
			ietf::Fetch::ID => {
				let msg = ietf::Fetch::decode_msg(&mut data, this.version)?;
				if !data.is_empty() {
					return Err(Error::WrongSize);
				}
				tracing::debug!(message = ?msg, "received fetch");
				async move {
					if let Err(err) = this.run_fetch_stream(stream, msg).await {
						tracing::debug!(%err, "fetch stream error");
					}
				}
				.maybe_boxed()
			}
			// Draft-18 SUBSCRIBE_NAMESPACE (0x50) and the legacy 0x11 message decode
			// to the same request_id + namespace; the legacy Subscribe Options field
			// is ignored (moq-lite never subscribes to tracks).
			ietf::SubscribeNamespace::ID | ietf::SubscribeNamespaceLegacy::ID => {
				let msg = if id == ietf::SubscribeNamespace::ID {
					ietf::SubscribeNamespace::decode_msg(&mut data, this.version)?
				} else {
					let legacy = ietf::SubscribeNamespaceLegacy::decode_msg(&mut data, this.version)?;
					ietf::SubscribeNamespace {
						request_id: legacy.request_id,
						namespace: legacy.namespace,
					}
				};
				if !data.is_empty() {
					return Err(Error::WrongSize);
				}
				tracing::debug!(message = ?msg, "received subscribe_namespace");
				async move {
					if let Err(err) = this.run_subscribe_namespace_stream(stream, msg).await {
						tracing::debug!(%err, "subscribe_namespace stream error");
					}
				}
				.maybe_boxed()
			}
			ietf::TrackStatus::ID => {
				tracing::warn!("TrackStatus not supported");
				async {}.maybe_boxed()
			}
			_ => {
				tracing::warn!(id, "unexpected bidi stream type for publisher");
				return Err(Error::UnexpectedStream);
			}
		};
		Ok(task)
	}

	/// Handle a SUBSCRIBE on its bidi stream.
	async fn run_subscribe_stream(self, mut stream: Stream<S, Version>, msg: ietf::Subscribe<'_>) -> Result<(), Error> {
		match msg.filter_type {
			FilterType::AbsoluteStart | FilterType::AbsoluteRange => {
				tracing::warn!(?msg, "absolute subscribe not supported, ignoring");
			}
			FilterType::NextGroup => {
				tracing::warn!(?msg, "next group subscribe not supported, ignoring");
			}
			FilterType::LargestObject => {}
		};

		let request_id = msg.request_id;
		let track_name = msg.track_name.clone();
		let absolute = self.origin.absolute(&msg.track_namespace).to_owned();

		tracing::info!(id = %request_id, broadcast = %absolute, track = %track_name, "subscribe started");

		// Stats (subscriptions, viewer refcount, groups/frames/bytes) are counted in
		// the model, through the tagged `origin::Consumer` the broadcast resolves from.

		// We just received a subscribe for this exact namespace, so the peer must have already
		// seen the announcement. `request_broadcast` resolves it immediately, or falls back to
		// an `origin::Dynamic` handler if one is registered.
		let broadcast = match self
			.serving_origin()
			.await
			.request_broadcast(&msg.track_namespace)
			.await
		{
			Ok(broadcast) => broadcast,
			Err(_) => {
				return self
					.reject_subscribe(stream, request_id, 404, "Broadcast not found")
					.await;
			}
		};

		// moq-transport has no subscriber max age parameter. Keep everything the
		// producer retained and let the receiving subscriber enforce its own budget.
		let subscription = serving_subscription(msg.subscriber_priority);

		let track = match async { broadcast.track(&msg.track_name)?.subscribe(subscription).await }.await {
			Ok(track) => track,
			Err(err) => {
				return self.reject_subscribe(stream, request_id, 404, &err.to_string()).await;
			}
		};

		// Send SubscribeOk on the stream
		stream.writer.encode(&ietf::SubscribeOk::ID).await?;
		stream
			.writer
			.encode(&ietf::SubscribeOk {
				request_id: match self.version {
					Version::Draft14 | Version::Draft15 | Version::Draft16 => Some(request_id),
					_ => None,
				},
				track_alias: request_id.0,
				properties: ietf::Properties {
					// Declaring the timescale is what opts the track into timestamps; every
					// object Timestamp below is in these units.
					timescale: Some(track.info().timescale),
					// We serve the newest group first, matching moq-lite.
					group_order: Some(GroupOrder::Descending),
				},
			})
			.await?;

		// Run the track, cancelling on reader close (Unsubscribe or stream close)
		let res = {
			let mut closed_session = self.session.clone();
			let mut serve = TrackServe::new(self.session.clone(), track, request_id, self.version);
			kio::wait(|waiter| {
				if let Poll::Ready(res) = serve.poll(waiter) {
					return Poll::Ready(res);
				}
				let mut cx = std::task::Context::from_waker(waiter.waker());
				if stream.reader.poll_closed(&mut cx).is_ready() || closed_session.poll_closed(&mut cx).is_ready() {
					return Poll::Ready(Ok(()));
				}
				Poll::Pending
			})
			.await
		};

		// Send PublishDone
		let (status_code, reason) = match &res {
			Ok(()) => (200, "OK"),
			Err(_) => (500, "error"),
		};
		let _ = stream.writer.encode(&ietf::PublishDone::ID).await;
		let _ = stream
			.writer
			.encode(&ietf::PublishDone {
				request_id: match self.version {
					Version::Draft14 | Version::Draft15 | Version::Draft16 => Some(request_id),
					_ => None,
				},
				status_code,
				stream_count: 0,
				reason_phrase: reason.into(),
			})
			.await;

		// PUBLISH_DONE is the last thing on this stream, so it needs the acknowledgement too.
		let _ = stream.writer.close().await;

		res
	}

	/// Reject a SUBSCRIBE, ending the request stream.
	///
	/// Takes the whole stream because delivering the error is the other half of the job:
	/// [`Writer`] resets on drop, and a reset discards data the peer has not acknowledged, so
	/// returning here without [`Writer::close`] leaves the subscriber waiting on a request we
	/// already refused.
	async fn reject_subscribe(
		&self,
		mut stream: Stream<S, Version>,
		request_id: RequestId,
		error_code: u64,
		reason: &str,
	) -> Result<(), Error> {
		self.write_subscribe_error(&mut stream.writer, request_id, error_code, reason)
			.await?;

		// The peer dropping the stream once it has the rejection is a normal end, not our failure.
		let _ = stream.writer.close().await;
		Ok(())
	}

	/// Write a subscribe error on the bidi stream writer.
	async fn write_subscribe_error(
		&self,
		writer: &mut Writer<S::SendStream, Version>,
		request_id: RequestId,
		error_code: u64,
		reason: &str,
	) -> Result<(), Error> {
		match self.version {
			Version::Draft14 => {
				writer.encode(&ietf::SubscribeError::ID).await?;
				writer
					.encode(&ietf::SubscribeError {
						request_id,
						error_code,
						reason_phrase: reason.into(),
					})
					.await?;
			}
			Version::Draft15 | Version::Draft16 => {
				writer.encode(&ietf::RequestError::ID).await?;
				writer
					.encode(&ietf::RequestError {
						request_id: Some(request_id),
						error_code,
						reason_phrase: reason.into(),
						retry_interval: 0,
					})
					.await?;
			}
			_ => {
				writer.encode(&ietf::RequestError::ID).await?;
				writer
					.encode(&ietf::RequestError {
						request_id: None,
						error_code,
						reason_phrase: reason.into(),
						retry_interval: 0,
					})
					.await?;
			}
		}
		Ok(())
	}

	/// Handle a FETCH on its bidi stream.
	async fn run_fetch_stream(mut self, mut stream: Stream<S, Version>, msg: ietf::Fetch<'_>) -> Result<(), Error> {
		let _subscribe_id = match msg.fetch_type {
			FetchType::Standalone { .. } => {
				return self.reject_fetch(stream, msg.request_id, 500, "not supported").await;
			}
			FetchType::RelativeJoining {
				subscriber_request_id,
				group_offset,
			} => {
				if group_offset != 0 {
					return self.reject_fetch(stream, msg.request_id, 500, "not supported").await;
				}
				subscriber_request_id
			}
			FetchType::AbsoluteJoining { .. } => {
				return self.reject_fetch(stream, msg.request_id, 500, "not supported").await;
			}
		};

		// Send FetchOk/RequestOk
		self.write_fetch_ok(&mut stream.writer, msg.request_id).await?;

		// Create a uni stream with just a FetchHeader and FIN it
		let uni = self.session.open_uni().await.map_err(Error::from_transport)?;
		let mut writer = Writer::new(uni, self.version);
		writer.encode(&FetchHeader::TYPE).await?;
		writer
			.encode(&FetchHeader {
				request_id: msg.request_id,
			})
			.await?;
		writer.close().await?;

		Ok(())
	}

	async fn write_fetch_ok(
		&self,
		writer: &mut Writer<S::SendStream, Version>,
		request_id: RequestId,
	) -> Result<(), Error> {
		match self.version {
			Version::Draft14 => {
				writer.encode(&ietf::FetchOk::ID).await?;
				writer
					.encode(&ietf::FetchOk {
						request_id: Some(request_id),
						group_order: GroupOrder::Descending,
						end_of_track: false,
						end_location: Location { group: 0, object: 0 },
					})
					.await?;
			}
			Version::Draft15 | Version::Draft16 => {
				writer.encode(&ietf::RequestOk::ID).await?;
				writer
					.encode(&ietf::RequestOk {
						request_id: Some(request_id),
					})
					.await?;
			}
			_ => {
				writer.encode(&ietf::RequestOk::ID).await?;
				writer.encode(&ietf::RequestOk { request_id: None }).await?;
			}
		}
		Ok(())
	}

	/// Reject a FETCH, ending the request stream. See [`Self::reject_subscribe`] for why the
	/// close is not optional.
	async fn reject_fetch(
		&self,
		mut stream: Stream<S, Version>,
		request_id: RequestId,
		error_code: u64,
		reason: &str,
	) -> Result<(), Error> {
		self.write_fetch_error(&mut stream.writer, request_id, error_code, reason)
			.await?;

		let _ = stream.writer.close().await;
		Ok(())
	}

	async fn write_fetch_error(
		&self,
		writer: &mut Writer<S::SendStream, Version>,
		request_id: RequestId,
		error_code: u64,
		reason: &str,
	) -> Result<(), Error> {
		match self.version {
			Version::Draft14 => {
				writer.encode(&ietf::FetchError::ID).await?;
				writer
					.encode(&ietf::FetchError {
						request_id,
						error_code,
						reason_phrase: reason.into(),
					})
					.await?;
			}
			Version::Draft15 | Version::Draft16 => {
				writer.encode(&ietf::RequestError::ID).await?;
				writer
					.encode(&ietf::RequestError {
						request_id: Some(request_id),
						error_code,
						reason_phrase: reason.into(),
						retry_interval: 0,
					})
					.await?;
			}
			_ => {
				writer.encode(&ietf::RequestError::ID).await?;
				writer
					.encode(&ietf::RequestError {
						request_id: None,
						error_code,
						reason_phrase: reason.into(),
						retry_interval: 0,
					})
					.await?;
			}
		}
		Ok(())
	}

	/// Bring the peer's view of one namespace in line with the current selection.
	///
	/// A loop writing inline (draft-16+ answering a SUBSCRIBE_NAMESPACE) re-sends
	/// NAMESPACE on that stream, which the receiver treats as a replacement, and
	/// retracts with NAMESPACE_DONE. Otherwise each advertisement rides its own
	/// PUBLISH_NAMESPACE request: an update re-sends PUBLISH_NAMESPACE **on the stream
	/// that already carries it**, since a second stream would leave two claiming one
	/// namespace, and a withdrawal closes the request with PUBLISH_NAMESPACE_DONE.
	async fn sync_namespace(
		&self,
		ns: &mut Namespaces<S>,
		suffix: &crate::PathOwned,
		path: &crate::PathOwned,
	) -> Result<(), Error> {
		let Namespaces {
			peer,
			target,
			watched,
			requests,
		} = ns;

		let Some(watch) = watched.get(suffix) else {
			return Ok(());
		};
		let advert = self.select(watch, peer);
		let refused = watch.refused;
		let wanted = advert.wanted();
		let held = watch.sent.wanted();
		let unchanged = advert == watch.sent;

		if unchanged {
			// Nothing to send. A namespace that is no longer advertisable is no longer
			// pending either, and leaving that set would keep the retry timer armed
			// forever for a wire message that can never happen.
			if !wanted && let Some(watch) = watched.get_mut(suffix) {
				watch.deferred = false;
			}
			return Ok(());
		}

		// A fresh offer waits for what the refusal asked for, whatever brought us back.
		// Only withdrawing and re-announcing clears it, since that builds a fresh entry.
		if wanted && !held && !refused.offerable(self.runtime.now()) {
			return Ok(());
		}

		let absolute = self.origin.absolute(path).to_owned();
		// Only a fresh PUBLISH_NAMESPACE request can be refused; everything else below
		// either rides a stream the peer already accepted or says nothing at all.
		let mut refused = watch.refused;
		let sent = match target {
			Target::Requests(_) => {
				match (advert.wanted(), requests.get_mut(suffix)) {
					(false, _) => {
						if held {
							tracing::debug!(broadcast = %absolute, "namespace_done");
						}
						self.withdraw_namespace(target, requests, suffix.clone()).await?;
					}
					(true, Some(request)) => {
						tracing::debug!(broadcast = %absolute, "announce update");
						request.stream.writer.encode(&ietf::PublishNamespace::ID).await?;
						request
							.stream
							.writer
							.encode(&ietf::PublishNamespace {
								request_id: request.request_id,
								track_namespace: request.path.as_path(),
								cluster: advert.params(),
							})
							.await?;
					}
					(true, None) => {
						tracing::debug!(broadcast = %absolute, "publish_namespace");
						refused = self
							.advertise_namespace(requests, path, suffix.clone(), advert.params())
							.await?;
					}
				}
				// The peer can reject a fresh PUBLISH_NAMESPACE, which leaves no request
				// behind. Record what it actually holds, so a later route change retries
				// instead of believing the namespace is already advertised.
				match requests.contains_key(suffix) {
					true => advert,
					false => Advert::None,
				}
			}
			Target::Inline(stream) => {
				match (advert.wanted(), held) {
					(true, _) => {
						tracing::debug!(broadcast = %absolute, "namespace");
						stream.writer.encode(&ietf::Namespace::ID).await?;
						stream
							.writer
							.encode(&ietf::Namespace {
								suffix: suffix.as_path(),
								cluster: advert.params(),
							})
							.await?;
					}
					(false, true) => {
						tracing::debug!(broadcast = %absolute, "namespace_done");
						stream.writer.encode(&ietf::NamespaceDone::ID).await?;
						stream
							.writer
							.encode(&ietf::NamespaceDone {
								suffix: suffix.as_path(),
							})
							.await?;
					}
					// Never advertised and still not advertisable: nothing to say.
					(false, false) => {}
				}
				advert
			}
		};

		if let Some(watch) = watched.get_mut(suffix) {
			// A peer that asked not to be offered this again outranks the retry timer;
			// anything else it should hold and does not comes back on one.
			watch.refused = refused;
			watch.deferred = wanted && !sent.wanted() && refused.pending();
			watch.set_sent(sent);
		}
		Ok(())
	}

	/// Open a PUBLISH_NAMESPACE request for one namespace, recording it in `requests`
	/// so an update or withdrawal reuses the same stream. A declined request records
	/// nothing: a peer that wants none of this rejects each one and stays connected.
	///
	/// Returns what the refusal, if any, said about coming back.
	async fn advertise_namespace(
		&self,
		requests: &mut HashMap<crate::PathOwned, NamespaceRequest<S>>,
		path: &crate::PathOwned,
		suffix: crate::PathOwned,
		cluster: Option<cluster::Advert>,
	) -> Result<Refused, Error> {
		let request_id = self.control.next_request_id(&self.runtime).await?;

		// Bounded, because an advertisement holds its stream for as long as the namespace
		// lives: a peer whose concurrent-stream limit we have filled makes this open block,
		// and the withdrawals queued behind it are the only thing that would free a slot.
		// Giving up records nothing, so the namespace is simply retried later.
		let Some(mut request) = self.open_request().await? else {
			tracing::debug!(broadcast = %self.origin.absolute(path), "no stream for the advertisement");
			return Ok(Refused::No);
		};

		request.writer.encode(&ietf::PublishNamespace::ID).await?;
		request
			.writer
			.encode(&ietf::PublishNamespace {
				request_id,
				track_namespace: path.as_path(),
				cluster,
			})
			.await?;

		// Bounded for the same reason the open is: a peer that takes the stream and answers
		// nothing would park this loop forever, and every withdrawal queued behind it.
		let Some((type_id, mut data)) = self.read_response(&mut request).await? else {
			tracing::debug!(broadcast = %self.origin.absolute(path), "no answer to the advertisement");
			return Ok(Refused::No);
		};

		match (self.version, type_id) {
			(Version::Draft14, ietf::PublishNamespaceOk::ID) => {
				let msg = ietf::PublishNamespaceOk::decode_msg(&mut data, self.version)?;
				tracing::debug!(message = ?msg, "publish namespace ok");
			}
			(Version::Draft14, ietf::PublishNamespaceError::ID) => {
				let msg = ietf::PublishNamespaceError::decode_msg(&mut data, self.version)?;
				tracing::warn!(message = ?msg, "publish namespace error");
				// Draft-14's error carries no retry interval, so our own backoff stands.
				return Ok(Refused::No);
			}
			(_, ietf::RequestOk::ID) => {
				let msg = ietf::RequestOk::decode_msg(&mut data, self.version)?;
				tracing::debug!(message = ?msg, "publish namespace ok");
			}
			(_, ietf::RequestError::ID) => {
				let msg = ietf::RequestError::decode_msg(&mut data, self.version)?;
				tracing::warn!(message = ?msg, "publish namespace error");
				return Ok(self.refusal(msg.retry_interval));
			}
			_ => return Err(Error::UnexpectedMessage),
		}

		requests.insert(
			suffix,
			NamespaceRequest {
				path: path.clone(),
				request_id,
				stream: request,
			},
		);
		Ok(Refused::No)
	}

	/// How to read a refusal's retry interval, in milliseconds.
	///
	/// Draft-14/15 errors carry no interval, so a decoded 0 there says nothing and our own
	/// backoff stands. Everywhere else 0 is the peer asking not to be offered this again,
	/// which is what keeps a permanent refusal (unauthorized, uninterested) from becoming
	/// a request every few seconds for the life of the session.
	fn refusal(&self, retry_interval: u64) -> Refused {
		match (self.version, retry_interval) {
			(Version::Draft14 | Version::Draft15, _) => Refused::No,
			(_, 0) => Refused::Never,
			(_, ms) => Refused::Until(self.runtime.now() + Duration::from_millis(ms)),
		}
	}

	/// Open a stream for one advertisement, or `None` if the peer did not give us one in
	/// time.
	///
	/// The announce loop is single-threaded over origin updates, so an open that parks
	/// forever parks everything, including the unannounces that release the streams the
	/// peer is waiting on us to retire. Failing instead keeps the loop moving.
	async fn open_request(&self) -> Result<Option<Stream<S, Version>>, Error> {
		let mut session = self.session.clone();
		let mut open = std::pin::pin!(Stream::open(&mut session, self.version));
		let mut timeout = crate::runtime::Deadline::after(&self.runtime, ADVERTISE_TIMEOUT);

		kio::wait(|waiter| {
			if let Poll::Ready(res) = waiter.poll_future(open.as_mut()) {
				return Poll::Ready(res.map(Some));
			}
			if timeout.poll(waiter).is_ready() {
				return Poll::Ready(Ok(None));
			}
			Poll::Pending
		})
		.await
	}

	/// Read the peer's answer to one advertisement, or `None` if it did not answer in time.
	///
	/// Bounded for the same reason [`Self::open_request`] is, and it is the same peer
	/// behavior seen a step later: a stream the peer accepts and never answers on holds the
	/// loop just as effectively as one it never grants. Giving up records nothing, so the
	/// namespace stays outstanding and the retry re-offers it.
	async fn read_response(&self, request: &mut Stream<S, Version>) -> Result<Option<(u64, bytes::Bytes)>, Error> {
		let mut read = std::pin::pin!(async {
			let type_id: u64 = request.reader.decode().await?;
			let size: u16 = request.reader.decode().await?;
			let data = request.reader.read_exact(size as usize).await?;
			Ok::<_, Error>((type_id, data))
		});
		let mut timeout = crate::runtime::Deadline::after(&self.runtime, ADVERTISE_TIMEOUT);

		kio::wait(|waiter| {
			if let Poll::Ready(res) = waiter.poll_future(read.as_mut()) {
				return Poll::Ready(res.map(Some));
			}
			if timeout.poll(waiter).is_ready() {
				return Poll::Ready(Ok(None));
			}
			Poll::Pending
		})
		.await
	}

	/// Withdraw an advertised namespace: NAMESPACE_DONE inline, or PUBLISH_NAMESPACE_DONE
	/// closing the request that carried it.
	async fn withdraw_namespace(
		&self,
		target: &mut Target<S>,
		requests: &mut HashMap<crate::PathOwned, NamespaceRequest<S>>,
		suffix: crate::PathOwned,
	) -> Result<(), Error> {
		match target {
			Target::Requests(_) => {
				if let Some(mut request) = requests.remove(&suffix) {
					// Draft-17+ removed PUBLISH_NAMESPACE_DONE: the FIN below is the whole
					// withdrawal. Sending it anyway puts the type on the wire before the
					// body fails to encode, and a receiver reading 0x09 there has no choice
					// but to treat it as a protocol violation (see
					// `Subscriber::terminal_publish_namespace`).
					if matches!(self.version, Version::Draft14 | Version::Draft15 | Version::Draft16) {
						// Best effort: the peer may already be gone.
						let _ = request
							.stream
							.writer
							.encode_message(&ietf::PublishNamespaceDone {
								track_namespace: request.path.as_path(),
								request_id: request.request_id,
							})
							.await;
					}

					// The withdrawal rides this request's own stream, which drops with it, so
					// it needs the acknowledgement before the drop-time reset can discard it.
					let _ = request.stream.writer.close().await;
				}
			}
			Target::Inline(stream) => {
				stream.writer.encode(&ietf::NamespaceDone::ID).await?;
				stream
					.writer
					.encode(&ietf::NamespaceDone {
						suffix: suffix.as_path(),
					})
					.await?;
			}
		}
		Ok(())
	}

	/// Close out every open PUBLISH_NAMESPACE request. A no-op for a loop whose entries
	/// ride the SUBSCRIBE_NAMESPACE stream itself, which retracts them by ending.
	async fn withdraw_requests(
		&self,
		target: &mut Target<S>,
		requests: &mut HashMap<crate::PathOwned, NamespaceRequest<S>>,
	) {
		let suffixes: Vec<crate::PathOwned> = requests.keys().cloned().collect();
		for suffix in suffixes {
			let _ = self.withdraw_namespace(target, requests, suffix).await;
		}
	}

	/// Advertise every namespace we can, without waiting to be asked.
	///
	/// moq-transport itself says nothing about which of the two discovery messages a peer
	/// expects, and the peers that never send SUBSCRIBE_NAMESPACE are exactly the ones
	/// expecting a publisher to announce itself, so the default has to be to announce. A
	/// peer that would rather ask says so with the MoQ Solicit extension
	/// ([`solicit`](super::solicit)),
	/// and then this loop does nothing and
	/// [`Self::run_subscribe_namespace_stream`] carries the advertisements instead.
	/// Exactly one of the two is live, which is what keeps the peer from hearing a
	/// namespace twice.
	pub async fn run_publish_namespaces(self) -> Result<(), Error> {
		if self.requires_solicitation().await {
			return Ok(());
		}

		// The cluster extension changes what an advertisement carries, so nothing can be
		// sent until the peer's SETUP says whether it speaks it.
		let peer = self.peer().await;

		// Split horizon, as the solicited loop applies it: never advertise a route back
		// to the peer it came from.
		let origin = match self.exclude(&peer) {
			crate::Origin::UNKNOWN => self.origin.clone(),
			exclude => self.origin.clone().excluding(exclude),
		};

		let ns = Namespaces::new(peer, Target::Requests(None));
		self.run_namespaces(origin, crate::Path::empty().to_owned(), ns).await
	}

	/// Handle a SUBSCRIBE_NAMESPACE on its bidi stream.
	///
	/// All the announce state is local to this task (mirroring `lite::Publisher`'s
	/// announce handling): whatever this subscription advertised is withdrawn
	/// when its stream ends. It only advertises anything when the peer asked to be told
	/// on request; otherwise [`Self::run_publish_namespaces`] has already said it all.
	async fn run_subscribe_namespace_stream(
		self,
		mut stream: Stream<S, Version>,
		msg: ietf::SubscribeNamespace<'_>,
	) -> Result<(), Error> {
		let prefix = msg.namespace.to_owned();

		tracing::debug!(prefix = %self.origin.absolute(&prefix), "subscribe_namespace stream");

		// A prefix outside our scope (empty origin, or a token that doesn't grant it)
		// just means we have nothing to announce; respond with an empty set rather than
		// erroring, which would look fatal to the peer.
		let origin = self
			.origin
			.scope(&[prefix.as_path()])
			.unwrap_or_else(|| self.origin.empty());

		// Send OK response
		match self.version {
			Version::Draft14 => {
				stream.writer.encode(&ietf::SubscribeNamespaceOk::ID).await?;
				stream
					.writer
					.encode(&ietf::SubscribeNamespaceOk {
						request_id: msg.request_id,
					})
					.await?;
			}
			Version::Draft15 | Version::Draft16 => {
				stream.writer.encode(&ietf::RequestOk::ID).await?;
				stream
					.writer
					.encode(&ietf::RequestOk {
						request_id: Some(msg.request_id),
					})
					.await?;
			}
			_ => {
				stream.writer.encode(&ietf::RequestOk::ID).await?;
				stream.writer.encode(&ietf::RequestOk { request_id: None }).await?;
			}
		}

		// The extension changes what an advertisement carries, so nothing can be
		// sent until the peer's SETUP says whether it speaks it.
		let peer = self.peer().await;
		// Register the split-horizon peer on the announce cursor too. The origin
		// model uses this exposure to park a reflected copy before it can replace
		// the source we are currently advertising to that peer.
		let origin = match self.exclude(&peer) {
			crate::Origin::UNKNOWN => origin,
			exclude => origin.excluding(exclude),
		};

		// Draft-14/15 predate NAMESPACE, so they answer with their own PUBLISH_NAMESPACE
		// requests and keep this stream open for the subscription's lifetime.
		let target = match self.version {
			Version::Draft14 | Version::Draft15 => Target::Requests(Some(stream)),
			_ => Target::Inline(stream),
		};

		// Unless the peer asked to be told only on request, it has already heard all of
		// this as unsolicited PUBLISH_NAMESPACE. Repeating it here would leave it holding
		// two sources for one namespace, so this stream carries nothing and simply stays
		// open until the peer is done with it.
		let origin = match self.requires_solicitation().await {
			true => origin,
			false => origin.empty(),
		};

		let ns = Namespaces::new(peer, target);
		self.run_namespaces(origin, prefix, ns).await
	}

	/// Forward origin (un)announces to the peer until the loop ends.
	///
	/// Shared by both announce paths: they differ in where the advertisements go
	/// ([`Target`]) and where the origin is rooted (`prefix`, empty when nothing asked
	/// for a subset).
	async fn run_namespaces(
		&self,
		origin: origin::Consumer,
		prefix: crate::PathOwned,
		mut ns: Namespaces<S>,
	) -> Result<(), Error> {
		let mut announced = origin.announced();

		let mut linger = crate::runtime::Deadline::new(&self.runtime);

		// When to re-offer whatever the peer should hold and doesn't, and how long to wait
		// the next time that fails. Jittered so a relay's namespaces don't all come back on
		// the same tick.
		let mut retry = crate::runtime::Deadline::new(&self.runtime);
		let mut retry_at: Option<crate::runtime::Instant> = None;
		let mut retry_delay = RETRY_BASE;

		// Stream updates (origin (un)announces plus watched route and demand
		// changes), bailing if the peer closes its side first.
		let res = loop {
			linger.set(Self::linger_deadline(&ns.watched));

			match ns.watched.values().any(|watch| watch.deferred) {
				// Arm on the edge, so a turn that changes nothing else doesn't push the
				// deadline out forever.
				true => retry_at = retry_at.or_else(|| Some(self.runtime.now() + jitter(retry_delay))),
				false => {
					retry_at = None;
					retry_delay = RETRY_BASE;
				}
			}
			retry.set(retry_at);

			let event = {
				// Split the borrow so the close watch and the route sweep can both run in
				// the same turn.
				let Namespaces { target, watched, .. } = &mut ns;
				kio::wait(|waiter| {
					let mut cx = std::task::Context::from_waker(waiter.waker());
					if let Poll::Ready(res) = target.poll_closed(&mut cx) {
						return Poll::Ready(NamespaceEvent::Closed(res));
					}
					if let Poll::Ready(update) = announced.poll_next(waiter) {
						return Poll::Ready(NamespaceEvent::Update(update));
					}
					if retry.poll(waiter).is_ready() {
						return Poll::Ready(NamespaceEvent::Retry);
					}
					// Stamped per poll rather than kept: the turn always ends in a
					// `Ready` below once it fires, so it never has to survive.
					let fired = linger.poll(waiter).is_ready().then(|| self.runtime.now());
					match Self::poll_watched(watched, fired, waiter) {
						Poll::Ready(Watch::Changed(path)) => return Poll::Ready(NamespaceEvent::Routes(path)),
						Poll::Ready(Watch::Idle(path)) => return Poll::Ready(NamespaceEvent::Idle(path)),
						Poll::Pending => {}
					}
					match fired.is_some() {
						true => Poll::Ready(NamespaceEvent::Linger),
						false => Poll::Pending,
					}
				})
				.await
			};

			match event {
				NamespaceEvent::Closed(res) => break res,
				NamespaceEvent::Linger => continue,
				NamespaceEvent::Retry => {
					retry_at = None;
					retry_delay = (retry_delay * 2).min(RETRY_MAX);

					// A minimum wait the peer named is enforced by `sync_namespace`, which
					// every path goes through, so a namespace still inside one simply
					// makes no offer this turn. The next sweep is at most RETRY_MAX away.
					let deferred: Vec<crate::PathOwned> = ns
						.watched
						.iter()
						.filter(|(_, watch)| watch.deferred)
						.map(|(suffix, _)| suffix.clone())
						.collect();

					for suffix in deferred {
						let path = prefix.join(&suffix);
						self.sync_namespace(&mut ns, &suffix, &path).await?;
					}
				}
				NamespaceEvent::Update(None) => {
					// The origin is gone: withdraw everything, then finish the
					// stream and wait for delivery.
					self.withdraw_requests(&mut ns.target, &mut ns.requests).await;
					let Some(stream) = ns.target.stream() else {
						return Ok(());
					};
					stream.writer.finish()?;
					return stream.writer.closed().await;
				}
				NamespaceEvent::Update(Some(crate::announce::Update { path, broadcast })) => {
					let suffix = path
						.strip_prefix(&prefix)
						.expect("origin returned invalid path")
						.to_owned();
					let path = path.to_owned();

					match broadcast {
						Some(broadcast) => {
							ns.watched.insert(suffix.clone(), Watched::new(broadcast));
							self.sync_namespace(&mut ns, &suffix, &path).await?;
						}
						None => {
							// Only close out namespaces the peer actually saw.
							let held = ns.watched.remove(&suffix).is_some_and(|watch| watch.sent.wanted());
							if held {
								tracing::debug!(broadcast = %self.origin.absolute(&path), "namespace_done");
								self.withdraw_namespace(&mut ns.target, &mut ns.requests, suffix)
									.await?;
							}
						}
					}
				}
				NamespaceEvent::Routes(suffix) => {
					let path = prefix.join(&suffix);
					self.sync_namespace(&mut ns, &suffix, &path).await?;
				}
				NamespaceEvent::Idle(suffix) => {
					if let Some(watch) = ns.watched.get_mut(&suffix) {
						watch.idle_at = Some(self.runtime.now());
					}
				}
			}
		};

		// This loop's advertisements die with it.
		self.withdraw_requests(&mut ns.target, &mut ns.requests).await;

		res
	}
}

/// Serves a track's groups, one machine per group, with unlimited concurrency.
struct TrackServe<S: crate::transport::poll::Session> {
	session: S,
	track: track::Subscriber,
	request_id: RequestId,
	version: Version,
	children: kio::Tasks<crate::util::MaybeSendTask>,
	/// The track finished: the in-flight group machines drain, then FIN.
	draining: bool,
}

impl<S: crate::transport::poll::Session> TrackServe<S> {
	fn new(session: S, mut track: track::Subscriber, request_id: RequestId, version: Version) -> Self {
		// A fresh cursor starts at the oldest cached group, so leaving it there replays the
		// whole retained history at once, one concurrent stream per group. LargestObject is
		// the only filter we accept and it means the live edge.
		if let Some(latest) = track.latest() {
			track.start_at(latest);
		}

		Self {
			session,
			track,
			request_id,
			version,
			children: kio::Tasks::new(),
			draining: false,
		}
	}

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		if self.draining {
			return self.children.poll(waiter).map(Ok);
		}

		let _ = self.children.poll(waiter);
		loop {
			match self.track.poll_recv_group(waiter) {
				Poll::Ready(Ok(Some(group))) => {
					let sequence = group.sequence;
					tracing::debug!(subscribe = %self.request_id, track = %self.track.name(), sequence, "serving group");

					let msg = ietf::GroupHeader {
						track_alias: self.request_id.0,
						group_id: sequence,
						sub_group_id: 0,
						publisher_priority: 0,
						// Carry per-object timestamps as extension headers (the Timestamp
						// Object Property) so moq-transport peers get the real PTS. The
						// units are the track's, declared once in SUBSCRIBE_OK.
						flags: ietf::GroupFlags {
							has_extensions: true,
							..Default::default()
						},
					};

					let mut serve = GroupServe {
						session: self.session.clone(),
						msg,
						priority: self.track.subscription().priority,
						group,
						timescale: self.track.info().timescale,
						version: self.version,
						state: GroupState::Open,
					};
					self.children
						.push(crate::util::poll_task(move |waiter| serve.poll(waiter)));
				}
				Poll::Ready(Ok(None)) => {
					self.draining = true;
					return self.children.poll(waiter).map(Ok);
				}
				Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
				Poll::Pending => break,
			}
		}
		// Newly created group machines start now rather than on the next wake.
		let _ = self.children.poll(waiter);
		Poll::Pending
	}
}

/// Serves one group on its own unidirectional stream in the moq-transport
/// subgroup format.
struct GroupServe<S: crate::transport::poll::Session> {
	session: S,
	msg: ietf::GroupHeader,
	priority: u8,
	group: group::Consumer,
	timescale: Timescale,
	version: Version,
	state: GroupState<S>,
}

// A state machine's enum is its storage: one transient instance per stream, so the
// big variant is the working state, not padding held in bulk.
#[allow(clippy::large_enum_variant)]
enum GroupState<S: crate::transport::poll::Session> {
	/// Waiting for stream credit on this machine's own session handle.
	Open,
	/// Streaming objects: the write buffer drains first, then the pending chunk,
	/// then the pending frame, then the next frame.
	Serve {
		writer: Writer<S::SendStream, Version>,
		frame: Option<frame::Consumer>,
		chunk: Option<bytes::Bytes>,
	},
	/// Every frame is written and the FIN sent: wait for the acknowledgement so a
	/// late cancel can still reset the stream.
	Closed {
		writer: Writer<S::SendStream, Version>,
	},
	Done,
}

impl<S: crate::transport::poll::Session> GroupServe<S> {
	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		// Errors just drop the writer, whose Drop resets the stream, exactly like
		// the old future being discarded.
		ready!(self.poll_serve(waiter)).map(|()| ()).unwrap_or(());
		Poll::Ready(())
	}
}

impl<S: crate::transport::poll::Session> GroupServe<S> {
	fn poll_serve(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		let mut cx = std::task::Context::from_waker(waiter.waker());
		loop {
			match &mut self.state {
				GroupState::Open => {
					if self.group.poll_expired(waiter) {
						self.state = GroupState::Done;
						return Poll::Ready(Err(Error::Old));
					}
					let stream = match ready!(self.session.poll_open_uni(&mut cx)) {
						Ok(stream) => stream,
						Err(err) => {
							self.state = GroupState::Done;
							return Poll::Ready(Err(Error::from_transport(err)));
						}
					};
					let mut stream = stream;
					stream.set_priority(self.priority);

					let mut writer = Writer::new(stream, self.version);
					if let Err(err) = writer.buffer(&self.msg) {
						self.state = GroupState::Done;
						return Poll::Ready(Err(err));
					}
					self.state = GroupState::Serve {
						writer,
						frame: None,
						chunk: None,
					};
				}
				GroupState::Serve { writer, frame, chunk } => {
					// The peer closing first cancels the group.
					if writer.poll_closed(&mut cx).is_ready() {
						self.state = GroupState::Done;
						return Poll::Ready(Err(Error::Cancel));
					}
					let res = 'serve: {
						loop {
							match writer.poll_flush(&mut cx) {
								Poll::Ready(Ok(())) => {}
								Poll::Ready(Err(err)) => break 'serve Err(err),
								// Parking on the transport is the one stall the group cursor cannot
								// see, and the only place a served group applies the drift budget:
								// flow control must not pin a stream that has gone stale. `true`
								// because the transport still owns bytes the cursor has released.
								Poll::Pending => {
									if self.group.poll_expired_while_pending(waiter, true) {
										break 'serve Err(Error::Old);
									}
									return Poll::Pending;
								}
							}
							if let Some(pending) = chunk {
								match writer.poll_write(&mut cx, pending) {
									Poll::Ready(Ok(_)) => {
										if !bytes::Buf::has_remaining(pending) {
											*chunk = None;
										}
									}
									Poll::Ready(Err(err)) => break 'serve Err(err),
									// Parking on the transport is the one stall the group cursor cannot
									// see, and the only place a served group applies the drift budget:
									// flow control must not pin a stream that has gone stale. `true`
									// because the transport still owns bytes the cursor has released.
									Poll::Pending => {
										if self.group.poll_expired_while_pending(waiter, true) {
											break 'serve Err(Error::Old);
										}
										return Poll::Pending;
									}
								}
							} else if let Some(pending) = frame {
								match pending.poll_read_chunk(waiter) {
									Poll::Ready(Ok(Some(next))) => *chunk = Some(next),
									Poll::Ready(Ok(None)) => *frame = None,
									Poll::Ready(Err(err)) => break 'serve Err(err),
									Poll::Pending => return Poll::Pending,
								}
							} else {
								match self.group.poll_next_frame(waiter) {
									Poll::Ready(Ok(Some(next))) => {
										if let Err(err) = buffer_object(writer, &next, self.timescale, self.version) {
											break 'serve Err(err);
										}
										// An empty object has no payload to stream.
										if next.size > 0 {
											*frame = Some(next);
										}
									}
									Poll::Ready(Ok(None)) => break 'serve Ok(()),
									Poll::Ready(Err(err)) => break 'serve Err(err),
									Poll::Pending => return Poll::Pending,
								}
							}
						}
					};

					let GroupState::Serve { writer, .. } = std::mem::replace(&mut self.state, GroupState::Done) else {
						unreachable!()
					};
					match res {
						Ok(()) => {
							let mut writer = writer;
							match writer.finish() {
								Ok(()) => self.state = GroupState::Closed { writer },
								Err(err) => return Poll::Ready(Err(err)),
							}
						}
						Err(err) => return Poll::Ready(Err(err)),
					}
				}
				GroupState::Closed { writer } => {
					// Wait until everything is acknowledged by the peer so we can still
					// cancel the stream. poll_close releases the stream on completion so
					// the Drop fallback cannot reset the acknowledged stream.
					let res = ready!(writer.poll_close(&mut cx));
					let sequence = self.msg.group_id;
					self.state = GroupState::Done;
					return Poll::Ready(res.map(|()| {
						tracing::debug!(sequence, "finished group");
					}));
				}
				GroupState::Done => return Poll::Ready(Ok(())),
			}
		}
	}
}

/// Buffer one object's header and prefix: the id delta, the extension headers
/// carrying the timestamp, the size, and (for an empty object) the status.
fn buffer_object<W: crate::transport::poll::SendStream>(
	writer: &mut Writer<W, Version>,
	frame: &frame::Consumer,
	timescale: Timescale,
	version: Version,
) -> Result<(), Error> {
	// object id delta is always 0.
	writer.buffer(&0u64)?;

	// Per-object extension headers carry the frame's presentation timestamp.
	let mut ext = bytes::BytesMut::new();
	ietf::encode_object_time(&mut ext, frame.timestamp, timescale, version)?;
	writer.buffer(&(ext.len() as u64))?;
	writer.buffer_raw(&ext);

	writer.buffer(&frame.size)?;
	if frame.size == 0 {
		// Have to write the object status too.
		writer.buffer(&0u8)?;
	}
	Ok(())
}

/// One draft-14/15 advertisement: the PUBLISH_NAMESPACE request it rode on and
/// what closes it out with PUBLISH_NAMESPACE_DONE.
struct NamespaceRequest<S: crate::transport::poll::Session> {
	path: crate::PathOwned,
	request_id: RequestId,
	stream: Stream<S, Version>,
}

#[cfg(test)]
mod group_priority_test {
	use super::*;
	use crate::lite::test_transport::SinkSession;

	/// The model's `Subscription::priority` is higher-first ("higher values preempt
	/// lower ones"), matching the transport trait's send order, so a group stream must
	/// receive the model value unchanged. An inversion here would transmit the
	/// LOWEST-priority track first under contention.
	#[tokio::test]
	async fn group_stream_preserves_model_priority() {
		let log = crate::lite::test_transport::Log::default();
		let session = SinkSession::new(log.clone());

		let mut track = track::Producer::new(std::sync::Arc::new(crate::broadcast::Info::default()), "test", None);
		let mut group = track.create_group(group::Info { sequence: 0 }).unwrap();
		group
			.write_frame(crate::Timestamp::from_millis(0).unwrap(), b"hello".as_slice())
			.unwrap();
		let consumer = group.consume();
		group.finish().unwrap();

		let msg = ietf::GroupHeader {
			track_alias: 0,
			group_id: 0,
			sub_group_id: 0,
			publisher_priority: 0,
			flags: Default::default(),
		};

		let mut serve = GroupServe {
			session,
			msg,
			priority: 200,
			group: consumer,
			timescale: Timescale::default(),
			version: Version::Draft14,
			state: GroupState::Open,
		};
		kio::wait(|waiter| serve.poll_serve(waiter)).await.unwrap();

		assert_eq!(
			log.priorities(),
			vec![200],
			"model priority must pass through unchanged"
		);
	}

	/// A subgroup waiting for stream credit keeps its subscription expiry armed.
	#[tokio::test]
	async fn group_waiting_for_stream_credit_expires() {
		tokio::time::pause();

		let gate = kio::Producer::new(false);
		let session = SinkSession::gated_open_uni(gate.consume());
		let mut track = track::Producer::new(std::sync::Arc::new(crate::broadcast::Info::default()), "test", None);
		let mut subscriber = track.subscribe(None);
		let mut old = track.append_group().unwrap();
		old.write_frame(crate::Timestamp::ZERO, b"old".as_slice()).unwrap();
		old.finish().unwrap();
		let group = subscriber.recv_group().await.unwrap().expect("old group");

		let mut serve = GroupServe {
			session,
			msg: ietf::GroupHeader {
				track_alias: 0,
				group_id: 0,
				sub_group_id: 0,
				publisher_priority: 0,
				flags: Default::default(),
			},
			priority: 0,
			group,
			timescale: Timescale::default(),
			version: Version::Draft19,
			state: GroupState::Open,
		};
		let mut serving = std::pin::pin!(kio::wait(|waiter| serve.poll_serve(waiter)));
		assert!(
			futures::poll!(serving.as_mut()).is_pending(),
			"stream credit is exhausted"
		);

		tokio::time::advance(Duration::from_secs(1)).await;
		let mut edge = track.append_group().unwrap();
		edge.write_frame(crate::Timestamp::from_millis(1000).unwrap(), b"edge".as_slice())
			.unwrap();
		edge.finish().unwrap();

		assert!(matches!(serving.await, Err(Error::Old)));
	}

	/// The final payload remains guarded after its frame has advanced the group cursor.
	#[tokio::test]
	async fn blocked_final_transport_chunk_expires_with_the_group() {
		tokio::time::pause();

		let gate = kio::Producer::new(true);
		let session = SinkSession::gated_uni(gate.consume());
		let mut track = track::Producer::new(std::sync::Arc::new(crate::broadcast::Info::default()), "test", None);
		let mut subscriber = track.subscribe(None);
		let mut old = track.append_group().unwrap();
		let mut frame = old
			.create_frame(frame::Info {
				timestamp: crate::Timestamp::ZERO,
				size: 2,
			})
			.unwrap();
		frame.write(b"a".as_slice()).unwrap();
		let group = subscriber.recv_group().await.unwrap().expect("old group");
		let mut serve = GroupServe {
			session,
			msg: ietf::GroupHeader {
				track_alias: 0,
				group_id: 0,
				sub_group_id: 0,
				publisher_priority: 0,
				flags: Default::default(),
			},
			priority: 0,
			group,
			timescale: Timescale::default(),
			version: Version::Draft19,
			state: GroupState::Open,
		};
		let mut serving = std::pin::pin!(kio::wait(|waiter| serve.poll_serve(waiter)));
		assert!(
			futures::poll!(serving.as_mut()).is_pending(),
			"waiting for the final byte"
		);

		let Ok(mut open) = gate.write() else {
			panic!("transport gate closed");
		};
		*open = false;
		drop(open);
		frame.write(b"b".as_slice()).unwrap();
		frame.finish().unwrap();
		old.finish().unwrap();
		assert!(
			futures::poll!(serving.as_mut()).is_pending(),
			"the final byte is transport-blocked"
		);

		tokio::time::advance(Duration::from_secs(1)).await;
		let mut edge = track.append_group().unwrap();
		edge.write_frame(crate::Timestamp::from_millis(1000).unwrap(), b"edge".as_slice())
			.unwrap();
		edge.finish().unwrap();

		assert!(matches!(serving.await, Err(Error::Old)));
	}
}

#[cfg(test)]
mod subscribe_cursor_test {
	use super::*;
	use crate::lite::test_transport::{Log, SinkSession};

	/// A subscription's cursor starts at the oldest cached group, so serving it verbatim
	/// replays every retained group at once, each on its own stream. Relays reject the burst
	/// and players skip straight back to the live edge, so the catch-up is pure waste.
	#[tokio::test]
	async fn a_subscribe_is_served_from_the_live_edge() {
		let log = Log::default();
		let session = SinkSession::new(log.clone());

		let mut track = track::Producer::new(std::sync::Arc::new(crate::broadcast::Info::default()), "video", None);
		for sequence in 0..4 {
			let mut group = track.create_group(group::Info { sequence }).unwrap();
			group
				.write_frame(crate::Timestamp::from_millis(0).unwrap(), b"frame".as_slice())
				.unwrap();
			group.finish().unwrap();
		}

		let subscriber = track.subscribe(None);
		track.finish().unwrap();

		let mut serve = TrackServe::new(session, subscriber, RequestId(1), Version::Draft16);
		kio::wait(|waiter| serve.poll(waiter)).await.unwrap();

		// `GroupServe` sets the priority once per stream it opens, so this counts groups served.
		assert_eq!(log.priorities().len(), 1, "only group 3 should have been served");
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::lite::test_transport::SinkSession;
	use crate::model::ProduceTest;
	use crate::runtime::Timers as _;
	use futures::FutureExt;

	/// The tokio-backed test runtime. Its transport parameter is phantom, so one
	/// type serves every fake session in this module.
	type TestRuntime = crate::runtime::tokio_test::Tokio<SinkSession>;

	async fn settle() {
		tokio::time::sleep(Duration::from_millis(1)).await;
	}

	fn occurrences(log: &crate::lite::test_transport::Log, needle: &[u8]) -> usize {
		let writes = log.writes.lock().unwrap();
		writes.windows(needle.len()).filter(|window| *window == needle).count()
	}

	/// A SETUP slot already filled with what the peer declared. The announce loops block
	/// on it, so a test that leaves it empty is a test that never advertises.
	fn declared(solicit: Option<bool>) -> peer::PeerSetup {
		let slot = peer::PeerSetup::default();
		slot.set(peer::Peer {
			solicit,
			..Default::default()
		});
		slot
	}

	/// moq-transport cannot carry the receiver's max age budget, so the serving
	/// subscription must preserve everything the producer still retains.
	#[test]
	fn serving_subscription_keeps_retained_backlog() {
		let mut producer = track::Producer::new(std::sync::Arc::new(crate::broadcast::Info::default()), "video", None);
		for millis in [0, 1000] {
			let mut group = producer.append_group().unwrap();
			group
				.write_frame(crate::Timestamp::from_millis(millis).unwrap(), b"frame".as_slice())
				.unwrap();
			group.finish().unwrap();
		}

		let subscription = serving_subscription(128);
		assert_eq!(subscription.max_age.as_millis(), MAX_SAFE_AGE_MS as u128);
		let mut subscriber = producer.subscribe(subscription);
		for sequence in [0, 1] {
			let group = subscriber
				.recv_group()
				.now_or_never()
				.expect("retained group should be ready")
				.unwrap()
				.expect("track should remain open");
			assert_eq!(group.sequence, sequence);
		}
	}

	/// A peer that requires solicitation, which is what hands the advertisements to the
	/// SUBSCRIBE_NAMESPACE stream.
	fn requires_solicitation() -> peer::PeerSetup {
		declared(Some(true))
	}

	/// A broadcast whose every route flows through the peer's assigned identity
	/// (`Client::with_peer_origin`) is never advertised to that peer; it would only
	/// echo the peer's own content back at it. A broadcast with an independent
	/// route still is.
	#[tokio::test]
	async fn assigned_peer_origin_filters_echoed_announces() {
		let assigned = crate::Origin::new(777).unwrap();
		let other = crate::Origin::new(778).unwrap();

		let origin = crate::origin::Info::new(crate::Origin::new(1).unwrap()).produce();
		let consumer = origin.consume();

		let session = crate::lite::test_transport::SinkSession::new(Default::default());
		let publisher = Publisher::new(
			TestRuntime::new(),
			session,
			origin.consume(),
			Control::new(None, false),
			Some(assigned),
			peer::PeerSetup::default(),
			Version::Draft16,
		);

		let mut echoed_hops = crate::OriginList::new();
		echoed_hops.push(assigned).unwrap();
		let _echoed = origin
			.create_broadcast(
				"from/peer",
				crate::broadcast::Route::new()
					.with_hops(echoed_hops)
					.with_announce(true),
			)
			.unwrap();

		let mut local_hops = crate::OriginList::new();
		local_hops.push(other).unwrap();
		let _local = origin
			.create_broadcast(
				"from/us",
				crate::broadcast::Route::new().with_hops(local_hops).with_announce(true),
			)
			.unwrap();

		// Broadcast visibility is deferred until the executor ticks.
		tokio::time::sleep(Duration::from_millis(1)).await;

		let peer = cluster::Peer::default();

		let echoed = consumer.get_broadcast("from/peer").unwrap();
		assert!(!publisher.select(&Watched::new(echoed), &peer).wanted());

		let local = consumer.get_broadcast("from/us").unwrap();
		assert_eq!(publisher.select(&Watched::new(local), &peer), Advert::Plain);
	}

	/// A drained broadcast arms a linger so the cold cost is restored after a grace
	/// period. If the advertisement stops being discounted before that fires, the
	/// linger must go with it: an expired deadline that nothing clears makes
	/// `linger_deadline` hand back the same instant every turn, and the announce loops
	/// spin on it forever.
	#[tokio::test]
	async fn linger_clears_when_the_advert_stops_being_discounted() {
		let origin = crate::origin::Info::new(crate::Origin::new(1).unwrap()).produce();
		let broadcast = origin
			.clone()
			.create_broadcast("cam", crate::broadcast::Route::announced())
			.unwrap();

		let mut watch = Watched::new(broadcast.consume());
		let hops = crate::OriginList::try_from(vec![crate::Origin::new(7).unwrap()]).unwrap();

		// Discounted (cost 0) and drained: the linger is running.
		watch.set_sent(Advert::Cluster(cluster::Advert {
			hops: cluster::HopPath::new(hops.clone()),
			cost: 0,
		}));
		watch.idle_at = Some(TestRuntime::new().now());
		let watched = HashMap::from([(crate::Path::new("cam").to_owned(), watch)]);
		assert!(Publisher::<SinkSession, TestRuntime>::linger_deadline(&watched).is_some());

		// A re-priced advertisement has a cost to advertise, so there is nothing left
		// to restore.
		let mut watch = watched.into_values().next().unwrap();
		watch.set_sent(Advert::Cluster(cluster::Advert {
			hops: cluster::HopPath::new(hops),
			cost: 9,
		}));
		let watched = HashMap::from([(crate::Path::new("cam").to_owned(), watch)]);
		assert_eq!(
			Publisher::<SinkSession, TestRuntime>::linger_deadline(&watched),
			None,
			"a non-discounted advert must not leave a deadline behind"
		);

		// So does one that stopped being advertisable at all.
		let mut watch = watched.into_values().next().unwrap();
		watch.idle_at = Some(TestRuntime::new().now());
		watch.set_sent(Advert::None);
		let watched = HashMap::from([(crate::Path::new("cam").to_owned(), watch)]);
		assert_eq!(Publisher::<SinkSession, TestRuntime>::linger_deadline(&watched), None);
	}

	/// A same-path source can splice into (or detach from) an existing broadcast
	/// without an origin-level (un)announce, silently flipping `advertisable`.
	/// Namespace forwarding must follow: advertise when a clean route appears,
	/// withdraw when the last one detaches.
	#[tokio::test]
	async fn namespace_follows_route_eligibility_changes() {
		let assigned = crate::Origin::new(777).unwrap();
		let clean_publisher = crate::Origin::new(778).unwrap();
		let origin = crate::origin::Info::new(crate::Origin::new(1).unwrap()).produce();

		let gate = kio::Producer::new(true);
		let session = SinkSession::gated_bi(gate.consume());
		let log = session.log.clone();
		let publisher = Publisher::new(
			TestRuntime::new(),
			session.clone(),
			origin.consume(),
			Control::new(None, false),
			Some(assigned),
			requires_solicitation(),
			Version::Draft16,
		);

		// The broadcast starts with only a route through the assigned peer.
		let mut tainted_hops = crate::OriginList::new();
		tainted_hops.push(assigned).unwrap();
		let _tainted = origin
			.create_broadcast(
				"route-flip-cam",
				crate::broadcast::Route::new()
					.with_hops(tainted_hops)
					.with_announce(true),
			)
			.unwrap();
		settle().await;

		let stream = Stream::open(&mut session.clone(), Version::Draft16).await.unwrap();
		let msg = ietf::SubscribeNamespace {
			request_id: RequestId(1),
			namespace: crate::Path::new(""),
		};
		let mut run = std::pin::pin!(publisher.run_subscribe_namespace_stream(stream, msg));

		// Initial set: the tainted-only broadcast is filtered, nothing but the OK
		// response on the wire.
		assert!(futures::poll!(run.as_mut()).is_pending());
		assert_eq!(occurrences(&log, b"route-flip-cam"), 0);

		// A clean source splices in: no origin announce fires, only the route table
		// changes. The namespace must now be advertised.
		let mut clean_hops = crate::OriginList::new();
		clean_hops.push(clean_publisher).unwrap();
		let clean = origin
			.create_broadcast(
				"route-flip-cam",
				crate::broadcast::Route::new().with_hops(clean_hops).with_announce(true),
			)
			.unwrap();
		settle().await;
		assert!(futures::poll!(run.as_mut()).is_pending());
		assert_eq!(
			occurrences(&log, b"route-flip-cam"),
			1,
			"NAMESPACE after a clean route joins"
		);

		// The clean source detaches, leaving only the tainted route: withdrawn.
		drop(clean);
		settle().await;
		assert!(futures::poll!(run.as_mut()).is_pending());
		assert_eq!(
			occurrences(&log, b"route-flip-cam"),
			2,
			"NAMESPACE_DONE after the last clean route detaches"
		);
	}

	/// The peer's OK to a PUBLISH_NAMESPACE, framed exactly as the announce path
	/// reads it -- built with the crate's own writer so the framing can't drift
	/// from the encoder under test.
	/// A REQUEST_ERROR declining an advertisement, with the retry interval the peer asked
	/// for in milliseconds. Zero means it does not want the namespace offered again.
	async fn publish_namespace_error(version: Version, retry_interval: u64) -> Vec<u8> {
		let log = crate::lite::test_transport::Log::default();
		let mut writer = crate::coding::Writer::new(crate::lite::test_transport::SinkSend::new(log.clone()), version);

		writer.encode(&ietf::RequestError::ID).await.unwrap();
		writer
			.encode(&ietf::RequestError {
				request_id: matches!(version, Version::Draft15 | Version::Draft16).then_some(RequestId(1)),
				error_code: 403,
				reason_phrase: "no".into(),
				retry_interval,
			})
			.await
			.unwrap();

		log.writes.lock().unwrap().clone()
	}

	/// A peer that refuses an advertisement with a retry interval of 0 is asking not to be
	/// offered it again. Coming back anyway turns a permanent refusal (unauthorized,
	/// uninterested) into a request every few seconds for the life of the session.
	#[tokio::test(start_paused = true)]
	async fn a_refusal_that_forbids_retrying_is_not_retried() {
		const VERSION: Version = Version::Draft17;

		let origin = crate::origin::Info::new(crate::Origin::new(1).unwrap()).produce();
		let _cam = origin
			.create_broadcast("lonely-cam", crate::broadcast::Route::announced())
			.unwrap();
		settle().await;

		// Every stream is answered with the same refusal, so a retry would show up as a
		// second occurrence on the wire.
		let refusal = publish_namespace_error(VERSION, 0).await;
		let session =
			crate::lite::test_transport::ScriptedSession::per_stream(vec![refusal.clone(), refusal.clone(), refusal]);
		let log = session.log.clone();

		let publisher = Publisher::new(
			TestRuntime::new(),
			session,
			origin.consume(),
			Control::new(None, false),
			None,
			declared(Some(false)),
			VERSION,
		);

		let mut run = std::pin::pin!(publisher.run_publish_namespaces());
		for _ in 0..100 {
			assert!(futures::poll!(run.as_mut()).is_pending());
			if occurrences(&log, b"lonely-cam") > 0 {
				break;
			}
			settle().await;
		}
		assert_eq!(occurrences(&log, b"lonely-cam"), 1, "the advertisement never went out");

		// Well past every retry the loop would otherwise take.
		for _ in 0..100 {
			assert!(futures::poll!(run.as_mut()).is_pending());
			tick().await;
		}

		assert_eq!(
			occurrences(&log, b"lonely-cam"),
			1,
			"re-offered a namespace the peer asked not to be offered again"
		);
	}

	async fn publish_namespace_ok(version: Version) -> Vec<u8> {
		let log = crate::lite::test_transport::Log::default();
		let mut writer = crate::coding::Writer::new(crate::lite::test_transport::SinkSend::new(log.clone()), version);

		match version {
			Version::Draft14 => {
				writer.encode(&ietf::PublishNamespaceOk::ID).await.unwrap();
				writer
					.encode(&ietf::PublishNamespaceOk {
						request_id: RequestId(1),
					})
					.await
					.unwrap();
			}
			Version::Draft15 | Version::Draft16 => {
				writer.encode(&ietf::RequestOk::ID).await.unwrap();
				writer
					.encode(&ietf::RequestOk {
						request_id: Some(RequestId(1)),
					})
					.await
					.unwrap();
			}
			// Draft-17+ dropped the request id: the response rides the request's stream.
			_ => {
				writer.encode(&ietf::RequestOk::ID).await.unwrap();
				writer.encode(&ietf::RequestOk { request_id: None }).await.unwrap();
			}
		}

		let writes = log.writes.lock().unwrap();
		writes.clone()
	}

	/// Draft-14/15 predate the NAMESPACE message, so a SUBSCRIBE_NAMESPACE is
	/// answered with one PUBLISH_NAMESPACE request per matching namespace over the
	/// control stream, and PUBLISH_NAMESPACE_DONE withdraws it. The state is local
	/// to the subscription's task, mirroring lite's announce handling.
	#[tokio::test]
	async fn v14_subscribe_namespace_is_answered_with_publish_namespace() {
		const VERSION: Version = Version::Draft14;

		let origin = crate::origin::Info::new(crate::Origin::new(1).unwrap()).produce();
		let consumer = origin.consume();

		// Announced before the peer subscribes: it must only hit the wire after.
		let early = origin
			.create_broadcast("early-cam", crate::broadcast::Route::announced())
			.unwrap();
		settle().await;

		// Stream 1 is the peer's SUBSCRIBE_NAMESPACE (the peer stays quiet after);
		// streams 2 and 3 answer our two PUBLISH_NAMESPACE requests.
		let ok = publish_namespace_ok(VERSION).await;
		let session = crate::lite::test_transport::ScriptedSession::per_stream(vec![Vec::new(), ok.clone(), ok]);
		let log = session.log.clone();

		let publisher = Publisher::new(
			TestRuntime::new(),
			session.clone(),
			consumer,
			Control::new(None, false),
			None,
			requires_solicitation(),
			VERSION,
		);

		let stream = Stream::open(&mut session.clone(), VERSION).await.unwrap();
		let msg = ietf::SubscribeNamespace {
			request_id: RequestId(1),
			namespace: crate::Path::new(""),
		};
		let mut run = std::pin::pin!(publisher.run_subscribe_namespace_stream(stream, msg));

		// The subscription solicits the already-announced namespace.
		for _ in 0..100 {
			assert!(futures::poll!(run.as_mut()).is_pending());
			if occurrences(&log, b"early-cam") >= 1 {
				break;
			}
			settle().await;
		}
		assert_eq!(
			occurrences(&log, b"early-cam"),
			1,
			"PUBLISH_NAMESPACE after subscribing"
		);

		// A later announce reaches the same subscription.
		let _late = origin
			.create_broadcast("late-cam", crate::broadcast::Route::announced())
			.unwrap();
		for _ in 0..100 {
			assert!(futures::poll!(run.as_mut()).is_pending());
			if occurrences(&log, b"late-cam") >= 1 {
				break;
			}
			settle().await;
		}
		assert_eq!(
			occurrences(&log, b"late-cam"),
			1,
			"PUBLISH_NAMESPACE for a live announce"
		);

		// An unannounce closes out its own request with PUBLISH_NAMESPACE_DONE.
		drop(early);
		for _ in 0..100 {
			assert!(futures::poll!(run.as_mut()).is_pending());
			if occurrences(&log, b"early-cam") >= 2 {
				break;
			}
			settle().await;
		}
		assert_eq!(
			occurrences(&log, b"early-cam"),
			2,
			"PUBLISH_NAMESPACE_DONE on unannounce"
		);

		// One stream for the subscription itself, one per PUBLISH_NAMESPACE: the
		// withdrawal rode the announce's own request, not a new stream.
		assert_eq!(log.bi_opens(), 3, "no extra stream for the withdrawal");
	}

	/// A peer that declared nothing is told without being asked. Relays that never send
	/// SUBSCRIBE_NAMESPACE hear nothing otherwise, and every third-party one behaves
	/// that way: a publisher is expected to announce itself.
	#[tokio::test]
	async fn a_peer_that_declared_nothing_is_told_unsolicited() {
		const VERSION: Version = Version::Draft17;

		let origin = crate::origin::Info::new(crate::Origin::new(1).unwrap()).produce();
		let _local = origin
			.create_broadcast("local-cam", crate::broadcast::Route::announced())
			.unwrap();
		settle().await;

		// The only stream is the PUBLISH_NAMESPACE request we open ourselves.
		let session =
			crate::lite::test_transport::ScriptedSession::per_stream(vec![publish_namespace_ok(VERSION).await]);
		let log = session.log.clone();

		let peer_setup = peer::PeerSetup::default();
		peer_setup.set(peer::Peer::default());

		let publisher = Publisher::new(
			TestRuntime::new(),
			session,
			origin.consume(),
			Control::new(None, false),
			None,
			peer_setup,
			VERSION,
		);

		let mut run = std::pin::pin!(publisher.run_publish_namespaces());
		for _ in 0..100 {
			assert!(futures::poll!(run.as_mut()).is_pending());
			if occurrences(&log, b"local-cam") >= 1 {
				break;
			}
			settle().await;
		}

		assert_eq!(
			occurrences(&log, b"local-cam"),
			1,
			"PUBLISH_NAMESPACE without a SUBSCRIBE_NAMESPACE"
		);
		assert_eq!(log.bi_opens(), 1, "one request stream");
	}

	/// Drive both announce loops at once against a peer that declared `solicit`,
	/// returning how many times the namespace hit the wire and how many bidi streams
	/// were opened. One stream means the entry rode the subscription inline; two means
	/// it went out as its own PUBLISH_NAMESPACE request.
	async fn advertise_both_ways(solicit: Option<bool>) -> (usize, usize) {
		const VERSION: Version = Version::Draft17;

		let origin = crate::origin::Info::new(crate::Origin::new(1).unwrap()).produce();
		let _cam = origin
			.create_broadcast("cam", crate::broadcast::Route::announced())
			.unwrap();
		settle().await;

		// Stream 1 is the peer's SUBSCRIBE_NAMESPACE; stream 2, if opened at all, is our
		// PUBLISH_NAMESPACE request.
		let session = crate::lite::test_transport::ScriptedSession::per_stream(vec![
			Vec::new(),
			publish_namespace_ok(VERSION).await,
		]);
		let log = session.log.clone();

		let publisher = Publisher::new(
			TestRuntime::new(),
			session.clone(),
			origin.consume(),
			Control::new(None, false),
			None,
			declared(solicit),
			VERSION,
		);

		let stream = Stream::open(&mut session.clone(), VERSION).await.unwrap();
		let msg = ietf::SubscribeNamespace {
			request_id: RequestId(1),
			namespace: crate::Path::new(""),
		};
		let mut solicited = std::pin::pin!(publisher.clone().run_subscribe_namespace_stream(stream, msg));
		let mut unsolicited = std::pin::pin!(publisher.run_publish_namespaces());

		// Poll well past the first advertisement, so a second one from the other loop
		// would show up rather than being missed by an early break. The unsolicited loop
		// finishes immediately when the peer requires solicitation, and a completed
		// future must not be polled again.
		let mut quiet = false;
		for _ in 0..100 {
			assert!(futures::poll!(solicited.as_mut()).is_pending());
			if !quiet {
				quiet = futures::poll!(unsolicited.as_mut()).is_ready();
			}
			settle().await;
		}

		(occurrences(&log, b"cam"), log.bi_opens())
	}

	/// The regression that made announces solicited in the first place: a namespace sent
	/// as both PUBLISH_NAMESPACE and NAMESPACE leaves the peer holding two sources for
	/// one broadcast, and whichever arrives second replaces the one the first attached.
	/// The peer's SETUP picks which loop carries it, so the other stays quiet and the
	/// namespace goes out exactly once either way.
	#[tokio::test]
	async fn each_namespace_is_advertised_exactly_once() {
		let (unsolicited, streams) = advertise_both_ways(Some(false)).await;
		assert_eq!(unsolicited, 1, "a peer that required nothing is told once");
		assert_eq!(streams, 2, "on its own PUBLISH_NAMESPACE request");

		let (solicited, streams) = advertise_both_ways(Some(true)).await;
		assert_eq!(solicited, 1, "a peer that asked to be told on request is told once");
		assert_eq!(streams, 1, "inline on the SUBSCRIBE_NAMESPACE stream it asked on");
	}

	/// A peer out of stream credit parks the open. That must not wedge the loop, because
	/// the withdrawals queued behind it are the only thing that frees a slot: an open that
	/// never gives up is a deadlock, not a delay.
	///
	/// Draft-14 so the withdrawal names its namespace on the wire, which is what makes the
	/// loop's progress visible while every open is blocked.
	#[tokio::test(start_paused = true)]
	async fn a_parked_open_still_lets_a_namespace_be_withdrawn() {
		const VERSION: Version = Version::Draft14;

		let origin = crate::origin::Info::new(crate::Origin::new(1).unwrap()).produce();
		let first = origin
			.create_broadcast("first-cam", crate::broadcast::Route::announced())
			.unwrap();
		settle().await;

		// Open: the peer still has credit for the first advertisement, and answers it.
		let gate = kio::Producer::new(true);
		let ok = publish_namespace_ok(VERSION).await;
		let session = crate::lite::test_transport::ScriptedSession::gated_open(vec![ok.clone(), ok], gate.consume());
		let log = session.log.clone();

		let publisher = Publisher::new(
			TestRuntime::new(),
			session,
			origin.consume(),
			Control::new(None, false),
			None,
			declared(Some(false)),
			VERSION,
		);

		let mut run = std::pin::pin!(publisher.run_publish_namespaces());
		for _ in 0..100 {
			assert!(futures::poll!(run.as_mut()).is_pending());
			if occurrences(&log, b"first-cam") > 0 {
				break;
			}
			settle().await;
		}
		assert_eq!(
			occurrences(&log, b"first-cam"),
			1,
			"the first advertisement never went out"
		);

		// Credit runs out, and a second namespace wants a stream we cannot get.
		set_gate(&gate, false);
		let _second = origin
			.create_broadcast("second-cam", crate::broadcast::Route::announced())
			.unwrap();
		settle().await;

		// Retiring the first frees a slot and needs no new stream, so the loop has to reach
		// it despite the open above.
		drop(first);

		for _ in 0..100 {
			assert!(futures::poll!(run.as_mut()).is_pending());
			if occurrences(&log, b"first-cam") >= 2 {
				break;
			}
			tick().await;
		}
		assert_eq!(
			occurrences(&log, b"first-cam"),
			2,
			"PUBLISH_NAMESPACE_DONE never sent: the open wedged the loop"
		);

		// Credit returns, and nothing else about the origin changes.
		set_gate(&gate, true);

		for _ in 0..100 {
			assert!(futures::poll!(run.as_mut()).is_pending());
			if occurrences(&log, b"second-cam") > 0 {
				break;
			}
			tick().await;
		}
		assert_eq!(
			occurrences(&log, b"second-cam"),
			1,
			"never retried once credit returned"
		);
	}

	/// A namespace nobody can advertise any more is not pending, whatever happened before.
	/// `deferred` outliving the want would arm the retry timer forever for a wire message
	/// that can never happen: not a spin, but a session that never sleeps.
	#[tokio::test(start_paused = true)]
	async fn a_namespace_that_stops_being_advertisable_stops_being_deferred() {
		let assigned = crate::Origin::new(777).unwrap();

		let origin = crate::origin::Info::new(crate::Origin::new(1).unwrap()).produce();

		// Every route flows through the peer's own identity, so split horizon says this
		// must never be advertised back to it: `select` wants nothing, which is what the
		// peer already holds.
		let mut hops = crate::OriginList::new();
		hops.push(assigned).unwrap();
		let _echoed = origin
			.create_broadcast(
				"from/peer",
				crate::broadcast::Route::new().with_hops(hops).with_announce(true),
			)
			.unwrap();
		settle().await;

		let session = crate::lite::test_transport::SinkSession::new(Default::default());
		let publisher = Publisher::new(
			TestRuntime::new(),
			session,
			origin.consume(),
			Control::new(None, false),
			Some(assigned),
			declared(Some(false)),
			Version::Draft17,
		);

		// The state a refused or failed offer leaves behind: the peer holds nothing, and
		// the loop is coming back to it on a timer.
		let suffix: crate::PathOwned = crate::Path::new("from/peer").to_owned();
		let broadcast = origin.consume().get_broadcast("from/peer").unwrap();
		let mut watch = Watched::new(broadcast);
		watch.deferred = true;

		let mut ns = Namespaces::new(cluster::Peer::default(), Target::Requests(None));
		ns.watched.insert(suffix.clone(), watch);

		publisher.sync_namespace(&mut ns, &suffix, &suffix).await.unwrap();

		assert!(
			!ns.watched[&suffix].deferred,
			"the retry timer stays armed for a namespace that can never be advertised"
		);
	}

	/// A minimum wait binds every path back to the namespace, not just the retry sweep.
	/// A route change re-prices the advertisement; it does not excuse us from the wait the
	/// peer asked for.
	#[tokio::test(start_paused = true)]
	async fn a_route_change_still_waits_out_a_refusal() {
		const VERSION: Version = Version::Draft17;

		let origin = crate::origin::Info::new(crate::Origin::new(1).unwrap()).produce();
		let cam = origin
			.create_broadcast("solo-cam", crate::broadcast::Route::announced())
			.unwrap();
		settle().await;

		// Refused with a wait far longer than any backoff the loop would take on its own.
		let refusal = publish_namespace_error(VERSION, 600_000).await;
		let session =
			crate::lite::test_transport::ScriptedSession::per_stream(vec![refusal.clone(), refusal.clone(), refusal]);
		let log = session.log.clone();

		let publisher = Publisher::new(
			TestRuntime::new(),
			session,
			origin.consume(),
			Control::new(None, false),
			None,
			declared(Some(false)),
			VERSION,
		);

		let mut run = std::pin::pin!(publisher.run_publish_namespaces());
		for _ in 0..100 {
			assert!(futures::poll!(run.as_mut()).is_pending());
			if occurrences(&log, b"solo-cam") > 0 {
				break;
			}
			settle().await;
		}
		assert_eq!(occurrences(&log, b"solo-cam"), 1, "the advertisement never went out");

		// A second route makes the advertisement worth re-pricing, which is a path back
		// into the reconciliation that does not go through the retry timer.
		let _standby = origin
			.create_broadcast("solo-cam", crate::broadcast::Route::announced())
			.unwrap();

		for _ in 0..100 {
			assert!(futures::poll!(run.as_mut()).is_pending());
			tick().await;
		}

		assert_eq!(
			occurrences(&log, b"solo-cam"),
			1,
			"re-offered inside the wait the peer asked for"
		);

		drop(cam);
	}

	/// Draft-17+ has no PUBLISH_NAMESPACE_DONE, so a withdrawal there is the FIN and
	/// nothing else. Writing the message anyway puts its type on the wire before the body
	/// fails to encode, which the receiver can only read as a protocol violation, so every
	/// unannounce would kill an otherwise healthy session.
	///
	/// Only reachable through the unsolicited loop, which is what this branch made the
	/// default: the solicited path answers inline and never opens a request per namespace.
	#[tokio::test]
	async fn a_modern_withdrawal_is_the_fin_alone() {
		const VERSION: Version = Version::Draft17;

		let origin = crate::origin::Info::new(crate::Origin::new(1).unwrap()).produce();
		let cam = origin
			.create_broadcast("solo-cam", crate::broadcast::Route::announced())
			.unwrap();
		settle().await;

		let session =
			crate::lite::test_transport::ScriptedSession::per_stream(vec![publish_namespace_ok(VERSION).await]);
		let log = session.log.clone();

		let publisher = Publisher::new(
			TestRuntime::new(),
			session,
			origin.consume(),
			Control::new(None, false),
			None,
			declared(None),
			VERSION,
		);

		let mut run = std::pin::pin!(publisher.run_publish_namespaces());
		for _ in 0..100 {
			assert!(futures::poll!(run.as_mut()).is_pending());
			if occurrences(&log, b"solo-cam") > 0 {
				break;
			}
			settle().await;
		}
		assert_eq!(occurrences(&log, b"solo-cam"), 1, "the advertisement never went out");

		let advertised = log.writes.lock().unwrap().len();

		// Unannounce, which retires the request the advertisement opened.
		drop(cam);
		for _ in 0..100 {
			assert!(futures::poll!(run.as_mut()).is_pending());
			settle().await;
		}

		assert_eq!(
			log.writes.lock().unwrap().len(),
			advertised,
			"a draft-17+ withdrawal wrote a message; the FIN alone retracts"
		);
	}

	/// The peer granting a stream is only half the exchange. One it accepts and then never
	/// answers on wedges the loop exactly as a parked open does, so the response is bounded
	/// too: everything queued behind it is otherwise stranded for the session.
	///
	/// Draft-14 so each advertisement names its namespace on the wire.
	#[tokio::test(start_paused = true)]
	async fn a_silent_answer_still_lets_the_next_namespace_be_advertised() {
		const VERSION: Version = Version::Draft14;

		let origin = crate::origin::Info::new(crate::Origin::new(1).unwrap()).produce();
		let _first = origin
			.create_broadcast("first-cam", crate::broadcast::Route::announced())
			.unwrap();
		let _second = origin
			.create_broadcast("second-cam", crate::broadcast::Route::announced())
			.unwrap();
		settle().await;

		// Every stream opens and then goes silent: an exhausted script parks rather than
		// reporting EOF, which is the peer that takes the request and answers nothing.
		let session = crate::lite::test_transport::ScriptedSession::per_stream(vec![Vec::new(), Vec::new()]);
		let log = session.log.clone();

		let publisher = Publisher::new(
			TestRuntime::new(),
			session,
			origin.consume(),
			Control::new(None, false),
			None,
			declared(Some(false)),
			VERSION,
		);

		let mut run = std::pin::pin!(publisher.run_publish_namespaces());
		for _ in 0..200 {
			assert!(futures::poll!(run.as_mut()).is_pending());
			if occurrences(&log, b"first-cam") > 0 && occurrences(&log, b"second-cam") > 0 {
				break;
			}
			tick().await;
		}

		// Whichever went first is the one that stalled, so both having reached the wire is
		// the proof: the loop gave up on the answer and carried on.
		assert!(
			occurrences(&log, b"first-cam") > 0,
			"the first advertisement never went out"
		);
		assert!(
			occurrences(&log, b"second-cam") > 0,
			"the silent answer wedged the loop: the second namespace never went out"
		);
	}

	/// Credit returning raises no signal of its own: no announce, no route change, nothing
	/// the loop is watching. Only a retry brings the namespace back, and without one it
	/// stays undiscoverable for the life of the session.
	#[tokio::test(start_paused = true)]
	async fn a_namespace_refused_a_stream_is_retried_on_its_own() {
		const VERSION: Version = Version::Draft14;

		let origin = crate::origin::Info::new(crate::Origin::new(1).unwrap()).produce();
		let _cam = origin
			.create_broadcast("lonely-cam", crate::broadcast::Route::announced())
			.unwrap();
		settle().await;

		// Closed from the start: the peer has granted nothing.
		let gate = kio::Producer::new(false);
		let ok = publish_namespace_ok(VERSION).await;
		let session = crate::lite::test_transport::ScriptedSession::gated_open(vec![ok], gate.consume());
		let log = session.log.clone();

		let publisher = Publisher::new(
			TestRuntime::new(),
			session,
			origin.consume(),
			Control::new(None, false),
			None,
			declared(Some(false)),
			VERSION,
		);

		let mut run = std::pin::pin!(publisher.run_publish_namespaces());

		// Well past the point where the open gives up.
		for _ in 0..100 {
			assert!(futures::poll!(run.as_mut()).is_pending());
			tick().await;
		}
		assert_eq!(occurrences(&log, b"lonely-cam"), 0, "advertised without a stream");

		// Credit returns. Nothing else changes: no publish, no unannounce, no route move.
		set_gate(&gate, true);

		for _ in 0..100 {
			assert!(futures::poll!(run.as_mut()).is_pending());
			if occurrences(&log, b"lonely-cam") > 0 {
				break;
			}
			tick().await;
		}
		assert_eq!(occurrences(&log, b"lonely-cam"), 1, "never came back on its own");
	}

	/// Advance far enough that a parked open gives up and its retry comes due, without
	/// making the test wait: time is paused, so this only moves the clock the loop reads.
	async fn tick() {
		tokio::time::advance(Duration::from_millis(200)).await;
	}

	fn set_gate(gate: &kio::Producer<bool>, open: bool) {
		let Ok(mut gate) = gate.write() else {
			panic!("gate closed")
		};
		*gate = open;
	}

	/// A publisher talking to a scripted peer that never answers, over one bidi stream.
	struct Harness {
		publisher: Publisher<crate::lite::test_transport::ScriptedSession, TestRuntime>,
		session: crate::lite::test_transport::ScriptedSession,
		log: crate::lite::test_transport::Log,
		/// Keeps the origin alive; the publisher only holds a consumer.
		_origin: origin::Producer,
	}

	fn harness(version: Version) -> Harness {
		let origin = crate::origin::Info::new(crate::Origin::new(1).unwrap()).produce();
		let session = crate::lite::test_transport::ScriptedSession::per_stream(vec![Vec::new()]);
		let log = session.log.clone();

		// Serving a request blocks on the peer's SETUP, which no scripted peer sends here.
		let peer_setup = peer::PeerSetup::default();
		peer_setup.set(peer::Peer::default());

		let publisher = Publisher::new(
			TestRuntime::new(),
			session.clone(),
			origin.consume(),
			Control::new(None, false),
			None,
			peer_setup,
			version,
		);

		Harness {
			publisher,
			session,
			log,
			_origin: origin,
		}
	}

	/// Subscribe to a path nothing publishes, returning what the peer would read off the
	/// request stream plus the reset codes the stream recorded.
	async fn subscribe_missing(version: Version) -> (Vec<u8>, Vec<u32>) {
		let h = harness(version);

		let stream = Stream::open(&mut h.session.clone(), version).await.unwrap();
		h.publisher
			.clone()
			.run_subscribe_stream(
				stream,
				ietf::Subscribe {
					request_id: RequestId(1),
					track_namespace: crate::Path::new("nothing/here"),
					track_name: "video".into(),
					subscriber_priority: 128,
					group_order: GroupOrder::Descending,
					filter_type: FilterType::LargestObject,
				},
			)
			.await
			.unwrap();

		let writes = h.log.writes.lock().unwrap().clone();
		(writes, h.log.resets())
	}

	/// Send a FETCH we don't implement, returning the same pair.
	async fn fetch_unsupported(version: Version, fetch_type: FetchType<'_>) -> (Vec<u8>, Vec<u32>) {
		let h = harness(version);

		let stream = Stream::open(&mut h.session.clone(), version).await.unwrap();
		h.publisher
			.clone()
			.run_fetch_stream(
				stream,
				ietf::Fetch {
					request_id: RequestId(1),
					subscriber_priority: 128,
					group_order: GroupOrder::Descending,
					fetch_type,
				},
			)
			.await
			.unwrap();

		let writes = h.log.writes.lock().unwrap().clone();
		(writes, h.log.resets())
	}

	/// A SUBSCRIBE for a path with no publisher is refused with REQUEST_ERROR, and the refusal
	/// has to survive the trip. `Writer` resets the stream on drop, and a reset that races the
	/// write discards the bytes the peer has not read yet, which leaves the subscriber waiting
	/// on a request we already refused. Finishing first makes the drop-time reset a no-op.
	#[tokio::test]
	async fn missing_broadcast_is_refused_without_resetting_the_stream() {
		for version in [Version::Draft17, Version::Draft18, Version::Draft19] {
			let (writes, resets) = subscribe_missing(version).await;

			assert!(!writes.is_empty(), "{version}: nothing was sent");
			assert_eq!(
				writes[0],
				ietf::RequestError::ID as u8,
				"{version}: not a REQUEST_ERROR"
			);
			assert!(resets.is_empty(), "{version}: stream reset, discarding the error");
		}
	}

	/// Every FETCH we refuse goes out through its own error encoder, so it needs the same
	/// finish: a reset there loses the rejection the same way.
	#[tokio::test]
	async fn unsupported_fetch_is_refused_without_resetting_the_stream() {
		let unsupported = || {
			[
				(
					"standalone",
					FetchType::Standalone {
						namespace: crate::Path::new("nothing/here"),
						track: "video".into(),
						start: Location { group: 0, object: 0 },
						end: Location { group: 1, object: 0 },
					},
				),
				(
					"relative joining with an offset",
					FetchType::RelativeJoining {
						subscriber_request_id: RequestId(3),
						group_offset: 1,
					},
				),
				(
					"absolute joining",
					FetchType::AbsoluteJoining {
						subscriber_request_id: RequestId(3),
						group_id: 7,
					},
				),
			]
		};

		for version in [Version::Draft17, Version::Draft18, Version::Draft19] {
			for (label, fetch_type) in unsupported() {
				let (writes, resets) = fetch_unsupported(version, fetch_type).await;

				assert!(!writes.is_empty(), "{version} {label}: nothing was sent");
				assert_eq!(
					writes[0],
					ietf::RequestError::ID as u8,
					"{version} {label}: not a REQUEST_ERROR"
				);
				assert!(
					resets.is_empty(),
					"{version} {label}: stream reset, discarding the error"
				);
			}
		}
	}
}
