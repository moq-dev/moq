use crate::{frame, group, origin, track};
use std::{
	collections::HashMap,
	task::{Poll, ready},
	time::Duration,
};

use web_transport_trait::{MaybeSend, MaybeSync, poll::SendStream as _};

use crate::{
	AsPath, Error, Timescale, Timestamp,
	coding::{Stream, Writer},
	ietf::{self, Control, EndLocation, FetchHeader, FetchType, Filter, GroupOrder, Location, RequestId},
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

enum FillStep {
	Batch,
	Partial(frame::Consumer),
	Done,
}

/// A broadcast whose route table is watched for changes in what we advertise: the
/// namespace becoming (un)advertisable, or its path or cost moving.
struct Watched {
	/// The route last announced for this namespace, as the origin delivered it.
	route: crate::origin::Route,
	/// What the peer currently holds for this namespace, or [`Advert::None`] while it
	/// is filtered. A selection that differs is worth a wire message; one that matches
	/// is not.
	sent: Advert,
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
	fn new(route: crate::origin::Route) -> Self {
		Self {
			route,
			sent: Advert::None,
			deferred: false,
			refused: Refused::No,
		}
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
	/// An origin-level route (un)announce, `None` once the announce stream ends.
	Update(Option<crate::announce::Update>),
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
	self_origin: crate::Hop,
	// The identity assigned to the peer by the caller (`Client::with_peer_hop`, or
	// the per-session default a server hands every request), used when the peer declares
	// none itself. A peer that negotiates the MoQ Cluster extension declares its own,
	// which wins unless it withheld it as the reserved 0.
	peer_hop: Option<crate::Hop>,
	// What the peer declared in its SETUP, filled when that stream is read.
	peer_setup: peer::PeerSetup,
	version: Version,
}

impl<S, R> Publisher<S, R>
where
	S: crate::transport::poll::Boxable,
	R: crate::runtime::Runtime + MaybeSend + MaybeSync + 'static,
	R::Timer: MaybeSend,
{
	pub fn new(
		runtime: R,
		session: S,
		origin: origin::Consumer,
		control: Control,
		peer_hop: Option<crate::Hop>,
		peer_setup: peer::PeerSetup,
		version: Version,
	) -> Self {
		Self {
			runtime,
			session,
			self_origin: *origin,
			origin,
			control,
			peer_hop,
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
		self.excluding(&self.peer().await)
	}

	/// Our origin handle with [`Self::exclude`] applied, the view both the data plane
	/// and the announce loops read this peer's routes through.
	fn excluding(&self, peer: &cluster::Peer) -> origin::Consumer {
		match self.exclude(peer) {
			crate::Hop::UNKNOWN => self.origin.clone(),
			exclude => self.origin.clone().excluding(exclude),
		}
	}

	/// The Hop ID whose paths must not be advertised (or served) back to this peer.
	///
	/// A peer that declared an identity supplies its own; otherwise fall back to the one
	/// we assigned it (`Client::with_peer_hop` when dialing, `Request::with_peer_hop`
	/// or a fresh per-session id when accepting), since moq-transport carries no identity
	/// of its own. A peer that declared the reserved 0 declared no identity, so it takes
	/// the fallback like any other anonymous peer.
	fn exclude(&self, peer: &cluster::Peer) -> crate::Hop {
		peer.identity().or(self.peer_hop).unwrap_or(crate::Hop::UNKNOWN)
	}

	/// Pick what to advertise to this peer for one route.
	///
	/// The origin's announce cursor already filters routes through the peer
	/// (control-plane split horizon); this only shapes what the wire can carry.
	fn select(&self, route: &crate::origin::Route, peer: &cluster::Peer) -> Advert {
		// A route that already passed through us is a reflection. The origin
		// filters these on receive, so this is defensive.
		if self.self_origin != crate::Hop::UNKNOWN && route.hops.contains(&self.self_origin) {
			return Advert::None;
		}

		if !peer.negotiated() {
			return Advert::Plain;
		}

		// The Cluster extension has room for the warm cost only; a peer on this
		// wire learns nothing about the cold path (see `cluster::Advert::route`).
		let cost = route.cost.clamped().warm;
		// Our own Hop ID is always the last entry, so the peer reconstructs the full
		// path. A chain with no room left is a loop in all but name.
		match cluster::Advert::forward(&route.hops, cost, self.self_origin) {
			Ok(advert) => Advert::Cluster(advert),
			Err(_) => Advert::None,
		}
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

		let track = match broadcast.track(&msg.track_name) {
			Ok(track) => track,
			Err(err) => {
				return self.reject_subscribe(stream, request_id, 404, &err.to_string()).await;
			}
		};

		let mut subscription = serving_subscription(msg.subscriber_priority);
		let priority = subscription.priority;

		// Subscribe before resolving the filter: on a routed broadcast the live edge only
		// becomes readable once the subscription's demand attaches a route, so the edge
		// snapshot has to come after. The resolved range is applied to the preference
		// right below, before anything is served.
		let (cache, mut track) = {
			match track.subscribe(subscription.clone()).await {
				Ok(subscribed) => (track, subscribed),
				Err(err) => {
					return self.reject_subscribe(stream, request_id, 404, &err.to_string()).await;
				}
			}
		};

		// The filter and any fill are relative to the live edge, so snapshot it once:
		// the fill ends exactly where a Next Object subscription begins, which is what
		// lets the draft's current-group join (Next Object plus a StartGroup=1 fill)
		// cover the group with no gap and no overlap.
		let edge = live_edge(&cache);
		let range = subscribe_range(&msg, edge, self.version);
		subscription.start = range.start.map(|start| track::Position {
			group: start.group,
			frame: start.object,
		});
		subscription.end = range.end.and_then(|end| match end.object {
			Some(object) => track::Position::after(end.group, object),
			None => track::Position::after_group(end.group),
		});
		let _ = track.update(subscription);
		let timescale = msg.properties_wanted.then(|| track.info().timescale);

		// A fill reads the group cache through its own consumer, independent of the
		// subscription's cursor.
		let fill = msg
			.fill
			.filter(|_| Filter::is_draft20(self.version))
			.map(|fill| (fill_range(fill, msg.filter, edge.largest), cache, timescale));

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
				// Required once the track has content; a fill-requesting subscriber
				// sizes its backfill against this.
				largest: edge.largest,
				properties: match msg.properties_wanted {
					// Declaring the timescale is what opts the track into timestamps; every
					// object Timestamp below is in these units.
					// We serve the newest group first, matching moq-lite.
					true => ietf::Properties {
						timescale: Some(track.info().timescale),
						group_order: Some(GroupOrder::Descending),
					},
					// INCLUDE_PROPERTIES=0. The field stays present but empty, which also
					// means the track opts out of timestamps for this subscriber.
					false => ietf::Properties::default(),
				},
			})
			.await?;

		// Run the track, cancelling on reader close (Unsubscribe or stream close).
		// The fill (when one was requested) runs alongside on its own fetch stream;
		// its failures reset that stream and never touch the subscription.
		let res = {
			let mut track_serve =
				TrackServe::new(self.session.clone(), track, request_id, self.version, range, timescale);
			let serve = async {
				match fill {
					Some((fill, cache, timescale)) => {
						let fill = self.run_fill(request_id, priority, fill, cache, timescale);
						let track = kio::wait(|waiter| track_serve.poll(waiter));
						let (res, ()) = futures::join!(track, fill);
						res
					}
					None => kio::wait(|waiter| track_serve.poll(waiter)).await,
				}
			};
			let mut serve = std::pin::pin!(serve);
			let mut closed_session = self.session.clone();
			kio::wait(|waiter| {
				if let Poll::Ready(res) = waiter.poll_future(serve.as_mut()) {
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
		let (status, reason) = match &res {
			Ok(()) => (ietf::PublishDoneStatus::TrackEnded, "track ended"),
			Err(_) => (ietf::PublishDoneStatus::InternalError, "internal error"),
		};
		let _ = stream.writer.encode(&ietf::PublishDone::ID).await;
		let _ = stream
			.writer
			.encode(&ietf::PublishDone {
				request_id: match self.version {
					Version::Draft14 | Version::Draft15 | Version::Draft16 => Some(request_id),
					_ => None,
				},
				status_code: status.code(self.version),
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

	/// Serve a draft-20 fill on its own fetch stream: the requested range, read from the
	/// group cache, capped at the Largest Object snapshot.
	///
	/// A fill is a promise once requested. An empty range opens no stream, but a range we
	/// cannot serve still opens one and resets it right after the FETCH_HEADER, the
	/// draft's fill-failure signal. Nothing here touches the subscription either way.
	async fn run_fill(
		&self,
		request_id: RequestId,
		priority: u8,
		fill: FillServe,
		track: track::Consumer,
		timescale: Option<Timescale>,
	) {
		if matches!(fill, FillServe::Empty) {
			return;
		}

		let mut session = self.session.clone();
		let stream = match session.open_uni().await {
			Ok(stream) => stream,
			Err(err) => {
				tracing::debug!(err = %Error::from_transport(err), fill = %request_id, "fill stream failed to open");
				return;
			}
		};
		let mut stream = Writer::new(stream, self.version);
		stream.set_priority(priority);

		let res = async {
			stream.encode(&FetchHeader::TYPE).await?;
			stream.encode(&FetchHeader { request_id }).await?;

			let FillServe::Group { sequence, skip, until } = fill else {
				return Err(Error::Unsupported);
			};

			let group = track
				.fetch_group(
					sequence,
					group::Fetch {
						priority,
						..Default::default()
					},
				)
				.await?;
			Self::write_fill_group(&mut stream, group, sequence, skip, until, timescale, self.version).await
		}
		.await;

		match res {
			Ok(()) => {
				// Close waits for the acknowledgement, and consuming the writer disarms
				// the Drop fallback that would reset a finished stream.
				if let Err(err) = stream.close().await {
					tracing::debug!(%err, fill = %request_id, "fill stream close failed");
				} else {
					tracing::debug!(fill = %request_id, "fill complete");
				}
			}
			Err(err) => {
				tracing::debug!(%err, fill = %request_id, "fill failed, resetting its stream");
				stream.abort(&err);
			}
		}
	}

	/// Write one group's frames as draft-20 fetch objects (section 11.4.4).
	///
	/// The first object carries its absolute Group and Object IDs plus the priority;
	/// every later one inherits them and increments the Object ID, so only the
	/// properties (the timestamp) and the payload go on the wire. A fetch object has no
	/// status field: a zero payload length is simply an empty object.
	async fn write_fill_group(
		stream: &mut Writer<S::SendStream, Version>,
		mut group: group::Consumer,
		sequence: u64,
		skip: u64,
		until: Option<u64>,
		timescale: Option<Timescale>,
		version: Version,
	) -> Result<(), Error> {
		let mut index: u64 = 0;
		let mut first = true;

		let mut buf: frame::Buffer = frame::Buffer::new();
		'serve: loop {
			// The cap is the Largest Object snapshot: the group may keep growing, but
			// everything past the snapshot belongs to the subscription, not the fill.
			if until.is_some_and(|until| index >= until) {
				break;
			}

			let step = {
				let mut closed = std::pin::pin!(stream.closed());
				kio::wait(|waiter| {
					if waiter.poll_future(closed.as_mut()).is_ready() {
						return Poll::Ready(Err(Error::Cancel));
					}
					match group.poll_read_frames(waiter, &mut buf) {
						Poll::Pending => match group.poll_next_frame(waiter) {
							Poll::Ready(Ok(Some(frame))) => Poll::Ready(Ok(FillStep::Partial(frame))),
							Poll::Ready(Ok(None)) => Poll::Ready(Ok(FillStep::Done)),
							Poll::Ready(Err(err)) => Poll::Ready(Err(err)),
							Poll::Pending => Poll::Pending,
						},
						Poll::Ready(Ok(0)) => Poll::Ready(Ok(FillStep::Done)),
						Poll::Ready(Ok(_)) => Poll::Ready(Ok(FillStep::Batch)),
						Poll::Ready(Err(err)) => Poll::Ready(Err(err)),
					}
				})
				.await
			};

			match step? {
				FillStep::Batch => {
					for i in 0..buf.filled().len() {
						if until.is_some_and(|until| index >= until) {
							break 'serve;
						}
						let frame = buf.filled()[i].clone();
						if index >= skip {
							Self::write_fill_object(
								stream,
								sequence,
								index,
								std::mem::take(&mut first),
								frame.timestamp,
								timescale,
								version,
							)
							.await?;
							stream.encode(&(frame.payload.len() as u64)).await?;
							if !frame.payload.is_empty() {
								let mut payload = frame.payload;
								stream.write_all(&mut payload).await?;
							}
						}
						index += 1;
						group.keep_alive();
					}
				}
				FillStep::Partial(mut frame) => {
					if index < skip {
						// A skipped frame still has to be drained to advance the cursor.
						loop {
							let chunk = {
								let mut closed = std::pin::pin!(stream.closed());
								kio::wait(|waiter| {
									if waiter.poll_future(closed.as_mut()).is_ready() {
										return Poll::Ready(Err(Error::Cancel));
									}
									frame.poll_read_chunk(waiter)
								})
								.await
							};
							if chunk?.is_none() {
								break;
							}
						}
						index += 1;
						continue;
					}

					Self::write_fill_object(
						stream,
						sequence,
						index,
						std::mem::take(&mut first),
						frame.timestamp,
						timescale,
						version,
					)
					.await?;
					index += 1;

					stream.encode(&frame.size).await?;
					loop {
						let chunk = {
							let mut closed = std::pin::pin!(stream.closed());
							kio::wait(|waiter| {
								if waiter.poll_future(closed.as_mut()).is_ready() {
									return Poll::Ready(Err(Error::Cancel));
								}
								frame.poll_read_chunk(waiter)
							})
							.await
						};

						match chunk? {
							Some(mut chunk) => stream.write_all(&mut chunk).await?,
							None => break,
						}
					}
				}
				FillStep::Done => break,
			}
		}

		Ok(())
	}

	/// Write one fetch object's header: the Serialization Flags, the fields they
	/// declare, and the properties block carrying the timestamp.
	async fn write_fill_object(
		stream: &mut Writer<S::SendStream, Version>,
		sequence: u64,
		object: u64,
		first: bool,
		timestamp: Timestamp,
		timescale: Option<Timescale>,
		version: Version,
	) -> Result<(), Error> {
		// Serialization Flags: the two low bits encode the subgroup (00 = subgroup
		// zero), then per-field presence bits.
		const OBJECT_ID: u64 = 0x04;
		const GROUP_ID: u64 = 0x08;
		const PRIORITY: u64 = 0x10;
		const PROPERTIES: u64 = 0x20;
		let properties = if timescale.is_some() { PROPERTIES } else { 0 };

		if first {
			// The first object must carry its absolute Group and Object IDs. Include the
			// priority too: "same as the prior object" has no prior to refer to.
			stream.encode(&(GROUP_ID | OBJECT_ID | PRIORITY | properties)).await?;
			stream.encode(&sequence).await?;
			stream.encode(&object).await?;
			stream.encode(&0u8).await?;
		} else {
			// Same group and priority; the Object ID is the prior one plus one.
			stream.encode(&properties).await?;
		}

		if let Some(timescale) = timescale {
			let mut ext = bytes::BytesMut::new();
			ietf::encode_object_time(&mut ext, timestamp, timescale, version)?;
			stream.encode(&(ext.len() as u64)).await?;
			let mut ext = ext.freeze();
			stream.write_all(&mut ext).await?;
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
		let advert = self.select(&watch.route, peer);
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
			watch.sent = sent;
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
		let origin = self.excluding(&peer);

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
			crate::Hop::UNKNOWN => origin,
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

		// When to re-offer whatever the peer should hold and doesn't, and how long to wait
		// the next time that fails. Jittered so a relay's namespaces don't all come back on
		// the same tick.
		let mut retry = crate::runtime::Deadline::new(&self.runtime);
		let mut retry_at: Option<crate::runtime::Instant> = None;
		let mut retry_delay = RETRY_BASE;

		// Stream updates (origin route (un)announces), bailing if the peer closes
		// its side first.
		let res = loop {
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
				let Namespaces { target, .. } = &mut ns;
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
					Poll::Pending
				})
				.await
			};

			match event {
				NamespaceEvent::Closed(res) => break res,
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
				NamespaceEvent::Update(Some(update)) => {
					let path = update.prefix.as_path().to_owned();
					let suffix = path
						.strip_prefix(&prefix)
						.expect("origin returned invalid prefix")
						.to_owned();

					if update.active {
						// A repeat for a live suffix is a metadata update: keep the
						// peer's refusal state and re-run the selection.
						match ns.watched.get_mut(&suffix) {
							Some(watch) => watch.route = update.route,
							None => {
								ns.watched.insert(suffix.clone(), Watched::new(update.route));
							}
						}
						self.sync_namespace(&mut ns, &suffix, &path).await?;
					} else {
						// Only close out namespaces the peer actually saw.
						let held = ns.watched.remove(&suffix).is_some_and(|watch| watch.sent.wanted());
						if held {
							tracing::debug!(route = %self.origin.absolute(&path), "namespace_done");
							self.withdraw_namespace(&mut ns.target, &mut ns.requests, suffix)
								.await?;
						}
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
	range: ServeRange,
	timescale: Option<Timescale>,
	children: kio::Tasks<GroupServe<S>>,
	/// The track finished: the in-flight group machines drain, then FIN.
	draining: bool,
}

impl<S: crate::transport::poll::Session> TrackServe<S> {
	fn new(
		session: S,
		mut track: track::Subscriber,
		request_id: RequestId,
		version: Version,
		range: ServeRange,
		timescale: Option<Timescale>,
	) -> Self {
		match range.start {
			Some(start) => track.start_at(start.group),
			None => {
				if let Some(latest) = track.latest() {
					track.start_at(latest);
				}
			}
		}
		track.end_at(range.end.map(|end| end.group));

		Self {
			session,
			track,
			request_id,
			version,
			range,
			timescale,
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

					let slice = GroupSlice {
						skip: match self.range.start {
							Some(start) if start.group == sequence => start.object,
							_ => 0,
						},
						until: match self.range.end {
							Some(end) if end.group == sequence => end.object.map(|object| object.saturating_add(1)),
							_ => None,
						},
					};
					if slice.until.is_some_and(|until| until <= slice.skip) {
						continue;
					}

					let msg = ietf::GroupHeader {
						track_alias: self.request_id.0,
						group_id: sequence,
						sub_group_id: 0,
						// The publisher's own ranking of its tracks, which is what a relay
						// prefers when it can't pick between its subscribers' priorities. The
						// model ranks higher-first and this wire field lower-first.
						publisher_priority: super::priority::to_wire(self.track.info().priority),
						// Carry per-object timestamps as extension headers (the Timestamp
						// Object Property) so moq-transport peers get the real PTS. The
						// units are the track's, declared once in SUBSCRIBE_OK.
						flags: ietf::GroupFlags {
							has_extensions: self.timescale.is_some(),
							first_object: slice.skip == 0,
							..Default::default()
						},
					};

					self.children.push(GroupServe::new(
						self.session.clone(),
						msg,
						self.track.subscription().priority,
						group,
						self.timescale,
						self.version,
						slice,
					));
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
	timescale: Option<Timescale>,
	version: Version,
	object_delta: u64,
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
		batch: Box<frame::Buffer>,
		batch_pos: usize,
	},
	/// Every frame is written and the FIN sent: wait for the acknowledgement so a
	/// late cancel can still reset the stream.
	Closed {
		writer: Writer<S::SendStream, Version>,
	},
	Done,
}

impl<S: crate::transport::poll::Session> kio::Task for GroupServe<S> {
	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		// Errors just drop the writer, whose Drop resets the stream, exactly like
		// the old future being discarded.
		ready!(self.poll_serve(waiter)).map(|()| ()).unwrap_or(());
		Poll::Ready(())
	}
}

impl<S: crate::transport::poll::Session> GroupServe<S> {
	fn new(
		session: S,
		msg: ietf::GroupHeader,
		priority: u8,
		mut group: group::Consumer,
		timescale: Option<Timescale>,
		version: Version,
		slice: GroupSlice,
	) -> Self {
		group.skip_to(slice.skip);
		group.end_at(slice.until.and_then(|until| until.checked_sub(1)));
		let object_delta = group.index();
		Self {
			session,
			msg,
			priority,
			group,
			timescale,
			version,
			object_delta,
			state: GroupState::Open,
		}
	}

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
						batch: Box::new(frame::Buffer::new()),
						batch_pos: 0,
					};
				}
				GroupState::Serve {
					writer,
					frame,
					chunk,
					batch,
					batch_pos,
				} => {
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
							} else if *batch_pos < batch.len() {
								let batched = &mut batch.filled_mut()[*batch_pos];
								if let Err(err) = buffer_object_info(
									writer,
									std::mem::take(&mut self.object_delta),
									self.msg.flags.has_extensions,
									batched.timestamp,
									batched.payload.len() as u64,
									self.timescale,
									self.version,
								) {
									break 'serve Err(err);
								}
								let payload = std::mem::take(&mut batched.payload);
								if !payload.is_empty() {
									*chunk = Some(payload);
								}
								*batch_pos += 1;
								self.group.keep_alive();
							} else {
								match self.group.poll_read_frames(waiter, batch) {
									Poll::Ready(Ok(count)) if count > 0 => {
										*batch_pos = 0;
										continue;
									}
									Poll::Ready(Ok(_)) => break 'serve Ok(()),
									Poll::Ready(Err(err)) => break 'serve Err(err),
									Poll::Pending => {}
								}

								match self.group.poll_next_frame(waiter) {
									Poll::Ready(Ok(Some(next))) => {
										if let Err(err) = buffer_object(
											writer,
											std::mem::take(&mut self.object_delta),
											self.msg.flags.has_extensions,
											&next,
											self.timescale,
											self.version,
										) {
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

/// Buffer one object's header and prefix: the id delta, optional extension
/// headers carrying the timestamp, the size, and (for an empty object) the status.
fn buffer_object<W: crate::transport::poll::SendStream>(
	writer: &mut Writer<W, Version>,
	delta: u64,
	has_extensions: bool,
	frame: &frame::Consumer,
	timescale: Option<Timescale>,
	version: Version,
) -> Result<(), Error> {
	buffer_object_info(
		writer,
		delta,
		has_extensions,
		frame.timestamp,
		frame.size,
		timescale,
		version,
	)
}

fn buffer_object_info<W: crate::transport::poll::SendStream>(
	writer: &mut Writer<W, Version>,
	delta: u64,
	has_extensions: bool,
	timestamp: Timestamp,
	size: u64,
	timescale: Option<Timescale>,
	version: Version,
) -> Result<(), Error> {
	writer.buffer(&delta)?;

	if let Some(timescale) = timescale.filter(|_| has_extensions) {
		// Per-object extension headers carry the frame's presentation timestamp.
		let mut ext = bytes::BytesMut::new();
		ietf::encode_object_time(&mut ext, timestamp, timescale, version)?;
		writer.buffer(&(ext.len() as u64))?;
		writer.buffer_raw(&ext);
	}

	writer.buffer(&size)?;
	if size == 0 {
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
	use crate::coding::Decode;
	use crate::ietf::priority;
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

		let mut serve = GroupServe::new(
			session,
			msg,
			200,
			consumer,
			Some(Timescale::default()),
			Version::Draft14,
			GroupSlice::default(),
		);
		kio::wait(|waiter| serve.poll_serve(waiter)).await.unwrap();

		assert_eq!(
			log.priorities(),
			vec![200],
			"model priority must pass through unchanged"
		);
	}

	/// The publisher's own ranking of its tracks (`track::Info::priority`) is what a relay
	/// prefers when it has no subscriber preference to go on, so it has to reach the wire.
	/// It went out as a flat 0 before, which put catalog, audio, and video in one tier for
	/// every moq-transport peer.
	#[tokio::test]
	async fn group_header_carries_the_publisher_priority() {
		let log = crate::lite::test_transport::Log::default();
		let session = SinkSession::new(log.clone());

		let info = track::Info::default().with_priority(hang_audio_priority());
		let mut track = track::Producer::new(std::sync::Arc::new(crate::broadcast::Info::default()), "test", info);
		let subscriber = track.subscribe(None);

		let mut group = track.append_group().unwrap();
		group.write_frame(crate::Timestamp::ZERO, b"hello".as_slice()).unwrap();
		group.finish().unwrap();
		track.finish().unwrap();

		let mut serve = TrackServe::new(
			session,
			subscriber,
			RequestId(0),
			Version::Draft14,
			ServeRange::default(),
			Some(Timescale::default()),
		);
		kio::wait(|waiter| serve.poll(waiter)).await.unwrap();

		let written = log.writes.lock().unwrap().clone();
		let mut buf = bytes::Bytes::from(written);
		let header = ietf::GroupHeader::decode(&mut buf, Version::Draft14).expect("a group header");
		assert_eq!(
			header.publisher_priority,
			priority::to_wire(hang_audio_priority()),
			"the wire is lower-first, so audio must encode below video"
		);
		assert!(
			priority::to_wire(hang_audio_priority()) < priority::to_wire(hang_video_priority()),
			"audio outranks video on the wire"
		);
	}

	/// `hang::catalog::PRIORITY` isn't reachable from `moq-net` (hang depends on it, not the
	/// other way round), so the two ranks it publishes audio and video at are spelled here.
	fn hang_audio_priority() -> u8 {
		80
	}

	fn hang_video_priority() -> u8 {
		60
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

		let mut serve = GroupServe::new(
			session,
			ietf::GroupHeader {
				track_alias: 0,
				group_id: 0,
				sub_group_id: 0,
				publisher_priority: 0,
				flags: Default::default(),
			},
			0,
			group,
			Some(Timescale::default()),
			Version::Draft19,
			GroupSlice::default(),
		);
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
		let mut serve = GroupServe::new(
			session,
			ietf::GroupHeader {
				track_alias: 0,
				group_id: 0,
				sub_group_id: 0,
				publisher_priority: 0,
				flags: Default::default(),
			},
			0,
			group,
			Some(Timescale::default()),
			Version::Draft19,
			GroupSlice::default(),
		);
		let mut serving = std::pin::pin!(kio::wait(|waiter| serve.poll_serve(waiter)));
		// Let it run until it blocks on the rest of the frame.
		assert!(futures::poll!(serving.as_mut()).is_pending());

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

		let mut serve = TrackServe::new(
			session,
			subscriber,
			RequestId(1),
			Version::Draft14,
			ServeRange::default(),
			Some(Timescale::default()),
		);
		kio::wait(|waiter| serve.poll(waiter)).await.unwrap();

		// `GroupServe` sets the priority once per stream it opens, so this counts groups served.
		assert_eq!(log.priorities().len(), 1, "only group 3 should have been served");
	}
}

#[cfg(test)]
mod serve_tests {
	use super::*;
	use crate::lite::test_transport::{Log, ScriptedSession, SinkSession};
	use crate::model::ProduceTest;

	type TestRuntime = crate::runtime::tokio_test::Tokio<ScriptedSession>;

	fn occurrences(log: &Log, needle: &[u8]) -> usize {
		let writes = log.writes.lock().unwrap();
		writes.windows(needle.len()).filter(|window| *window == needle).count()
	}

	fn timestamp() -> crate::Timestamp {
		crate::Timestamp::from_millis(0).unwrap()
	}

	/// A publisher whose origin serves one broadcast ("room") with one track ("video").
	struct Serve {
		publisher: Publisher<ScriptedSession, TestRuntime>,
		session: ScriptedSession,
		log: Log,
		track: track::Producer,
		_origin: origin::Producer,
		_broadcast: crate::broadcast::Producer,
	}

	fn serve(version: Version) -> Serve {
		let origin = crate::origin::Info::new(crate::Hop::new(1).unwrap()).produce();
		let mut broadcast = origin.create_broadcast("room").unwrap();
		let track = broadcast.create_track("video", None).unwrap();

		let session = ScriptedSession::per_stream(vec![Vec::new()]);
		let log = session.log.clone();

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

		Serve {
			publisher,
			session,
			log,
			track,
			_origin: origin,
			_broadcast: broadcast,
		}
	}

	/// A distinctive request id, so `[FetchHeader::TYPE, REQUEST_ID]` is a usable needle.
	const REQUEST_ID: u64 = 0x2B;

	fn subscribe(filter: Filter, fill: Option<ietf::Fill>) -> ietf::Subscribe<'static> {
		ietf::Subscribe {
			request_id: RequestId(REQUEST_ID),
			track_namespace: crate::Path::new("room"),
			track_name: "video".into(),
			subscriber_priority: 128,
			group_order: GroupOrder::Descending,
			filter,
			fill,
			properties_wanted: true,
		}
	}

	/// The bytes that begin every fill fetch stream.
	const FETCH_STREAM: &[u8] = &[FetchHeader::TYPE as u8, REQUEST_ID as u8];

	/// Serve `msg` against the live track, then finish the track so the subscription
	/// completes. Subscribing after the finish would be rejected instead of served.
	async fn run_live(h: &mut Serve, msg: ietf::Subscribe<'static>) {
		// `create_broadcast` registers the broadcast from a spawned task, so yield to the
		// runtime before subscribing or the lookup 404s.
		tokio::time::sleep(std::time::Duration::from_millis(1)).await;

		let mut session = h.session.clone();
		let stream = Stream::open(&mut session, h.publisher.version).await.unwrap();
		let mut serve = std::pin::pin!(h.publisher.clone().run_subscribe_stream(stream, msg));

		// Everything cached serves immediately; the subscription then parks at the live
		// edge, which is where the track is allowed to finish.
		for _ in 0..200 {
			assert!(
				futures::poll!(serve.as_mut()).is_pending(),
				"subscription ended before the track finished"
			);
		}

		h.track.finish().unwrap();
		serve.await.unwrap();
	}

	/// The draft's canonical current-group join: a Next Object subscription plus a
	/// StartGroup=1 fill. The published head arrives exactly once, on a fetch stream,
	/// and the subscription starts past the snapshot, so nothing is duplicated and
	/// nothing outside the requested range is sent.
	#[tokio::test]
	async fn canonical_join_serves_the_head_on_a_fetch_stream() {
		let mut h = serve(Version::Draft20);

		let mut group = h.track.create_group(group::Info { sequence: 0 }).unwrap();
		for payload in [b"head-0", b"head-1", b"head-2"] {
			group.write_frame(timestamp(), payload.as_slice()).unwrap();
		}
		group.finish().unwrap();

		run_live(
			&mut h,
			subscribe(
				Filter::NextObject,
				Some(ietf::Fill {
					filter: Some(Filter::Relative(1)),
					range_filters: false,
				}),
			),
		)
		.await;

		assert_eq!(occurrences(&h.log, FETCH_STREAM), 1, "expected one fill fetch stream");
		for payload in [b"head-0", b"head-1", b"head-2"] {
			assert_eq!(
				occurrences(&h.log, payload),
				1,
				"each object exactly once, via the fill"
			);
		}
		assert!(h.log.resets().is_empty(), "a served fill must not reset");
	}

	/// moq-lite's own join over draft-20: Relative(1) names the start of the current
	/// group, so the cache replays the whole group on the subscription stream.
	#[tokio::test]
	async fn relative_one_replays_the_current_group() {
		let mut h = serve(Version::Draft20);

		let mut group = h.track.create_group(group::Info { sequence: 0 }).unwrap();
		for payload in [b"head-0", b"head-1", b"head-2"] {
			group.write_frame(timestamp(), payload.as_slice()).unwrap();
		}
		group.finish().unwrap();

		run_live(&mut h, subscribe(Filter::Relative(1), None)).await;

		assert_eq!(occurrences(&h.log, FETCH_STREAM), 0, "no fill was requested");
		for payload in [b"head-0", b"head-1", b"head-2"] {
			assert_eq!(occurrences(&h.log, payload), 1, "the whole group replays in range");
		}
	}

	/// A Next Object subscription never receives the already-published head of the
	/// current group: everything below the snapshot is outside the requested range.
	#[tokio::test]
	async fn next_object_does_not_replay_the_head() {
		let mut h = serve(Version::Draft20);

		let mut group = h.track.create_group(group::Info { sequence: 0 }).unwrap();
		for payload in [b"head-0", b"head-1", b"head-2"] {
			group.write_frame(timestamp(), payload.as_slice()).unwrap();
		}
		group.finish().unwrap();

		run_live(&mut h, subscribe(Filter::NextObject, None)).await;

		for payload in [b"head-0", b"head-1", b"head-2"] {
			assert_eq!(
				occurrences(&h.log, payload),
				0,
				"the head is outside the requested range"
			);
		}
	}

	/// A fill spanning several groups is refused by resetting the fetch stream right
	/// after the FETCH_HEADER, the draft's fill-failure signal; the subscription itself
	/// is untouched and still completes.
	#[tokio::test]
	async fn a_multi_group_fill_resets_its_stream() {
		let mut h = serve(Version::Draft20);

		for sequence in 0..2 {
			let mut group = h.track.create_group(group::Info { sequence }).unwrap();
			group.write_frame(timestamp(), b"frame".as_slice()).unwrap();
			group.finish().unwrap();
		}

		run_live(
			&mut h,
			subscribe(
				Filter::NextObject,
				Some(ietf::Fill {
					filter: Some(Filter::Relative(2)),
					range_filters: false,
				}),
			),
		)
		.await;

		assert_eq!(occurrences(&h.log, FETCH_STREAM), 1, "the promised stream still opens");
		assert_eq!(h.log.resets().len(), 1, "and is reset as the failure signal");
	}

	/// A fill against an empty track has an empty range: no fetch stream is owed.
	#[tokio::test]
	async fn an_empty_track_opens_no_fill_stream() {
		let mut h = serve(Version::Draft20);

		run_live(
			&mut h,
			subscribe(
				Filter::NextObject,
				Some(ietf::Fill {
					filter: Some(Filter::Relative(1)),
					range_filters: false,
				}),
			),
		)
		.await;

		assert_eq!(occurrences(&h.log, FETCH_STREAM), 0);
		assert!(h.log.resets().is_empty());
	}

	/// The filter's object bounds trim what `run_group` writes: the skipped head is not
	/// sent, the first written object's delta is its absolute id, and a capped tail stops
	/// early. Extensions are off so the wire is just deltas, sizes, and payloads.
	#[tokio::test]
	async fn run_group_honors_the_slice() {
		fn header() -> ietf::GroupHeader {
			ietf::GroupHeader {
				track_alias: 0,
				group_id: 0,
				sub_group_id: 0,
				publisher_priority: 0,
				flags: ietf::GroupFlags {
					first_object: false,
					..Default::default()
				},
			}
		}

		async fn serve_slice(slice: GroupSlice) -> Vec<u8> {
			let log = Log::default();
			let session = SinkSession::new(log.clone());
			let mut track = track::Producer::new(std::sync::Arc::new(crate::broadcast::Info::default()), "test", None);
			let mut group = track.create_group(group::Info { sequence: 0 }).unwrap();
			for payload in [b"aa", b"bb", b"cc", b"dd"] {
				group.write_frame(timestamp(), payload.as_slice()).unwrap();
			}
			let consumer = group.consume();
			group.finish().unwrap();

			let mut serve = GroupServe::new(
				session,
				header(),
				0,
				consumer,
				Some(Timescale::default()),
				Version::Draft20,
				slice,
			);
			kio::wait(|waiter| serve.poll_serve(waiter)).await.unwrap();

			log.writes.lock().unwrap().clone()
		}

		// Skip 2: the head is dropped and the first delta is the absolute id 2.
		let trimmed = serve_slice(GroupSlice { skip: 2, until: None }).await;
		assert!(
			trimmed.ends_with(&[0x02, 0x02, b'c', b'c', 0x00, 0x02, b'd', b'd']),
			"expected delta 2 then cc, delta 0 then dd, got {trimmed:x?}"
		);

		// Until 2: only the head is written, stopping before the cap.
		let capped = serve_slice(GroupSlice {
			skip: 0,
			until: Some(2),
		})
		.await;
		assert!(
			capped.ends_with(&[0x00, 0x02, b'a', b'a', 0x00, 0x02, b'b', b'b']),
			"expected aa then bb only, got {capped:x?}"
		);
		assert_eq!(
			capped.windows(2).filter(|w| *w == b"cc").count(),
			0,
			"the cap excludes cc"
		);
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::lite::test_transport::SinkSession;
	use crate::model::ProduceTest;
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

	/// A publisher for a peer assigned `assigned`, over an origin holding two
	/// broadcasts: `from/peer`, whose only route flows through `assigned`, and
	/// `from/us`, which does not. The producers are returned so the routes outlive
	/// the assertions.
	async fn echo_harness(
		assigned: crate::Hop,
	) -> (
		Publisher<SinkSession, TestRuntime>,
		origin::Consumer,
		Vec<crate::announce::Producer>,
	) {
		let other = crate::Hop::new(778).unwrap();
		let origin = crate::origin::Info::new(crate::Hop::new(1).unwrap()).produce();
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

		let mut echoed_hops = crate::Hops::new();
		echoed_hops.push(assigned).unwrap();
		let echoed = origin
			.announce("from/peer", crate::origin::Route::default().with_hops(echoed_hops))
			.unwrap();

		let mut local_hops = crate::Hops::new();
		local_hops.push(other).unwrap();
		let local = origin
			.announce("from/us", crate::origin::Route::default().with_hops(local_hops))
			.unwrap();

		(publisher, consumer, vec![echoed, local])
	}

	/// A broadcast whose every route flows through the peer's assigned identity
	/// (`Client::with_peer_hop`) is never advertised to that peer; it would only
	/// echo the peer's own content back at it. A broadcast with an independent
	/// route still is.
	#[tokio::test(start_paused = true)]
	async fn assigned_peer_hop_filters_echoed_announces() {
		let assigned = crate::Hop::new(777).unwrap();
		let (publisher, consumer, _routes) = echo_harness(assigned).await;

		let peer = cluster::Peer::default();

		// The cursor is what filters: an excluded consumer never sees the echoed route.
		let mut announced = consumer.excluding(assigned).announced();
		let local = announced.assert_next_active("from/us");
		announced.assert_next_wait();

		assert_eq!(publisher.select(&local, &peer), Advert::Plain);
	}

	/// Declaring the reserved 0 turns the extension on while naming nobody, so the
	/// identity we assigned stands in, exactly as for a peer that never negotiated.
	/// Asserted on the resolution itself rather than through an advertisement: a
	/// negotiated peer always sends its own HOP_PATH, so a route attributed to the
	/// assigned identity is a state this peer class cannot reach; see
	/// [`a_declared_zero_chain_is_still_advertised_back`] for what it gets instead.
	#[tokio::test(start_paused = true)]
	async fn withheld_peer_hop_falls_back_to_assigned() {
		let assigned = crate::Hop::new(777).unwrap();
		let declared = crate::Hop::new(9).unwrap();
		let (publisher, _consumer, _routes) = echo_harness(assigned).await;

		let withheld = cluster::Peer {
			hop: Some(crate::Hop::UNKNOWN),
			cost: None,
		};
		assert!(withheld.negotiated(), "the extension is on");
		assert_eq!(publisher.exclude(&withheld), assigned, "0 names nobody, so we do");

		let absent = cluster::Peer::default();
		assert_eq!(publisher.exclude(&absent), assigned, "so does declaring nothing");

		let named = cluster::Peer {
			hop: Some(declared),
			cost: None,
		};
		assert_eq!(publisher.exclude(&named), declared, "a declared identity wins");
	}

	/// A peer that negotiated the extension MUST send a HOP_PATH on every advertisement,
	/// and one that declared 0 names itself 0 there. An arriving chain is not rewritten,
	/// so the route carries 0, the assigned identity appears nowhere in it, and the
	/// split-horizon filter has nothing to match: the peer is advertised its own route
	/// back.
	#[tokio::test(start_paused = true)]
	async fn a_declared_zero_chain_is_still_advertised_back() {
		let assigned = crate::Hop::new(777).unwrap();
		let origin = crate::origin::Info::new(crate::Hop::new(1).unwrap()).produce();
		let consumer = origin.consume();

		let publisher = Publisher::new(
			TestRuntime::new(),
			crate::lite::test_transport::SinkSession::new(Default::default()),
			origin.consume(),
			Control::new(None, false),
			Some(assigned),
			peer::PeerSetup::default(),
			Version::Draft16,
		);

		// The chain as ingress stores it: the peer named itself 0.
		let mut hops = crate::Hops::new();
		hops.push(crate::Hop::UNKNOWN).unwrap();
		let _echoed = origin
			.announce("from/peer", crate::origin::Route::default().with_hops(hops))
			.unwrap();

		let peer = cluster::Peer {
			hop: Some(crate::Hop::UNKNOWN),
			cost: None,
		};
		// The excluding cursor cannot match hop 0 (it names nobody), so the route
		// still reaches this peer's stream.
		let mut announced = consumer.excluding(publisher.exclude(&peer)).announced();
		let echoed = announced.assert_next_active("from/peer");
		assert!(
			publisher.select(&echoed, &peer).wanted(),
			"known gap: the assigned identity is not in the chain, so nothing filters it",
		);
	}

	/// A same-path source can splice into (or detach from) an existing broadcast
	/// without an origin-level (un)announce, silently flipping `advertisable`.
	/// Namespace forwarding must follow: advertise when a clean route appears,
	/// withdraw when the last one detaches.
	#[tokio::test]
	async fn namespace_follows_route_eligibility_changes() {
		let assigned = crate::Hop::new(777).unwrap();
		let clean_publisher = crate::Hop::new(778).unwrap();
		let origin = crate::origin::Info::new(crate::Hop::new(1).unwrap()).produce();

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

		// The prefix starts with only a route through the assigned peer.
		let mut tainted_hops = crate::Hops::new();
		tainted_hops.push(assigned).unwrap();
		let _tainted = origin
			.announce(
				"route-flip-cam",
				crate::origin::Route::default().with_hops(tainted_hops),
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

		// A clean route joins the same prefix: the excluded cursor now has a best
		// visible route, so the namespace must be advertised.
		let mut clean_hops = crate::Hops::new();
		clean_hops.push(clean_publisher).unwrap();
		let clean = origin
			.announce("route-flip-cam", crate::origin::Route::default().with_hops(clean_hops))
			.unwrap();
		settle().await;
		assert!(futures::poll!(run.as_mut()).is_pending());
		assert_eq!(
			occurrences(&log, b"route-flip-cam"),
			1,
			"NAMESPACE after a clean route joins"
		);

		// The clean route retracts, leaving only the tainted one: withdrawn.
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

		let origin = crate::origin::Info::new(crate::Hop::new(1).unwrap()).produce();
		let _cam = origin.announce("lonely-cam", crate::origin::Route::default()).unwrap();
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

		let origin = crate::origin::Info::new(crate::Hop::new(1).unwrap()).produce();
		let consumer = origin.consume();

		// Announced before the peer subscribes: it must only hit the wire after.
		let early = origin.announce("early-cam", crate::origin::Route::default()).unwrap();
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
		let _late = origin.announce("late-cam", crate::origin::Route::default()).unwrap();
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

		let origin = crate::origin::Info::new(crate::Hop::new(1).unwrap()).produce();
		let _local = origin.announce("local-cam", crate::origin::Route::default()).unwrap();
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

		let origin = crate::origin::Info::new(crate::Hop::new(1).unwrap()).produce();
		let _cam = origin.announce("cam", crate::origin::Route::default()).unwrap();
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

		let origin = crate::origin::Info::new(crate::Hop::new(1).unwrap()).produce();
		let first = origin.announce("first-cam", crate::origin::Route::default()).unwrap();
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
		let _second = origin.announce("second-cam", crate::origin::Route::default()).unwrap();
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
		let assigned = crate::Hop::new(777).unwrap();

		let origin = crate::origin::Info::new(crate::Hop::new(1).unwrap()).produce();

		// A route that already passed through us must never be forwarded: `select`
		// wants nothing, which is what the peer already holds.
		let mut hops = crate::Hops::new();
		hops.push(crate::Hop::new(1).unwrap()).unwrap();

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
		let mut watch = Watched::new(crate::origin::Route::default().with_hops(hops));
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

		let origin = crate::origin::Info::new(crate::Hop::new(1).unwrap()).produce();
		let cam = origin.announce("solo-cam", crate::origin::Route::default()).unwrap();
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

		// A cheaper second route makes the advertisement worth re-pricing, which is a
		// path back into the reconciliation that does not go through the retry timer.
		let _standby = origin
			.announce("solo-cam", crate::origin::Route::default().with_cost(0))
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

		let origin = crate::origin::Info::new(crate::Hop::new(1).unwrap()).produce();
		let cam = origin.announce("solo-cam", crate::origin::Route::default()).unwrap();
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

		let origin = crate::origin::Info::new(crate::Hop::new(1).unwrap()).produce();
		let _first = origin.announce("first-cam", crate::origin::Route::default()).unwrap();
		let _second = origin.announce("second-cam", crate::origin::Route::default()).unwrap();
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

		let origin = crate::origin::Info::new(crate::Hop::new(1).unwrap()).produce();
		let _cam = origin.announce("lonely-cam", crate::origin::Route::default()).unwrap();
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
		let origin = crate::origin::Info::new(crate::Hop::new(1).unwrap()).produce();
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
					filter: Filter::NextObject,
					fill: None,
					properties_wanted: true,
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
		for version in [Version::Draft17, Version::Draft18, Version::Draft19, Version::Draft20] {
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

		for version in [Version::Draft17, Version::Draft18, Version::Draft19, Version::Draft20] {
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

/// The live edge a SUBSCRIBE resolves against, snapshotted once so the subscription
/// floor, the fill cap, and the advertised LARGEST_OBJECT all agree on where it is.
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
struct LiveEdge {
	/// The newest group sequence, `None` before any group exists.
	latest: Option<u64>,
	/// The precise Largest Object. `None` when the track is empty, or when the newest
	/// group's frames cannot be read right now (none written yet, or a spliced track
	/// between segments), in which case nothing is advertised and no fill is servable.
	largest: Option<Location>,
	/// One past the Largest Object, which is where a Next Object subscription begins.
	/// When the edge is imprecise this falls back to the next group boundary: never below
	/// the true Next Object, at worst under-delivering the current group's tail.
	next: Option<Location>,
}

/// Snapshot the live edge of a track.
fn live_edge(track: &track::Consumer) -> LiveEdge {
	let Some(latest) = track.latest() else {
		return LiveEdge::default();
	};

	match track.peek_latest() {
		Some(group) if group.sequence == latest => {
			let count = group.frame_count() as u64;
			let largest = match count.checked_sub(1) {
				Some(object) => Some(Location { group: latest, object }),
				// A group with no frames yet has no objects, so the largest sits in an
				// earlier group. Walk back through the cache to find it, or a peer that
				// subscribes in the instant between a group's creation and its first
				// frame is told the track is empty and gets no fill.
				None => largest_before(track, latest),
			};
			// One past the edge, even when the edge sits below the newest group: a group
			// may keep writing after a newer one exists, and a floor above the true Next
			// Object would strand those objects between the fill cap and the
			// subscription. With no readable object anywhere, the newest group's start
			// excludes nothing the cache can still name.
			let next = match largest {
				Some(largest) => Location {
					group: largest.group,
					object: largest.object.saturating_add(1),
				},
				None => Location {
					group: latest,
					object: 0,
				},
			};
			LiveEdge {
				latest: Some(latest),
				largest,
				next: Some(next),
			}
		}
		_ => LiveEdge {
			latest: Some(latest),
			largest: None,
			next: Some(Location {
				group: latest.saturating_add(1),
				object: 0,
			}),
		},
	}
}

/// The last object below `sequence`: the nearest earlier cached group that has started a
/// frame, walked in cache order so legal gaps in the group numbering are crossed. Empty
/// groups exist for at most the instant between creation and first frame, so the walk is
/// one step in practice. A group evicted from the cache is not visible, which is fine:
/// Largest Object is the track from this publisher's perspective, and that is the cache.
fn largest_before(track: &track::Consumer, sequence: u64) -> Option<Location> {
	let mut sequence = sequence;
	loop {
		let group = track.peek_before(sequence)?;
		if let Some(object) = (group.frame_count() as u64).checked_sub(1) {
			return Some(Location {
				group: group.sequence,
				object,
			});
		}
		sequence = group.sequence;
	}
}

/// The Locations a SUBSCRIBE's Location Filter selects, resolved against the live edge.
///
/// `start: None` joins at the beginning of the latest group, which is what moq-lite means
/// by joining a live track. An explicit start is honored down to the object: the start
/// group is served from `start.object` and the end group up to `end.object`, so a filter
/// is never widened into objects the subscriber excluded.
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
struct ServeRange {
	/// The first Location to serve, or `None` for the start of the latest group.
	start: Option<Location>,
	/// Where the range ends, inclusive. `None` is open ended. The subscription stays
	/// open once the range is exhausted; draft-20 removed the notion of a filter ending
	/// a subscription.
	end: Option<EndLocation>,
}

/// The slice of one group a subscription's [`ServeRange`] selects.
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
struct GroupSlice {
	/// Frames dropped from the front; also the first written object's absolute id.
	skip: u64,
	/// One past the last object to write, when the filter ends inside this group.
	until: Option<u64>,
}

/// Resolve a SUBSCRIBE's Location Filter into the range to serve.
///
/// Only draft-20 is honored. Earlier drafts have a Filter Type tag whose absolute forms we
/// never served, and starting to interpret them now would change what an existing peer
/// receives; draft-20 is also the first version whose relative forms can name a past group
/// without the subscriber knowing Largest Object.
fn subscribe_range(msg: &ietf::Subscribe<'_>, edge: LiveEdge, version: Version) -> ServeRange {
	if !Filter::is_draft20(version) {
		if !matches!(msg.filter, Filter::NextObject | Filter::Unfiltered) {
			tracing::warn!(filter = ?msg.filter, "filter not supported before draft-20, ignoring");
		}
		return ServeRange::default();
	}

	filter_range(msg.filter, edge)
}

/// The Locations a single Location Filter selects, resolved against the live edge.
fn filter_range(filter: Filter, edge: LiveEdge) -> ServeRange {
	match filter {
		// No restriction. moq-lite starts at the beginning of the latest group, which is
		// the join point it is built around; a subscription passes objects as they are
		// published, so an absent filter is not a request to replay history.
		Filter::Unfiltered => ServeRange::default(),
		// `{Largest.Group, Largest.Object + 1}`. Everything below it, including the
		// already-published head of the current group, is outside the requested range,
		// so the join is mid-group by construction. The draft pairs this with a fill
		// when the subscriber wants the head; see `run_fill`.
		Filter::NextObject => ServeRange {
			start: edge.next,
			end: None,
		},
		// `{Largest.Group + 1 - groups, 0}`: 0 is the next group and 1 is the current one.
		// Counted from `Largest.Group`, which sits below the newest group while that
		// group has no objects yet; only with no largest at all does the newest group
		// stand in for it.
		Filter::Relative(groups) => ServeRange {
			start: edge
				.largest
				.map(|largest| largest.group)
				.or(edge.latest)
				.map(|group| Location {
					group: group.saturating_add(1).saturating_sub(groups),
					object: 0,
				}),
			end: None,
		},
		Filter::Absolute { start, end } => ServeRange {
			start: Some(start),
			end,
		},
	}
}

/// What a draft-20 fill request resolves to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FillServe {
	/// The range is empty, so no fetch stream is opened at all.
	Empty,
	/// A single group served from the cache: `skip` frames dropped from the front, and
	/// delivery stopping before `until` when set (the current group is capped at the
	/// Largest Object snapshot; a whole past group reads to its end).
	Group {
		sequence: u64,
		skip: u64,
		until: Option<u64>,
	},
	/// A range spanning several groups, which we do not serve: multi-group fetch
	/// serialization depends on a negotiated group order we do not implement, so the
	/// stream is reset instead, the draft's fill-failure signal.
	Unsupported,
}

/// Resolve a fill request using the Fetch rules: relative to Largest Object and never
/// extending beyond it. An omitted Location Filter inherits the subscription's.
fn fill_range(fill: ietf::Fill, subscription: Filter, largest: Option<Location>) -> FillServe {
	// A Range Filter narrows which objects pass, which we do not implement; serving the
	// unfiltered range instead would deliver objects the peer excluded, so refuse it.
	if fill.range_filters {
		return FillServe::Unsupported;
	}
	let filter = fill.filter.unwrap_or(subscription);

	// Nothing published (or no precise edge to cap at) means no fill is servable; an
	// empty range opens no stream.
	let Some(largest) = largest else {
		return FillServe::Empty;
	};

	let start = match filter {
		// A Fetch without a filter is the whole track up to Largest Object.
		Filter::Unfiltered => Location { group: 0, object: 0 },
		// One past the edge, which for a Fetch is always empty.
		Filter::NextObject => return FillServe::Empty,
		Filter::Relative(groups) => Location {
			group: largest.group.saturating_add(1).saturating_sub(groups),
			object: 0,
		},
		Filter::Absolute { start, .. } => start,
	};

	// Cap the requested end at Largest Object.
	let end = match filter {
		Filter::Absolute { end: Some(end), .. }
			if end.group < largest.group
				|| (end.group == largest.group && end.object.is_some_and(|object| object < largest.object)) =>
		{
			end
		}
		_ => EndLocation {
			group: largest.group,
			object: Some(largest.object),
		},
	};

	if start.group > end.group || (start.group == end.group && end.object.is_some_and(|object| object < start.object)) {
		return FillServe::Empty;
	}
	if start.group != end.group {
		return FillServe::Unsupported;
	}

	FillServe::Group {
		sequence: start.group,
		skip: start.object,
		until: end.object.map(|object| object.saturating_add(1)),
	}
}

#[cfg(test)]
mod range_tests {
	use super::*;
	use crate::ietf::EndLocation;

	fn subscribe(filter: Filter) -> ietf::Subscribe<'static> {
		ietf::Subscribe {
			request_id: RequestId(1),
			track_namespace: crate::Path::new("broadcast"),
			track_name: "video".into(),
			subscriber_priority: 128,
			group_order: GroupOrder::Descending,
			filter,
			fill: None,
			properties_wanted: true,
		}
	}

	/// A live edge of group 100 whose current group has objects 0 through 4.
	const EDGE: LiveEdge = LiveEdge {
		latest: Some(100),
		largest: Some(Location { group: 100, object: 4 }),
		next: Some(Location { group: 100, object: 5 }),
	};

	/// A start past the live edge is what the subscriber asked for, so it is used as given.
	/// Clamping it to the live edge would serve a group outside the requested range.
	#[tokio::test]
	async fn a_future_start_is_not_clamped_to_the_live_edge() {
		let mut track = track::Producer::new(std::sync::Arc::new(crate::broadcast::Info::default()), "video", None);
		track
			.create_group(group::Info { sequence: 7 })
			.unwrap()
			.finish()
			.unwrap();
		track
			.create_group(group::Info { sequence: 8 })
			.unwrap()
			.finish()
			.unwrap();

		// Next Group against a live edge of 8 asks for 9, which does not exist yet.
		let mut subscriber = track.subscribe(None);
		subscriber.start_at(9);
		assert!(
			futures::poll!(std::pin::pin!(subscriber.recv_group())).is_pending(),
			"a future start must wait for its group rather than serving the live edge"
		);

		// The group it asked for is what it gets once published.
		track
			.create_group(group::Info { sequence: 9 })
			.unwrap()
			.finish()
			.unwrap();
		let group = subscriber.recv_group().await.unwrap().expect("group 9");
		assert_eq!(group.sequence, 9);
	}

	/// Earlier drafts never had their absolute filters served, so honoring one now would
	/// change what an existing peer receives.
	#[test]
	fn older_drafts_are_ignored() {
		let msg = subscribe(Filter::Absolute {
			start: Location { group: 4, object: 0 },
			end: Some(EndLocation { group: 9, object: None }),
		});
		assert_eq!(subscribe_range(&msg, EDGE, Version::Draft19), ServeRange::default());
	}

	/// An absent filter is "no restriction on what is forwarded", not a request for
	/// history, so it joins at the live edge.
	#[test]
	fn an_unfiltered_subscription_stays_live() {
		let msg = subscribe(Filter::Unfiltered);
		assert_eq!(subscribe_range(&msg, EDGE, Version::Draft20), ServeRange::default());
	}

	/// Next Object starts one past the Largest Object, mid-group. Everything below it,
	/// including the current group's head, is outside the requested range.
	#[test]
	fn next_object_starts_past_the_largest_object() {
		let msg = subscribe(Filter::NextObject);
		assert_eq!(
			subscribe_range(&msg, EDGE, Version::Draft20),
			ServeRange {
				start: Some(Location { group: 100, object: 5 }),
				end: None,
			}
		);
	}

	/// When the edge cannot be read precisely, Next Object falls back to the next group
	/// boundary: never below the true Next Object, so nothing already published is sent.
	#[test]
	fn next_object_without_a_precise_edge_waits_for_the_next_group() {
		let edge = LiveEdge {
			latest: Some(100),
			largest: None,
			next: Some(Location { group: 101, object: 0 }),
		};
		let msg = subscribe(Filter::NextObject);
		assert_eq!(
			subscribe_range(&msg, edge, Version::Draft20),
			ServeRange {
				start: Some(Location { group: 101, object: 0 }),
				end: None,
			}
		);
	}

	/// `{Largest.Group + 1 - groups, 0}`: one is the current group, zero is the next one,
	/// and larger values reach further back.
	#[test]
	fn relative_counts_back_from_the_next_group() {
		for (groups, expected) in [(0, 101), (1, 100), (2, 99), (5, 96)] {
			let msg = subscribe(Filter::Relative(groups));
			assert_eq!(
				subscribe_range(&msg, EDGE, Version::Draft20),
				ServeRange {
					start: Some(Location {
						group: expected,
						object: 0,
					}),
					end: None,
				},
				"{groups} groups back"
			);
		}
	}

	/// Relative counts from `Largest.Group`, which is below the newest group while that
	/// group has no objects yet, so a current-group join still reaches the content.
	#[test]
	fn relative_counts_from_the_largest_group_over_an_empty_newest_group() {
		let edge = LiveEdge {
			latest: Some(1),
			largest: Some(Location { group: 0, object: 2 }),
			next: Some(Location { group: 0, object: 3 }),
		};
		let msg = subscribe(Filter::Relative(1));
		assert_eq!(
			subscribe_range(&msg, edge, Version::Draft20),
			ServeRange {
				start: Some(Location { group: 0, object: 0 }),
				end: None,
			}
		);
	}

	/// Counting back further than the track goes lands at its start rather than wrapping.
	#[test]
	fn relative_saturates_at_the_start() {
		let msg = subscribe(Filter::Relative(500));
		assert_eq!(
			subscribe_range(&msg, EDGE, Version::Draft20),
			ServeRange {
				start: Some(Location { group: 0, object: 0 }),
				end: None,
			}
		);
	}

	/// Nothing published yet means there is no edge to count back from.
	#[test]
	fn relative_without_an_edge_stays_live() {
		let msg = subscribe(Filter::Relative(3));
		assert_eq!(
			subscribe_range(&msg, LiveEdge::default(), Version::Draft20),
			ServeRange::default()
		);
	}

	/// A group created but not yet written has no objects, so the largest sits in an
	/// earlier group. Losing it would tell a fill-requesting peer the track is empty, and
	/// a floor above the true Next Object would strand a late object of the earlier group
	/// between the fill cap and the subscription: a group may keep writing after a newer
	/// one exists, so the earlier group is deliberately left unfinished here.
	#[tokio::test]
	async fn an_empty_newest_group_walks_back_for_the_largest() {
		let mut track = track::Producer::new(std::sync::Arc::new(crate::broadcast::Info::default()), "video", None);
		let mut first = track.create_group(group::Info { sequence: 0 }).unwrap();
		for _ in 0..3 {
			first
				.write_frame(crate::Timestamp::from_millis(0).unwrap(), b"frame".as_slice())
				.unwrap();
		}
		let _open = track.create_group(group::Info { sequence: 1 }).unwrap();

		let edge = live_edge(&track.consume());
		assert_eq!(edge.latest, Some(1));
		assert_eq!(
			edge.largest,
			Some(Location { group: 0, object: 2 }),
			"the largest object is the previous group's last frame"
		);
		assert_eq!(
			edge.next,
			Some(Location { group: 0, object: 3 }),
			"the floor is one past the largest, so a late object of group 0 is not stranded"
		);
	}

	/// Group numbering may legally skip sequences, so the walk follows the cache's own
	/// order rather than decrementing by one.
	#[tokio::test]
	async fn the_walkback_crosses_a_gap_in_the_numbering() {
		let mut track = track::Producer::new(std::sync::Arc::new(crate::broadcast::Info::default()), "video", None);
		let mut first = track.create_group(group::Info { sequence: 0 }).unwrap();
		first
			.write_frame(crate::Timestamp::from_millis(0).unwrap(), b"frame".as_slice())
			.unwrap();
		first.finish().unwrap();
		// Sequence 1 never exists; the newest group is empty.
		let _open = track.create_group(group::Info { sequence: 2 }).unwrap();

		let edge = live_edge(&track.consume());
		assert_eq!(edge.latest, Some(2));
		assert_eq!(edge.largest, Some(Location { group: 0, object: 0 }));
		assert_eq!(edge.next, Some(Location { group: 0, object: 1 }));
	}

	/// Both ends carry through, object bounds included, so the boundary groups can be
	/// trimmed rather than widened.
	#[test]
	fn absolute_carries_both_ends() {
		let msg = subscribe(Filter::Absolute {
			start: Location { group: 4, object: 3 },
			end: Some(EndLocation {
				group: 9,
				object: Some(6),
			}),
		});
		assert_eq!(
			subscribe_range(&msg, EDGE, Version::Draft20),
			ServeRange {
				start: Some(Location { group: 4, object: 3 }),
				end: Some(EndLocation {
					group: 9,
					object: Some(6)
				}),
			}
		);
	}
}

#[cfg(test)]
mod fill_range_tests {
	use super::*;

	/// Objects 0 through 4 of group 100 are published.
	const LARGEST: Option<Location> = Some(Location { group: 100, object: 4 });

	/// A fill with an explicit Location Filter and no range filters.
	fn fill(filter: Filter) -> ietf::Fill {
		ietf::Fill {
			filter: Some(filter),
			range_filters: false,
		}
	}

	/// The canonical current-group join: a fill one group back covers the published head
	/// of the current group, capped at the Largest Object snapshot.
	#[test]
	fn current_group_fill() {
		assert_eq!(
			fill_range(fill(Filter::Relative(1)), Filter::NextObject, LARGEST),
			FillServe::Group {
				sequence: 100,
				skip: 0,
				until: Some(5),
			}
		);
	}

	/// A fill of the next group starts past the Largest Object, which for a Fetch is
	/// always empty, as is an explicit Next Object.
	#[test]
	fn a_future_fill_is_empty() {
		assert_eq!(
			fill_range(fill(Filter::Relative(0)), Filter::NextObject, LARGEST),
			FillServe::Empty
		);
		assert_eq!(
			fill_range(fill(Filter::NextObject), Filter::NextObject, LARGEST),
			FillServe::Empty
		);
	}

	/// Nothing published means every fill range is empty; no stream is owed.
	#[test]
	fn no_content_means_no_fill() {
		assert_eq!(
			fill_range(fill(Filter::Relative(1)), Filter::NextObject, None),
			FillServe::Empty
		);
		assert_eq!(
			fill_range(fill(Filter::Unfiltered), Filter::NextObject, None),
			FillServe::Empty
		);
	}

	/// A whole past group is served to its end; only the current group is capped.
	#[test]
	fn a_past_group_is_served_whole() {
		assert_eq!(
			fill_range(
				fill(Filter::Absolute {
					start: Location { group: 7, object: 0 },
					end: Some(EndLocation { group: 7, object: None }),
				}),
				Filter::NextObject,
				LARGEST
			),
			FillServe::Group {
				sequence: 7,
				skip: 0,
				until: None,
			}
		);
	}

	/// Object bounds inside the group carry through to the served slice.
	#[test]
	fn object_bounds_trim_the_group() {
		assert_eq!(
			fill_range(
				fill(Filter::Absolute {
					start: Location { group: 7, object: 2 },
					end: Some(EndLocation {
						group: 7,
						object: Some(5)
					}),
				}),
				Filter::NextObject,
				LARGEST
			),
			FillServe::Group {
				sequence: 7,
				skip: 2,
				until: Some(6),
			}
		);
	}

	/// An end past the edge is capped at the Largest Object, per the Fetch rules.
	#[test]
	fn the_end_is_capped_at_the_largest_object() {
		assert_eq!(
			fill_range(
				fill(Filter::Absolute {
					start: Location { group: 100, object: 0 },
					end: Some(EndLocation {
						group: 100,
						object: Some(1000),
					}),
				}),
				Filter::NextObject,
				LARGEST
			),
			FillServe::Group {
				sequence: 100,
				skip: 0,
				until: Some(5),
			}
		);
	}

	/// A range spanning several groups is refused rather than served in an order the
	/// peer may not expect; the reset is the draft's fill-failure signal.
	#[test]
	fn a_multi_group_fill_is_unsupported() {
		assert_eq!(
			fill_range(fill(Filter::Relative(3)), Filter::NextObject, LARGEST),
			FillServe::Unsupported
		);
		assert_eq!(
			fill_range(fill(Filter::Unfiltered), Filter::NextObject, LARGEST),
			FillServe::Unsupported
		);
		assert_eq!(
			fill_range(
				fill(Filter::Absolute {
					start: Location { group: 7, object: 0 },
					end: Some(EndLocation { group: 9, object: None }),
				}),
				Filter::NextObject,
				LARGEST
			),
			FillServe::Unsupported
		);
	}

	/// A Range Filter narrows which objects pass; refusing beats serving objects the
	/// peer excluded.
	#[test]
	fn a_range_filtered_fill_is_unsupported() {
		let fill = ietf::Fill {
			filter: Some(Filter::Relative(1)),
			range_filters: true,
		};
		assert_eq!(fill_range(fill, Filter::NextObject, LARGEST), FillServe::Unsupported);
	}

	/// An omitted Location Filter inherits the subscription's, per the draft: a fill
	/// scope carries only the settings that differ.
	#[test]
	fn an_omitted_filter_inherits_the_subscription() {
		let empty = ietf::Fill::default();
		// A Next Object subscription inherited into a Fetch is always empty.
		assert_eq!(fill_range(empty, Filter::NextObject, LARGEST), FillServe::Empty);
		// A current-group subscription inherited into the fill covers its head.
		assert_eq!(
			fill_range(empty, Filter::Relative(1), LARGEST),
			FillServe::Group {
				sequence: 100,
				skip: 0,
				until: Some(5),
			}
		);
	}

	/// A backwards range is empty, not an error.
	#[test]
	fn a_backwards_range_is_empty() {
		assert_eq!(
			fill_range(
				fill(Filter::Absolute {
					start: Location { group: 7, object: 5 },
					end: Some(EndLocation {
						group: 7,
						object: Some(2)
					}),
				}),
				Filter::NextObject,
				LARGEST
			),
			FillServe::Empty
		);
	}

	/// The whole track fits in one group only when the track has exactly one group.
	#[test]
	fn unfiltered_with_one_group_is_the_canonical_fill() {
		assert_eq!(
			fill_range(
				fill(Filter::Unfiltered),
				Filter::NextObject,
				Some(Location { group: 0, object: 9 })
			),
			FillServe::Group {
				sequence: 0,
				skip: 0,
				until: Some(10),
			}
		);
	}
}
