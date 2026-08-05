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

		let subscription = Subscription {
			priority: msg.subscriber_priority,
			..Default::default()
		};

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

	/// Reject a SUBSCRIBE, ending the request stream.
	///
	/// Takes the whole stream because the finish is the half that makes the error arrive:
	/// [`Writer`] resets on drop, and a reset that races the write discards the bytes the peer
	/// has not read yet, so the subscriber sees no response and waits forever. Finishing first
	/// leaves that reset a no-op. The rejection is the whole exchange, so don't wait on the
	/// peer's close.
	async fn reject_subscribe(
		&self,
		mut stream: Stream<S, Version>,
		request_id: RequestId,
		error_code: u64,
		reason: &str,
	) -> Result<(), Error> {
		self.write_subscribe_error(&mut stream.writer, request_id, error_code, reason)
			.await?;
		let _ = stream.writer.finish();
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

	/// Reject a FETCH, ending the request stream. See [`Self::reject_subscribe`] for why the
	/// finish is not optional.
	async fn reject_fetch(
		&self,
		mut stream: Stream<S, Version>,
		request_id: RequestId,
		error_code: u64,
		reason: &str,
	) -> Result<(), Error> {
		self.write_fetch_error(&mut stream.writer, request_id, error_code, reason)
			.await?;
		let _ = stream.writer.finish();
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
	/// Draft-16+ re-sends NAMESPACE on the SUBSCRIBE_NAMESPACE stream (the receiver
	/// treats a repeat as a replacement) and retracts with NAMESPACE_DONE.
	/// Draft-14/15 carry each advertisement on its own PUBLISH_NAMESPACE request:
	/// an update re-sends PUBLISH_NAMESPACE **on the stream that already carries
	/// it**, since a second stream would leave two claiming one namespace, and a
	/// withdrawal closes the request with PUBLISH_NAMESPACE_DONE.
	async fn sync_namespace(
		&self,
		suffix: &crate::PathOwned,
		path: &crate::PathOwned,
		peer: &cluster::Peer,
		watched: &mut HashMap<crate::PathOwned, Watched>,
		requests: &mut HashMap<crate::PathOwned, NamespaceRequest<S>>,
		stream: &mut Stream<S, Version>,
	) -> Result<(), Error> {
		let Some(watch) = watched.get(suffix) else {
			return Ok(());
		};
		let advert = self.select(watch, peer);
		if advert == watch.sent {
			return Ok(());
		}
		let held = watch.sent.wanted();

		let absolute = self.origin.absolute(path).to_owned();
		let sent = match self.version {
			Version::Draft14 | Version::Draft15 => {
				match (advert.wanted(), requests.get_mut(suffix)) {
					(false, _) => {
						if held {
							tracing::debug!(broadcast = %absolute, "namespace_done");
						}
						self.withdraw_namespace(stream, requests, suffix.clone()).await?;
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
						tracing::debug!(broadcast = %absolute, "namespace");
						self.advertise_namespace(requests, path, suffix.clone(), advert.params())
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
			_ => {
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
			watch.set_sent(sent);
		}
		Ok(())
	}

	/// Open a PUBLISH_NAMESPACE request for one namespace (draft-14/15, which have
	/// no NAMESPACE message), recording it in `requests` so an update or withdrawal
	/// reuses the same stream. A declined request records nothing.
	async fn advertise_namespace(
		&self,
		requests: &mut HashMap<crate::PathOwned, NamespaceRequest<S>>,
		path: &crate::PathOwned,
		suffix: crate::PathOwned,
		cluster: Option<cluster::Advert>,
	) -> Result<(), Error> {
		let request_id = self.control.next_request_id().await?;
		let mut request = Stream::open(&self.session, self.version).await?;

		request.writer.encode(&ietf::PublishNamespace::ID).await?;
		request
			.writer
			.encode(&ietf::PublishNamespace {
				request_id,
				track_namespace: path.as_path(),
				cluster,
			})
			.await?;

		let type_id: u64 = request.reader.decode().await?;
		let size: u16 = request.reader.decode().await?;
		let mut data = request.reader.read_exact(size as usize).await?;

		match (self.version, type_id) {
			(Version::Draft14, ietf::PublishNamespaceOk::ID) => {
				let msg = ietf::PublishNamespaceOk::decode_msg(&mut data, self.version)?;
				tracing::debug!(message = ?msg, "publish namespace ok");
			}
			(Version::Draft14, ietf::PublishNamespaceError::ID) => {
				let msg = ietf::PublishNamespaceError::decode_msg(&mut data, self.version)?;
				tracing::warn!(message = ?msg, "publish namespace error");
				return Ok(());
			}
			(_, ietf::RequestOk::ID) => {
				let msg = ietf::RequestOk::decode_msg(&mut data, self.version)?;
				tracing::debug!(message = ?msg, "publish namespace ok");
			}
			(_, ietf::RequestError::ID) => {
				let msg = ietf::RequestError::decode_msg(&mut data, self.version)?;
				tracing::warn!(message = ?msg, "publish namespace error");
				return Ok(());
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
		Ok(())
	}

	/// Withdraw an advertised namespace: NAMESPACE_DONE inline on draft-16+, or
	/// PUBLISH_NAMESPACE_DONE closing its own request on draft-14/15.
	async fn withdraw_namespace(
		&self,
		stream: &mut Stream<S, Version>,
		requests: &mut HashMap<crate::PathOwned, NamespaceRequest<S>>,
		suffix: crate::PathOwned,
	) -> Result<(), Error> {
		match self.version {
			Version::Draft14 | Version::Draft15 => {
				if let Some(mut request) = requests.remove(&suffix) {
					// Best effort: the peer may already be gone.
					let _ = request
						.stream
						.writer
						.encode_message(&ietf::PublishNamespaceDone {
							track_namespace: request.path.as_path(),
							request_id: request.request_id,
						})
						.await;
					request.stream.writer.finish().ok();
				}
			}
			_ => {
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

	/// Close out every open draft-14/15 PUBLISH_NAMESPACE request. A no-op on
	/// later drafts, whose entries ride the SUBSCRIBE_NAMESPACE stream itself.
	async fn withdraw_requests(
		&self,
		stream: &mut Stream<S, Version>,
		requests: &mut HashMap<crate::PathOwned, NamespaceRequest<S>>,
	) {
		let suffixes: Vec<crate::PathOwned> = requests.keys().cloned().collect();
		for suffix in suffixes {
			let _ = self.withdraw_namespace(stream, requests, suffix).await;
		}
	}

	/// Handle a SUBSCRIBE_NAMESPACE on its bidi stream.
	///
	/// Namespaces are only advertised in response to one of these, and all the
	/// announce state is local to this task (mirroring `lite::Publisher`'s
	/// announce handling): whatever this subscription advertised is withdrawn
	/// when its stream ends.
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

		let mut announced = origin.announced();
		let mut watched: HashMap<crate::PathOwned, Watched> = HashMap::new();
		// Draft-14/15: the open PUBLISH_NAMESPACE request carrying each advertised
		// namespace. Empty on later drafts, whose entries ride `stream` inline.
		let mut requests: HashMap<crate::PathOwned, NamespaceRequest<S>> = HashMap::new();

		// The extension changes what an advertisement carries, so nothing can be
		// sent until the peer's SETUP says whether it speaks it.
		let peer = self.peer().await;
		let mut linger = kio::time::Deadline::new();

		// Stream updates (origin (un)announces plus watched route and demand
		// changes), bailing if the peer closes its side first.
		let res = loop {
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
				NamespaceEvent::Closed(res) => break res,
				NamespaceEvent::Linger => continue,
				NamespaceEvent::Update(None) => {
					// The origin is gone: withdraw everything, then finish the
					// stream and wait for delivery.
					self.withdraw_requests(&mut stream, &mut requests).await;
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
							watched.insert(suffix.clone(), Watched::new(broadcast));
							self.sync_namespace(&suffix, &path, &peer, &mut watched, &mut requests, &mut stream)
								.await?;
						}
						None => {
							// Only close out namespaces the peer actually saw.
							let held = watched.remove(&suffix).is_some_and(|watch| watch.sent.wanted());
							if held {
								tracing::debug!(broadcast = %self.origin.absolute(&path), "namespace_done");
								self.withdraw_namespace(&mut stream, &mut requests, suffix).await?;
							}
						}
					}
				}
				NamespaceEvent::Routes(suffix) => {
					let path = prefix.join(&suffix);
					self.sync_namespace(&suffix, &path, &peer, &mut watched, &mut requests, &mut stream)
						.await?;
				}
				NamespaceEvent::Idle(suffix) => {
					if let Some(watch) = watched.get_mut(&suffix) {
						watch.idle_at = Some(web_async::time::Instant::now());
					}
				}
			}
		};

		// This subscription's advertisements die with it.
		self.withdraw_requests(&mut stream, &mut requests).await;

		res
	}
}

/// One draft-14/15 advertisement: the PUBLISH_NAMESPACE request it rode on and
/// what closes it out with PUBLISH_NAMESPACE_DONE.
struct NamespaceRequest<S: web_transport_trait::Session> {
	path: crate::PathOwned,
	request_id: RequestId,
	stream: Stream<S, Version>,
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
		watch.idle_at = Some(web_async::time::Instant::now());
		let watched = HashMap::from([(crate::Path::new("cam").to_owned(), watch)]);
		assert!(Publisher::<SinkSession>::linger_deadline(&watched).is_some());

		// A re-priced advertisement has a cost to advertise, so there is nothing left
		// to restore.
		let mut watch = watched.into_values().next().unwrap();
		watch.set_sent(Advert::Cluster(cluster::Advert {
			hops: cluster::HopPath::new(hops),
			cost: 9,
		}));
		let watched = HashMap::from([(crate::Path::new("cam").to_owned(), watch)]);
		assert_eq!(
			Publisher::<SinkSession>::linger_deadline(&watched),
			None,
			"a non-discounted advert must not leave a deadline behind"
		);

		// So does one that stopped being advertisable at all.
		let mut watch = watched.into_values().next().unwrap();
		watch.idle_at = Some(web_async::time::Instant::now());
		watch.set_sent(Advert::None);
		let watched = HashMap::from([(crate::Path::new("cam").to_owned(), watch)]);
		assert_eq!(Publisher::<SinkSession>::linger_deadline(&watched), None);
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

	/// The peer's OK to a PUBLISH_NAMESPACE, framed exactly as the announce path
	/// reads it -- built with the crate's own writer so the framing can't drift
	/// from the encoder under test.
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
			_ => {
				writer.encode(&ietf::RequestOk::ID).await.unwrap();
				writer
					.encode(&ietf::RequestOk {
						request_id: Some(RequestId(1)),
					})
					.await
					.unwrap();
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
			.create_broadcast("early-cam", crate::broadcast::Route::new().with_announce(true))
			.unwrap();
		settle().await;

		// Stream 1 is the peer's SUBSCRIBE_NAMESPACE (the peer stays quiet after);
		// streams 2 and 3 answer our two PUBLISH_NAMESPACE requests.
		let ok = publish_namespace_ok(VERSION).await;
		let session = crate::lite::test_transport::ScriptedSession::per_stream(vec![Vec::new(), ok.clone(), ok]);
		let log = session.log.clone();

		let publisher = Publisher::new(
			session.clone(),
			consumer,
			Control::new(None, false),
			None,
			cluster::PeerSetup::default(),
			VERSION,
		);

		let stream = Stream::open(&session, VERSION).await.unwrap();
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
			.create_broadcast("late-cam", crate::broadcast::Route::new().with_announce(true))
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

	/// A publisher talking to a scripted peer that never answers, over one bidi stream.
	struct Harness {
		publisher: Publisher<crate::lite::test_transport::ScriptedSession>,
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
		let peer_setup = cluster::PeerSetup::default();
		peer_setup.set(cluster::Peer::default());

		let publisher = Publisher::new(
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

		let stream = Stream::open(&h.session, version).await.unwrap();
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

		let stream = Stream::open(&h.session, version).await.unwrap();
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

	/// A SUBSCRIBE for a path with no publisher must be answered, and the answer must survive
	/// the trip. `Writer` resets the stream on drop, so returning straight after writing the
	/// error threw the bytes away and left every interop client waiting on a request we had
	/// already refused. Finishing first makes the drop-time reset a no-op.
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

	/// Every FETCH we refuse takes the same path, through its own error encoder. A reset there
	/// loses the rejection exactly like the SUBSCRIBE one did.
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
