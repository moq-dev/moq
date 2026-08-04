use crate::{group, origin, track};
use std::{collections::HashMap, task::Poll};

use futures::{FutureExt, StreamExt, stream::FuturesUnordered};
use web_transport_trait::SendStream;

use crate::{
	AsPath, Error, Timescale,
	coding::{Stream, Writer},
	ietf::{self, Control, FetchHeader, FetchType, FilterType, GroupOrder, Location, RequestId},
	track::Subscription,
	util::{MaybeBoxedExt, MaybeSendBox},
};

use super::{Message, Version, cluster};

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
	idle_at: Option<web_async::time::Instant>,
	/// Set once the broadcast errors, so a dead entry stops being polled.
	dead: bool,
}

impl Watched {
	fn new(broadcast: crate::broadcast::Consumer) -> Self {
		Self {
			demand: broadcast.demand(),
			broadcast,
			sent: Advert::None,
			idle_at: None,
			dead: false,
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
}

#[derive(Clone)]
pub(super) struct Publisher<S: web_transport_trait::Session> {
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
	peer_setup: cluster::PeerSetup,
	version: Version,
}

impl<S: web_transport_trait::Session> Publisher<S> {
	pub fn new(
		session: S,
		origin: origin::Consumer,
		control: Control,
		peer_origin: Option<crate::Origin>,
		peer_setup: cluster::PeerSetup,
		version: Version,
	) -> Self {
		Self {
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
			true => self.peer_setup.get().await,
			false => cluster::Peer::default(),
		}
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

			let cost = crate::broadcast::outgoing_cost(&watch.demand, route, serving);
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
		fired: Option<web_async::time::Instant>,
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
	fn linger_deadline(watched: &HashMap<crate::PathOwned, Watched>) -> Option<web_async::time::Instant> {
		watched
			.values()
			.filter_map(|watch| watch.idle_at)
			.min()
			.map(|at| at + crate::broadcast::COST_LINGER)
	}

	pub async fn run(self) -> Result<(), Error> {
		self.run_announce().await
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
				self.write_subscribe_error(&mut stream.writer, request_id, 404, "Broadcast not found")
					.await?;
				return Ok(());
			}
		};

		let subscription = Subscription {
			priority: msg.subscriber_priority,
			..Default::default()
		};

		let track = match async { broadcast.track(&msg.track_name)?.subscribe(subscription).await }.await {
			Ok(track) => track,
			Err(err) => {
				self.write_subscribe_error(&mut stream.writer, request_id, 404, &err.to_string())
					.await?;
				return Ok(());
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
				// Declaring the timescale is what opts the track into timestamps; every
				// object Timestamp below is in these units.
				timescale: Some(track.info().timescale),
			})
			.await?;

		// Run the track, cancelling on reader close (Unsubscribe or stream close)
		let res = {
			let mut serve = std::pin::pin!(self.run_track(track, request_id));
			let mut reader_closed = std::pin::pin!(stream.reader.closed());
			let mut session_closed = std::pin::pin!(self.session.closed());
			kio::wait(|waiter| {
				if let Poll::Ready(res) = waiter.poll_future(serve.as_mut()) {
					return Poll::Ready(res);
				}
				if waiter.poll_future(reader_closed.as_mut()).is_ready()
					|| waiter.poll_future(session_closed.as_mut()).is_ready()
				{
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

		stream.writer.finish().ok();

		res
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

	/// Serve a track using FuturesUnordered for unlimited concurrent groups.
	async fn run_track(&self, mut track: track::Subscriber, request_id: RequestId) -> Result<(), Error> {
		let mut tasks = FuturesUnordered::new();

		loop {
			// Await the next group while driving the in-flight group futures.
			let group = {
				kio::wait(|waiter| {
					let mut cx = std::task::Context::from_waker(waiter.waker());
					while let std::task::Poll::Ready(Some(())) = tasks.poll_next_unpin(&mut cx) {}
					track.poll_recv_group(waiter)
				})
				.await
			};

			let Some(group) = group? else {
				// Track finished: drain the in-flight group futures, then FIN.
				while tasks.next().await.is_some() {}
				return Ok(());
			};

			let sequence = group.sequence;
			tracing::debug!(subscribe = %request_id, track = %track.name(), sequence, "serving group");

			let msg = ietf::GroupHeader {
				track_alias: request_id.0,
				group_id: sequence,
				sub_group_id: 0,
				publisher_priority: 0,
				// Carry per-object timestamps as extension headers (the Timestamp Object
				// Property) so moq-transport peers get the real PTS. The units are the
				// track's, declared once in SUBSCRIBE_OK.
				flags: ietf::GroupFlags {
					has_extensions: true,
					..Default::default()
				},
			};

			let priority = track.subscription().priority;
			let timescale = track.info().timescale;
			tasks
				.push(Self::run_group(self.session.clone(), msg, priority, group, timescale, self.version).map(|_| ()));
		}
	}

	async fn run_group(
		session: S,
		msg: ietf::GroupHeader,
		priority: u8,
		mut group: group::Consumer,
		timescale: Timescale,
		version: Version,
	) -> Result<(), Error> {
		let mut stream = session.open_uni().await.map_err(Error::from_transport)?;
		stream.set_priority(priority);

		let mut stream = Writer::new(stream, version);

		stream.encode(&msg).await?;

		loop {
			// Wait for the next frame, bailing if the peer closes the stream first.
			let frame = {
				let mut closed = std::pin::pin!(stream.closed());
				kio::wait(|waiter| {
					if waiter.poll_future(closed.as_mut()).is_ready() {
						return Poll::Ready(Err(Error::Cancel));
					}
					group.poll_next_frame(waiter)
				})
				.await
			};

			let mut frame = match frame? {
				Some(frame) => frame,
				None => break,
			};

			// object id delta is always 0.
			stream.encode(&0u64).await?;

			// Per-object extension headers carry the frame's presentation timestamp.
			if msg.flags.has_extensions {
				let mut ext = bytes::BytesMut::new();
				ietf::encode_object_time(&mut ext, frame.timestamp, timescale, version)?;
				stream.encode(&(ext.len() as u64)).await?;
				stream.write_chunk(ext.freeze()).await?;
			}

			// Write the size of the frame.
			stream.encode(&frame.size).await?;

			if frame.size == 0 {
				// Have to write the object status too.
				stream.encode(&0u8).await?;
			} else {
				// Stream each chunk of the frame.
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
						Some(chunk) => {
							stream.write_chunk(chunk).await?;
						}
						None => break,
					}
				}
			}
		}

		stream.finish()?;

		// Wait until everything is acknowledged by the peer so we can still cancel the stream.
		stream.closed().await?;

		tracing::debug!(sequence = %msg.group_id, "finished group");

		Ok(())
	}

	/// Handle a FETCH on its bidi stream.
	async fn run_fetch_stream(self, mut stream: Stream<S, Version>, msg: ietf::Fetch<'_>) -> Result<(), Error> {
		let _subscribe_id = match msg.fetch_type {
			FetchType::Standalone { .. } => {
				self.write_fetch_error(&mut stream.writer, msg.request_id, 500, "not supported")
					.await?;
				return Ok(());
			}
			FetchType::RelativeJoining {
				subscriber_request_id,
				group_offset,
			} => {
				if group_offset != 0 {
					self.write_fetch_error(&mut stream.writer, msg.request_id, 500, "not supported")
						.await?;
					return Ok(());
				}
				subscriber_request_id
			}
			FetchType::AbsoluteJoining { .. } => {
				self.write_fetch_error(&mut stream.writer, msg.request_id, 500, "not supported")
					.await?;
				return Ok(());
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
		writer.finish()?;
		writer.closed().await?;

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

	/// Outgoing PublishNamespace: announce each namespace via a bidi stream.
	async fn run_announce(self) -> Result<(), Error> {
		// Each accepted namespace holds a `publisher()` announce guard (bumps
		// `announced` / `announced_closed`) alongside its stream, so dropping the
		// tuple on unannounce or cleanup records the close.
		let mut namespace_streams: HashMap<crate::PathOwned, (RequestId, Stream<S, Version>)> = HashMap::new();
		let mut announced = self.origin.announced();
		let mut watched: HashMap<crate::PathOwned, Watched> = HashMap::new();

		// The extension changes what a PUBLISH_NAMESPACE carries, so nothing can be
		// advertised until the peer's SETUP says whether it speaks it.
		let peer = self.peer().await;
		let mut linger = kio::time::Deadline::new();

		loop {
			linger.set(Self::linger_deadline(&watched));

			// Wait for the next (un)announce, watched route change, or cost restore,
			// bailing once the session dies.
			let event = {
				let mut closed = std::pin::pin!(self.session.closed());
				kio::wait(|waiter| {
					if waiter.poll_future(closed.as_mut()).is_ready() {
						return Poll::Ready(NamespaceEvent::Closed(Ok(())));
					}
					if let Poll::Ready(update) = announced.poll_next(waiter) {
						return Poll::Ready(NamespaceEvent::Update(update));
					}
					// Stamped per poll rather than kept: the turn always ends in a
					// `Ready` below once it fires, so it never has to survive.
					let fired = linger.poll(waiter).is_ready().then(web_async::time::Instant::now);
					match Self::poll_watched(&mut watched, fired, waiter) {
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
				NamespaceEvent::Closed(res) => return res,
				NamespaceEvent::Linger => continue,
				NamespaceEvent::Update(None) => break,
				NamespaceEvent::Update(Some(crate::announce::Update { path, broadcast })) => {
					let suffix = path.to_owned();
					match broadcast {
						Some(broadcast) => {
							watched.insert(suffix.clone(), Watched::new(broadcast));
							self.sync_publish_namespace(&suffix, &peer, &mut watched, &mut namespace_streams)
								.await?;
						}
						None => {
							watched.remove(&suffix);
							// A no-op for a namespace that was never announced (no stream
							// to tear down).
							self.unannounce_namespace(&suffix, &mut namespace_streams).await;
						}
					}
				}
				NamespaceEvent::Routes(suffix) => {
					self.sync_publish_namespace(&suffix, &peer, &mut watched, &mut namespace_streams)
						.await?;
				}
				NamespaceEvent::Idle(suffix) => {
					if let Some(watch) = watched.get_mut(&suffix) {
						watch.idle_at = Some(web_async::time::Instant::now());
					}
				}
			}
		}

		// Clean up remaining streams
		let suffixes: Vec<crate::PathOwned> = namespace_streams.keys().cloned().collect();
		for suffix in suffixes {
			self.unannounce_namespace(&suffix, &mut namespace_streams).await;
		}

		Ok(())
	}

	/// Bring the peer's view of one namespace in line with the current selection.
	///
	/// A namespace that became advertisable opens its stream, one that stopped tears it
	/// down, and one whose path or cost moved is updated by re-sending PUBLISH_NAMESPACE
	/// **on the stream that already carries it**: opening a second stream would leave
	/// two claiming one namespace and let the superseded one retract its replacement.
	async fn sync_publish_namespace(
		&self,
		suffix: &crate::PathOwned,
		peer: &cluster::Peer,
		watched: &mut HashMap<crate::PathOwned, Watched>,
		namespace_streams: &mut HashMap<crate::PathOwned, (RequestId, Stream<S, Version>)>,
	) -> Result<(), Error> {
		let Some(watch) = watched.get(suffix) else {
			return Ok(());
		};
		let advert = self.select(watch, peer);
		if advert == watch.sent {
			return Ok(());
		}

		match (advert.wanted(), namespace_streams.get_mut(suffix)) {
			(false, _) => self.unannounce_namespace(suffix, namespace_streams).await,
			(true, Some((request_id, stream))) => {
				tracing::debug!(broadcast = %self.origin.absolute(suffix), "announce update");
				let request_id = *request_id;
				stream.writer.encode(&ietf::PublishNamespace::ID).await?;
				stream
					.writer
					.encode(&ietf::PublishNamespace {
						request_id,
						track_namespace: suffix.as_path(),
						cluster: advert.params(),
					})
					.await?;
			}
			(true, None) => {
				self.announce_namespace(suffix.clone(), advert.params(), namespace_streams)
					.await?
			}
		}

		// The peer can reject a fresh PUBLISH_NAMESPACE, which leaves no stream behind.
		// Record what it actually holds, so a later route change retries instead of
		// believing the namespace is already advertised.
		let sent = match namespace_streams.contains_key(suffix) {
			true => advert,
			false => Advert::None,
		};
		if let Some(watch) = watched.get_mut(suffix) {
			watch.sent = sent;
		}
		Ok(())
	}

	/// Bring the peer's view of one namespace in line with the current selection, on the
	/// SUBSCRIBE_NAMESPACE response stream.
	///
	/// A namespace whose path or cost moved is updated by re-sending NAMESPACE on the
	/// same stream; the receiver treats the repeat as a replacement rather than a
	/// duplicate. One that stopped being advertisable gets a NAMESPACE_DONE, which
	/// carries no cluster state of its own.
	async fn sync_namespace<W: SendStream>(
		&self,
		suffix: &crate::PathOwned,
		peer: &cluster::Peer,
		watched: &mut HashMap<crate::PathOwned, Watched>,
		writer: &mut Writer<W, Version>,
	) -> Result<(), Error> {
		let Some(watch) = watched.get(suffix) else {
			return Ok(());
		};
		let advert = self.select(watch, peer);
		if advert == watch.sent {
			return Ok(());
		}

		let absolute = self.origin.absolute(suffix).to_owned();
		match (advert.wanted(), watch.sent.wanted()) {
			(true, _) => {
				tracing::debug!(broadcast = %absolute, "namespace");
				writer.encode(&ietf::Namespace::ID).await?;
				writer
					.encode(&ietf::Namespace {
						suffix: suffix.as_path(),
						cluster: advert.params(),
					})
					.await?;
			}
			(false, true) => {
				tracing::debug!(broadcast = %absolute, "namespace_done");
				writer.encode(&ietf::NamespaceDone::ID).await?;
				writer
					.encode(&ietf::NamespaceDone {
						suffix: suffix.as_path(),
					})
					.await?;
			}
			// Never advertised and still not advertisable: nothing to say.
			(false, false) => {}
		}

		if let Some(watch) = watched.get_mut(suffix) {
			watch.sent = advert;
		}
		Ok(())
	}

	/// Open a bidi stream and send a PublishNamespace, recording the stream for later teardown.
	async fn announce_namespace(
		&self,
		suffix: crate::PathOwned,
		cluster: Option<cluster::Advert>,
		namespace_streams: &mut HashMap<crate::PathOwned, (RequestId, Stream<S, Version>)>,
	) -> Result<(), Error> {
		let absolute = self.origin.absolute(&suffix).to_owned();
		tracing::debug!(broadcast = %absolute, "announce");

		let request_id = self.control.next_request_id().await?;
		let mut stream = Stream::open(&self.session, self.version).await?;

		stream.writer.encode(&ietf::PublishNamespace::ID).await?;
		stream
			.writer
			.encode(&ietf::PublishNamespace {
				request_id,
				track_namespace: suffix.as_path(),
				cluster,
			})
			.await?;

		let type_id: u64 = stream.reader.decode().await?;
		let size: u16 = stream.reader.decode().await?;
		let mut data = stream.reader.read_exact(size as usize).await?;

		match (self.version, type_id) {
			(Version::Draft14, ietf::PublishNamespaceOk::ID) => {
				let msg = ietf::PublishNamespaceOk::decode_msg(&mut data, self.version)?;
				tracing::debug!(message = ?msg, "publish namespace ok");
				namespace_streams.insert(suffix, (request_id, stream));
			}
			(Version::Draft14, ietf::PublishNamespaceError::ID) => {
				let msg = ietf::PublishNamespaceError::decode_msg(&mut data, self.version)?;
				tracing::warn!(message = ?msg, "publish namespace error");
			}
			(_, ietf::RequestOk::ID) => {
				let msg = ietf::RequestOk::decode_msg(&mut data, self.version)?;
				tracing::debug!(message = ?msg, "publish namespace ok");
				namespace_streams.insert(suffix, (request_id, stream));
			}
			(_, ietf::RequestError::ID) => {
				let msg = ietf::RequestError::decode_msg(&mut data, self.version)?;
				tracing::warn!(message = ?msg, "publish namespace error");
			}
			_ => return Err(Error::UnexpectedMessage),
		}

		Ok(())
	}

	/// Tear down the namespace stream for a suffix, sending PublishNamespaceDone where required.
	async fn unannounce_namespace(
		&self,
		suffix: &crate::PathOwned,
		namespace_streams: &mut HashMap<crate::PathOwned, (RequestId, Stream<S, Version>)>,
	) {
		tracing::debug!(broadcast = %self.origin.absolute(suffix), "unannounce");
		if let Some((request_id, mut stream)) = namespace_streams.remove(suffix) {
			// v14-16 sends PublishNamespaceDone; v17+ just closes the stream.
			match self.version {
				Version::Draft14 | Version::Draft15 | Version::Draft16 => {
					let _ = stream
						.writer
						.encode_message(&ietf::PublishNamespaceDone {
							track_namespace: suffix.as_path(),
							request_id,
						})
						.await;
				}
				_ => {}
			}
			stream.writer.finish().ok();
		}
	}

	/// Handle a SUBSCRIBE_NAMESPACE on its bidi stream.
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

		match self.version {
			// v14/v15: Namespace/NamespaceDone don't exist. After OK, the publisher
			// sends PUBLISH_NAMESPACE/PUBLISH_NAMESPACE_DONE as separate control
			// stream messages (handled by run_announce). Just wait for stream close.
			Version::Draft14 | Version::Draft15 => {
				return stream.reader.closed().await;
			}
			// v16+: Send Namespace/NamespaceDone entries on this bidi stream.
			_ => {
				let mut announced = origin.announced();
				let mut watched: HashMap<crate::PathOwned, Watched> = HashMap::new();

				// The extension changes the NAMESPACE encoding, so nothing can be sent
				// until the peer's SETUP says whether it speaks it.
				let peer = self.peer().await;
				let mut linger = kio::time::Deadline::new();

				// Send initial NAMESPACE messages for currently active namespaces.
				while let Some(crate::announce::Update { path, broadcast }) = announced.try_next() {
					if let Some(broadcast) = broadcast {
						let suffix = path
							.strip_prefix(&prefix)
							.expect("origin returned invalid path")
							.to_owned();
						watched.insert(suffix.clone(), Watched::new(broadcast));
						self.sync_namespace(&suffix, &peer, &mut watched, &mut stream.writer)
							.await?;
					}
				}

				// Stream updates (origin (un)announces plus watched route and demand
				// changes), bailing if the peer closes its side first.
				loop {
					linger.set(Self::linger_deadline(&watched));

					let event = {
						let mut closed = std::pin::pin!(stream.reader.closed());
						kio::wait(|waiter| {
							if let Poll::Ready(res) = waiter.poll_future(closed.as_mut()) {
								return Poll::Ready(NamespaceEvent::Closed(res));
							}
							if let Poll::Ready(update) = announced.poll_next(waiter) {
								return Poll::Ready(NamespaceEvent::Update(update));
							}
							let fired = linger.poll(waiter).is_ready().then(web_async::time::Instant::now);
							match Self::poll_watched(&mut watched, fired, waiter) {
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
						NamespaceEvent::Closed(res) => return res,
						NamespaceEvent::Linger => continue,
						NamespaceEvent::Update(None) => {
							stream.writer.finish()?;
							return stream.writer.closed().await;
						}
						NamespaceEvent::Update(Some(crate::announce::Update { path, broadcast })) => {
							let suffix = path
								.strip_prefix(&prefix)
								.expect("origin returned invalid path")
								.to_owned();

							match broadcast {
								Some(broadcast) => {
									watched.insert(suffix.clone(), Watched::new(broadcast));
									self.sync_namespace(&suffix, &peer, &mut watched, &mut stream.writer)
										.await?;
								}
								None => {
									// Only close out namespaces the peer actually saw.
									let sent = watched.remove(&suffix).is_some_and(|watch| watch.sent.wanted());
									if sent {
										tracing::debug!(broadcast = %origin.absolute(&path), "namespace_done");
										stream.writer.encode(&ietf::NamespaceDone::ID).await?;
										stream.writer.encode(&ietf::NamespaceDone { suffix }).await?;
									}
								}
							}
						}
						NamespaceEvent::Routes(suffix) => {
							self.sync_namespace(&suffix, &peer, &mut watched, &mut stream.writer)
								.await?;
						}
						NamespaceEvent::Idle(suffix) => {
							if let Some(watch) = watched.get_mut(&suffix) {
								watch.idle_at = Some(web_async::time::Instant::now());
							}
						}
					}
				}
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::lite::test_transport::SinkSession;

	async fn settle() {
		tokio::time::sleep(std::time::Duration::from_millis(1)).await;
	}

	fn occurrences(log: &crate::lite::test_transport::Log, needle: &[u8]) -> usize {
		let writes = log.writes.lock().unwrap();
		writes.windows(needle.len()).filter(|window| *window == needle).count()
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
			session,
			origin.consume(),
			Control::new(None, false),
			Some(assigned),
			cluster::PeerSetup::default(),
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
		tokio::time::sleep(std::time::Duration::from_millis(1)).await;

		let peer = cluster::Peer::default();

		let echoed = consumer.get_broadcast("from/peer").unwrap();
		assert!(!publisher.select(&Watched::new(echoed), &peer).wanted());

		let local = consumer.get_broadcast("from/us").unwrap();
		assert_eq!(publisher.select(&Watched::new(local), &peer), Advert::Plain);
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
			session.clone(),
			origin.consume(),
			Control::new(None, false),
			Some(assigned),
			cluster::PeerSetup::default(),
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

		let stream = Stream::open(&session, Version::Draft16).await.unwrap();
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
}
