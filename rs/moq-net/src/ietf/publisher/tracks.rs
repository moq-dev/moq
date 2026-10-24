//! Answers a moq-transport SUBSCRIBE_TRACKS (draft-18+) through the TRACKS hook.
//!
//! Every track of every broadcast under the request's prefix is listed, later tracks and
//! broadcasts included, but no track's data is pushed unasked. A FORWARD=0 request gets a
//! paused PUBLISH per track, which carries the track's properties and an error PUBLISH_DONE
//! once the track is no longer listed; the subscriber resumes one with REQUEST_UPDATE to get its
//! data, served from the cache as a SUBSCRIBE would be. A FORWARD=1 request asks for every
//! track's data, which no draft lets us downgrade, so from draft-19 each track is answered
//! with PUBLISH_SKIPPED and the subscriber SUBSCRIBEs to what it wants. Draft-18 has no such
//! skip, so there the request is refused.

use crate::driver::Transport;
use std::collections::HashSet;

use hashlink::LinkedHashMap;

use super::*;
use crate::util::TaskSet;

/// The session's SUBSCRIBE_TRACKS bookkeeping, shared by its requests.
#[derive(Default)]
pub(super) struct Listings {
	/// The prefix of every established SUBSCRIBE_TRACKS, by a key its guard removes.
	prefixes: HashMap<u64, crate::PathOwned>,
	next: u64,
	/// On draft-18, the tracks a PUBLISH of ours subscribes the peer to, and how many
	/// SUBSCRIBEs of the peer's each track has: draft-18 allows the peer one subscription
	/// per track (section 5.1), so a PUBLISH joins neither kind.
	published: HashMap<(crate::PathOwned, Arc<str>), usize>,
	subscribed: HashMap<(crate::PathOwned, Arc<str>), usize>,
}

/// Holds a SUBSCRIBE_TRACKS prefix as established until dropped.
struct Established {
	listings: kio::Shared<Listings>,
	key: u64,
}

impl Established {
	/// Establish `prefix`, unless it overlaps an established request (section 10.19): two
	/// would match the same namespaces, and keeping them apart is also what lists each
	/// broadcast at most once per session.
	fn new(listings: &kio::Shared<Listings>, prefix: &crate::PathOwned) -> Option<Self> {
		let mut state = listings.lock();
		if state
			.prefixes
			.values()
			.any(|other| other.has_prefix(prefix) || prefix.has_prefix(other))
		{
			return None;
		}
		let key = state.next;
		state.next += 1;
		state.prefixes.insert(key, prefix.clone());
		Some(Self {
			listings: listings.clone(),
			key,
		})
	}
}

impl Drop for Established {
	fn drop(&mut self) {
		self.listings.lock().prefixes.remove(&self.key);
	}
}

/// Counts one subscription to a track, a PUBLISH of ours or a SUBSCRIBE of the peer's,
/// until dropped.
pub(super) struct Subscribed {
	listings: kio::Shared<Listings>,
	key: (crate::PathOwned, Arc<str>),
	/// A SUBSCRIBE of the peer's, rather than a PUBLISH of ours.
	direct: bool,
}

impl Subscribed {
	/// Claim `namespace`'s `track` for a PUBLISH of ours, unless the peer is subscribed to
	/// it already, by its SUBSCRIBE or another PUBLISH.
	fn publish(listings: &kio::Shared<Listings>, namespace: crate::PathOwned, track: Arc<str>) -> Option<Self> {
		Self::claim(listings, (namespace, track), false)
	}

	/// Count the peer's SUBSCRIBE to `namespace`'s `track`, unless a PUBLISH of ours
	/// subscribes it already.
	pub(super) fn subscribe(listings: &kio::Shared<Listings>, namespace: &crate::Path, track: &str) -> Option<Self> {
		Self::claim(listings, (namespace.to_owned(), Arc::from(track)), true)
	}

	fn claim(listings: &kio::Shared<Listings>, key: (crate::PathOwned, Arc<str>), direct: bool) -> Option<Self> {
		let mut locked = listings.lock();
		let Listings {
			published, subscribed, ..
		} = &mut *locked;
		let (mine, other) = if direct {
			(subscribed, published)
		} else {
			(published, subscribed)
		};
		if other.contains_key(&key) || (!direct && mine.contains_key(&key)) {
			return None;
		}
		*mine.entry(key.clone()).or_default() += 1;
		Some(Self {
			listings: listings.clone(),
			key,
			direct,
		})
	}
}

impl Drop for Subscribed {
	fn drop(&mut self) {
		let mut listings = self.listings.lock();
		let counts = if self.direct {
			&mut listings.subscribed
		} else {
			&mut listings.published
		};
		if let Some(count) = counts.get_mut(&self.key) {
			*count -= 1;
			if *count == 0 {
				counts.remove(&self.key);
			}
		}
	}
}

/// The subscription parameters a PUBLISH subscription has, starting from those of the
/// SUBSCRIBE_TRACKS that made it (draft-19 section 10.19.1) and moved by each update.
#[derive(Clone, Copy)]
struct Params {
	priority: u8,
	order: Option<GroupOrder>,
	filter: Filter,
	max_age: Option<u64>,
	/// NEW_GROUP_REQUEST, applied on the first resume.
	new_group: Option<u64>,
}

/// The listed tracks a request answers with PUBLISH_SKIPPED and has not sent yet, keyed by
/// (suffix, name) in listing order. A track removed before its skip went out cancels it,
/// so a peer that stops reading holds at most one per listed track.
type Skips = kio::Shared<LinkedHashMap<(crate::PathOwned, Arc<str>), ()>>;

/// What a request's listing shares with the tasks listing each broadcast.
#[derive(Clone)]
struct Request {
	origin: origin::Consumer,
	/// The request's prefix, which every PUBLISH namespace starts with.
	prefix: crate::PathOwned,
	/// Whether the request asked for every track's objects, which we answer with skips.
	forward: bool,
	params: Params,
	properties_wanted: bool,
	skips: Skips,
	/// Closes when the request is cancelled: nothing more is PUBLISHed or skipped, but the
	/// PUBLISHes already sent live on.
	live: kio::Consumer<()>,
}

/// One listed track, handed to its PUBLISH task.
struct Listed {
	track: track::Consumer,
	/// The track's full namespace, as the PUBLISH names it.
	namespace: crate::PathOwned,
	name: Arc<str>,
	properties: ietf::Properties,
	/// Closes once the track is no longer listed, holding whether it finished.
	listed: kio::Consumer<bool>,
	/// Closes once the request is cancelled, which a PUBLISH not yet sent heeds.
	live: kio::Consumer<()>,
	/// The subscription slot the PUBLISH holds for as long as it lives.
	slot: crate::session::Slot,
	/// On draft-18, claims the track's one subscription for as long as the PUBLISH lives.
	subscribed: Option<Subscribed>,
	params: Params,
}

/// How a paused PUBLISH subscription resumes forwarding.
struct Resume {
	/// FILL_PARAMETERS (draft-20+) for the objects before the join.
	fill: Option<ietf::Fill>,
	/// NEW_GROUP_REQUEST, counted as a join as on a SUBSCRIBE.
	new_group: Option<u64>,
	/// Whether a REQUEST_UPDATE asked for it, which the join's REQUEST_OK answers.
	answer: bool,
}

/// What ended a paused PUBLISH subscription's wait.
enum Wait {
	/// The subscriber's REQUEST_UPDATE.
	Update(super::super::request_stream::Update),
	/// The track is no longer listed.
	Removed,
	/// Our grant stopped covering the track.
	Revoked,
	/// The subscriber cancelled, or the session ended.
	Cancelled,
	/// The request stream failed.
	Failed(Error),
}

impl Publisher {
	/// Answer a SUBSCRIBE_TRACKS: list every track under its prefix until the subscriber
	/// cancels or the session ends, then let the PUBLISHes it made run to their own end.
	pub(super) async fn serve_subscribe_tracks_stream(
		self,
		mut stream: Stream<Transport, Version>,
		msg: ietf::SubscribeTracks<'_>,
	) -> Result<(), Error> {
		if let Some(error) = super::super::namespace::refusal(&msg.prefix, self.version) {
			return super::super::request_stream::reject(stream, error).await;
		}
		let refuse = |error_code, reason: &'static str| ietf::RequestError {
			request_id: None,
			error_code,
			reason_phrase: reason.into(),
			retry_interval: 0,
		};
		// Filters we do not implement are refused rather than ignored, which would hand the
		// subscriber tracks or objects it excluded.
		if msg.range_filters {
			let error = refuse(request::INVALID_FILTER, "Range Filters exceed MAX_FILTER_RANGES=0");
			return super::super::request_stream::reject(stream, error).await;
		}
		if msg.track_filter {
			let error = refuse(request::NOT_SUPPORTED, "TRACK_PROPERTY_FILTER is not supported");
			return super::super::request_stream::reject(stream, error).await;
		}
		// We never push a track's objects unasked, and draft-18 has no PUBLISH_SKIPPED to
		// list a track without them: its PUBLISH_BLOCKED only reports running out of streams.
		if msg.forward && self.version == Version::Draft18 {
			let error = refuse(request::NOT_SUPPORTED, "SUBSCRIBE_TRACKS needs FORWARD=0");
			return super::super::request_stream::reject(stream, error).await;
		}
		let prefix = msg.prefix.to_owned();
		let Some(established) = Established::new(&self.listings, &prefix) else {
			let error = refuse(request::PREFIX_OVERLAP, "overlaps another SUBSCRIBE_TRACKS");
			return super::super::request_stream::reject(stream, error).await;
		};

		tracing::debug!(prefix = %self.origin.absolute(&prefix), forward = msg.forward, "subscribe_tracks stream");

		// The view SUBSCRIBE_NAMESPACE discovers through: rooted at the prefix so every
		// broadcast arrives named by its suffix, never handing back what flowed through the
		// subscriber, and without hidden broadcasts unless the peer opted in.
		let scope = crate::Pattern::subtree(prefix.as_str())
			.map(|subtree| subtree.rebase(prefix.as_str()))
			.unwrap_or_default();
		let peer = self.peer().await;
		let declared = self.peer_setup.get().await;
		// With MoQ Auth, list nothing until the credential we presented at setup is answered,
		// so no track is listed before our grant says which ones the peer may see. The wait
		// lives in the listing loop so a cancel or a closed session still frees the prefix.
		let mut answered = !declared.auth;
		let origin = self
			.origin
			.scope(&prefix, &scope)
			.unwrap_or_else(|_| self.origin.empty())
			.excluding(self.exclude(&peer))
			.discovery(!declared.hidden || self.origin.includes_hidden());
		let mut announced = origin.announced();

		stream.writer.buffer_varint(ietf::RequestOk::ID)?;
		stream
			.writer
			.encode(&ietf::RequestOk {
				request_id: None,
				active: None,
				largest: None,
			})
			.await?;

		// Dropped when the request is cancelled, which tells every broadcast to stop.
		let live = kio::Producer::new(());
		let request = Request {
			origin,
			prefix: prefix.clone(),
			forward: msg.forward,
			params: Params {
				priority: msg.subscriber_priority.unwrap_or(crate::ietf::priority::ABSENT_WIRE),
				order: msg.group_order,
				filter: msg.filter.unwrap_or(Filter::Unfiltered),
				max_age: None,
				new_group: msg.new_group,
			},
			properties_wanted: msg.properties_wanted,
			skips: Skips::default(),
			live: live.consume(),
		};
		// Each listed broadcast's route: dropping it ends that broadcast's listing.
		let mut routes: HashMap<crate::PathOwned, kio::Producer<()>> = HashMap::new();
		let mut broadcasts = TaskSet::owned();
		let mut session = self.session.clone();
		let mut finished = false;
		let mut goaways = super::super::request_stream::Messages::new(self.session.clone(), self.control.client());

		enum Event {
			Announce(crate::announce::Event),
			/// The subscriber ended the request, or the stream failed.
			Cancelled(Result<(), Error>),
			Closed,
		}

		let cancelled = loop {
			let event = broadcasts
				.drive(|waiter| {
					let mut cx = waiter.context();
					if session.poll_closed(&mut cx).is_ready() {
						return Poll::Ready(Event::Closed);
					}
					if let Poll::Ready(res) =
						self.poll_listing_cancel(&mut stream, &mut finished, &mut goaways, &mut cx)
					{
						return Poll::Ready(Event::Cancelled(res));
					}
					if let Err(err) = Self::poll_write_skips(&mut stream, &request.skips, waiter, &mut cx) {
						return Poll::Ready(Event::Cancelled(Err(err)));
					}
					if !answered {
						ready!(self.auth.poll_setup_answered(waiter));
						answered = true;
					}
					match announced.poll_next(waiter) {
						Poll::Ready(Some(event)) => Poll::Ready(Event::Announce(event)),
						_ => Poll::Pending,
					}
				})
				.await;

			let announce = match event {
				Event::Closed => return Ok(()),
				Event::Cancelled(res) => break res,
				Event::Announce(
					crate::announce::Event::Start(announce) | crate::announce::Event::Restart(announce),
				) => announce,
				Event::Announce(crate::announce::Event::End(announce)) => {
					routes.remove(&announce.prefix);
					continue;
				}
				// A new hop or cost changes nothing listed.
				Event::Announce(crate::announce::Event::Update(_)) => continue,
			};
			// A restart is a new instance: the old one's listing ends with its route.
			let route = kio::Producer::new(());
			let task = self.clone().run_listed_broadcast(
				request.clone(),
				announce.prefix.clone(),
				announce.route.epoch.clone(),
				route.consume(),
			);
			routes.insert(announce.prefix, route);
			broadcasts.push(task);
		};

		tracing::debug!(prefix = %self.origin.absolute(&prefix), ?cancelled, "subscribe_tracks ended");
		// The request no longer stands: another may take its prefix, nothing more is
		// PUBLISHed, and what was never sent is dropped. Its routes stay, so a PUBLISH
		// already sent still ends when its track does.
		drop(established);
		drop(live);
		request.skips.lock().clear();
		// A refused update left its REQUEST_ERROR buffered, so a cancel flushes before the FIN.
		let mut closing = match cancelled {
			Ok(()) => Some(stream.writer),
			Err(err) => {
				stream.writer.abort(&err);
				None
			}
		};
		let mut fin = false;

		// The PUBLISHes already sent are subscriptions of their own, on their own streams,
		// each ending once its broadcast does, even while the peer is slow to take the close.
		loop {
			let event = kio::wait(|waiter| {
				let mut cx = waiter.context();
				if let Some(writer) = closing.as_mut() {
					let closed = 'close: {
						if !fin {
							match writer.poll_flush(&mut cx) {
								Poll::Pending => break 'close Poll::Pending,
								Poll::Ready(res) => {
									if res.and_then(|()| writer.finish()).is_err() {
										break 'close Poll::Ready(());
									}
								}
							}
							fin = true;
						}
						writer.poll_close(&mut cx).map(|_| ())
					};
					if closed.is_ready() {
						closing = None;
					}
				}
				if session.poll_closed(&mut cx).is_ready() {
					return Poll::Ready(None);
				}
				if broadcasts.poll(waiter).is_ready() && closing.is_none() {
					return Poll::Ready(None);
				}
				announced.poll_next(waiter)
			})
			.await;
			match event {
				Some(crate::announce::Event::End(announce) | crate::announce::Event::Restart(announce)) => {
					routes.remove(&announce.prefix);
				}
				Some(_) => {}
				None => return Ok(()),
			}
		}
	}

	/// Poll for the subscriber ending a SUBSCRIBE_TRACKS: `Ok` for a cancel, or the error
	/// ending it, such as a REQUEST_UPDATE we refuse.
	fn poll_listing_cancel(
		&self,
		stream: &mut Stream<Transport, Version>,
		finished: &mut bool,
		goaways: &mut super::super::request_stream::Messages,
		cx: &mut std::task::Context<'_>,
	) -> Poll<Result<(), Error>> {
		use super::super::request_stream::{NamespaceUpdate, Next};
		if stream.writer.poll_closed(cx).is_ready() {
			return Poll::Ready(Ok(()));
		}
		while !*finished {
			match ready!(stream.reader.poll_decode_maybe::<Next<NamespaceUpdate<true>>>(cx)) {
				Ok(Some(Next::Message(update))) => {
					// Its Request ID is spent like any other: a bad one is the session's error,
					// and one past our GOAWAY is refused as such.
					let going_away = match self.control.accept(update.request_id) {
						Err(Error::GoingAway) => true,
						Err(err) => {
							self.session.clone().close(
								super::super::error::to_session_code(&crate::SessionError::from(&err)),
								"invalid request ID",
							);
							return Poll::Ready(Err(err));
						}
						Ok(_) => false,
					};
					// A new prefix or FORWARD would change which later tracks are listed and
					// how, which we do not support. Refusing it ends the request (section 10.9.1).
					let (err, reason) = match going_away {
						true => (Error::GoingAway, "going away"),
						false => (
							update.refused.unwrap_or(Error::Unsupported),
							"REQUEST_UPDATE on SUBSCRIBE_TRACKS is not supported",
						),
					};
					let code = request::to_code(&err, request::Kind::SubscribeNamespace, self.version);
					stream.writer.buffer_varint(ietf::RequestError::ID)?;
					stream.writer.buffer(&ietf::RequestError {
						request_id: None,
						error_code: code,
						reason_phrase: reason.into(),
						retry_interval: 0,
					})?;
					return Poll::Ready(Ok(()));
				}
				Ok(Some(Next::GoAway { redirect })) => goaways.receive(redirect)?,
				Ok(None) if !super::super::request_stream::fin_cancels(self.version) => *finished = true,
				Ok(None) => return Poll::Ready(Ok(())),
				Err(err) => return Poll::Ready(Err(goaways.failed(err))),
			}
		}
		Poll::Pending
	}

	/// Write the pending PUBLISH_SKIPPEDs, buffering the next only once the last is out, so
	/// a peer that stops reading leaves the rest collapsing in `skips`.
	fn poll_write_skips(
		stream: &mut Stream<Transport, Version>,
		skips: &Skips,
		waiter: &kio::Waiter,
		cx: &mut std::task::Context<'_>,
	) -> Result<(), Error> {
		while stream.writer.poll_flush(cx)?.is_ready() {
			let Poll::Ready(mut pending) = skips.poll(waiter, |skips| match skips.is_empty() {
				true => Poll::Pending,
				false => Poll::Ready(()),
			}) else {
				break;
			};
			let Some(((suffix, name), ())) = pending.pop_front() else {
				break;
			};
			drop(pending);
			stream.writer.buffer_varint(ietf::PublishSkipped::ID)?;
			stream.writer.buffer(&ietf::PublishSkipped {
				suffix,
				track_name: name.as_ref().into(),
			})?;
		}
		Ok(())
	}

	/// List one broadcast under a SUBSCRIBE_TRACKS: PUBLISH or skip each of its tracks until
	/// its route goes or the broadcast ends, then let its PUBLISHes run to their end.
	async fn run_listed_broadcast(
		self,
		request: Request,
		suffix: crate::PathOwned,
		epoch: Option<crate::Epoch>,
		route: kio::Consumer<()>,
	) {
		let namespace = request.prefix.join(&suffix).to_owned();
		let absolute = self.origin.absolute(&namespace).to_owned();
		let flate = self.peer_setup.get().await.flate;

		// Resolving may wait on a route for the broadcast, which its going ends.
		let resolved = {
			let mut requesting = std::pin::pin!(request.origin.request_broadcast(&suffix, epoch));
			kio::wait(|waiter| {
				if route.poll_closed(waiter).is_ready() || request.live.poll_closed(waiter).is_ready() {
					return Poll::Ready(None);
				}
				waiter.poll_future(requesting.as_mut()).map(Some)
			})
			.await
		};
		let broadcast = match resolved {
			Some(Ok(broadcast)) => broadcast,
			Some(Err(err)) => {
				tracing::debug!(broadcast = %absolute, %err, "listed broadcast did not resolve");
				return;
			}
			None => return,
		};
		// Each PUBLISH watches its track's entry here: dropping it says the track is no
		// longer listed.
		let mut listed: HashMap<Arc<str>, kio::Producer<bool>> = HashMap::new();
		// The tracks this broadcast queued a skip for, so its end touches only its own.
		let mut skipped: HashSet<Arc<str>> = HashSet::new();
		let mut publishes = TaskSet::owned();
		// The listing ended, or the request did: nothing more to add.
		let mut adding = true;
		// The broadcast's listing, held only while our publish permit covers it: a track
		// it does not cover is not revealed at all, and one it comes to cover is listed
		// from scratch.
		let (permit, mut auth_epoch) = self.permit_now(None);
		let mut tracks = None;
		if permit.matches(namespace.as_str()) {
			match broadcast.tracks() {
				Ok(listing) => tracks = Some(listing),
				Err(err) => {
					tracing::debug!(broadcast = %absolute, %err, "listed broadcast ended");
					return;
				}
			}
		}

		enum Event {
			Track(crate::broadcast::TracksEvent),
			/// Our publish permit may have changed.
			Regrant(crate::auth::Permit),
			/// The broadcast ended or its route went: every track it listed is gone.
			Ended,
			/// The request was cancelled.
			Cancelled,
		}

		loop {
			let event = kio::wait(|waiter| {
				let drained = publishes.poll(waiter).is_ready();
				if !adding && drained {
					return Poll::Ready(None);
				}
				if adding && request.live.poll_closed(waiter).is_ready() {
					return Poll::Ready(Some(Event::Cancelled));
				}
				if route.poll_closed(waiter).is_ready() {
					return Poll::Ready(Some(Event::Ended));
				}
				if let Poll::Ready(permit) =
					self.auth
						.poll_permit(crate::auth::Direction::Publish, &mut auth_epoch, waiter)
				{
					return Poll::Ready(Some(Event::Regrant(permit)));
				}
				let Some(tracks) = tracks.as_mut() else {
					return Poll::Pending;
				};
				match tracks.poll_next(waiter) {
					Poll::Ready(Ok(Some(event))) => Poll::Ready(Some(Event::Track(event))),
					Poll::Ready(Ok(None)) => Poll::Ready(Some(Event::Ended)),
					Poll::Ready(Err(err)) => {
						tracing::debug!(broadcast = %absolute, %err, "listing failed");
						Poll::Ready(Some(Event::Ended))
					}
					Poll::Pending => Poll::Pending,
				}
			})
			.await;

			let Some(event) = event else {
				return;
			};
			let unlist = |listed: &mut HashMap<Arc<str>, kio::Producer<bool>>, skipped: &mut HashSet<Arc<str>>| {
				listed.clear();
				let mut skips = request.skips.lock();
				for name in skipped.drain() {
					skips.remove(&(suffix.clone(), name));
				}
			};
			match event {
				Event::Ended => {
					unlist(&mut listed, &mut skipped);
					// What it already PUBLISHed ends now; wait for that to go out.
					kio::wait(|waiter| publishes.poll(waiter)).await;
					return;
				}
				Event::Regrant(permit) => match (permit.matches(namespace.as_str()), tracks.is_some()) {
					// Each PUBLISH ends on its own gate as well; dropping the listing also
					// cancels the skips not yet sent.
					(false, true) => {
						tracks = None;
						unlist(&mut listed, &mut skipped);
					}
					(true, false) if adding => match broadcast.tracks() {
						Ok(listing) => tracks = Some(listing),
						Err(err) => {
							tracing::debug!(broadcast = %absolute, %err, "listed broadcast ended");
							unlist(&mut listed, &mut skipped);
							kio::wait(|waiter| publishes.poll(waiter)).await;
							return;
						}
					},
					_ => {}
				},
				// The PUBLISHes already sent still end when their track is unlisted, so keep
				// reading the listing for them, adding nothing.
				Event::Cancelled => adding = false,
				Event::Track(crate::broadcast::TracksEvent::Synced) => {}
				Event::Track(
					crate::broadcast::TracksEvent::Removed { name } | crate::broadcast::TracksEvent::Finished { name },
				) if skipped.remove(&name) => {
					request.skips.lock().remove(&(suffix.clone(), name));
				}
				Event::Track(crate::broadcast::TracksEvent::Removed { name }) => {
					listed.remove(&name);
				}
				// Its PUBLISH ends with TRACK_ENDED rather than in error.
				Event::Track(crate::broadcast::TracksEvent::Finished { name }) => {
					if let Some(entry) = listed.remove(&name)
						&& let Ok(mut finished) = entry.write()
					{
						*finished = true;
					}
				}
				Event::Track(crate::broadcast::TracksEvent::Added { .. }) if !adding => {}
				Event::Track(crate::broadcast::TracksEvent::Added { name, info }) => {
					let mut skip = |name: Arc<str>| {
						request.skips.lock().insert((suffix.clone(), name.clone()), ());
						skipped.insert(name);
					};
					if request.forward {
						skip(name);
						continue;
					}
					// A track this draft cannot describe is skipped rather than PUBLISHed with
					// its properties rewritten.
					let Ok(properties) = track_properties(&info, request.properties_wanted, self.version, flate) else {
						skip(name);
						continue;
					};
					let Ok(track) = broadcast.track(&name) else {
						continue;
					};
					// Each paused PUBLISH holds a subscription slot; past the cap the peer is
					// told the track exists rather than left without it.
					let Ok(slot) = self.subscriptions.acquire() else {
						skip(name);
						continue;
					};
					// Draft-18 allows one subscription per track (section 5.1), so a track the
					// peer is subscribed to, by its SUBSCRIBE or a PUBLISH still alive, is
					// skipped. Held from now, so a SUBSCRIBE cannot slip in before the PUBLISH
					// goes out.
					let subscribed = match self.version {
						Version::Draft18 => {
							match Subscribed::publish(&self.listings, namespace.clone(), name.clone()) {
								Some(subscribed) => Some(subscribed),
								None => {
									skip(name);
									continue;
								}
							}
						}
						_ => None,
					};
					let entry = kio::Producer::new(false);
					let task = Listed {
						track,
						namespace: namespace.clone(),
						name: name.clone(),
						properties,
						listed: entry.consume(),
						live: request.live.clone(),
						slot,
						subscribed,
						params: request.params,
					};
					listed.insert(name.clone(), entry);
					let this = self.clone();
					publishes.push(async move {
						if let Err(err) = this.run_publish(task).await {
							tracing::debug!(%err, "publish stream error");
						}
					});
				}
			}
		}
	}

	/// Run one paused PUBLISH subscription until it ends: the track is unlisted, the
	/// subscriber cancels or refuses it, or, once resumed, the track ends.
	async fn run_publish(self, task: Listed) -> Result<(), Error> {
		let Listed {
			track,
			namespace,
			name,
			properties,
			listed,
			live,
			slot,
			subscribed,
			mut params,
		} = task;
		let _slot = slot;
		let _subscribed = subscribed;
		// An update may ask for a new group only if the PUBLISH sent DYNAMIC_GROUPS, which
		// INCLUDE_PROPERTIES=0 leaves out.
		let dynamic = properties.dynamic_groups;
		let absolute = self.origin.absolute(&namespace).to_owned();

		// Every wait on the peer's credit, for the stream, a Request ID, or the PUBLISH's
		// bytes, lasts only while the track is listed and the request stands; a peer that
		// grants none holds only this task and its slot. A Request ID abandoned unsent leaves
		// a gap, which the drafts with SUBSCRIBE_TRACKS allow: none has MAX_REQUEST_ID.
		let mut session = self.session.clone();
		let opened = {
			let mut opening = std::pin::pin!(Stream::open(&mut session, self.version));
			kio::wait(|waiter| {
				if listed.poll_closed(waiter).is_ready() || live.poll_closed(waiter).is_ready() {
					return Poll::Ready(None);
				}
				waiter.poll_future(opening.as_mut()).map(Some)
			})
			.await
		};
		let Some(opened) = opened else {
			return Ok(());
		};
		let mut stream = super::super::request_stream::with_stats(opened?, self.stats(), self.version);
		let allocated = kio::wait(|waiter| {
			if listed.poll_closed(waiter).is_ready() || live.poll_closed(waiter).is_ready() {
				return Poll::Ready(None);
			}
			self.control.poll_next_request_id(waiter).map(Some)
		})
		.await;
		// Dropped unwritten, the stream is reset before its header, which the peer ignores.
		let Some(request_id) = allocated else {
			return Ok(());
		};

		// A joining FETCH needs the subscription forwarding.
		let join = (!Filter::is_draft20(self.version)).then(|| self.register_join(request_id));
		self.pause_join(join.as_ref());

		tracing::debug!(id = %request_id, broadcast = %absolute, track = %name, "publish started");
		stream.writer.buffer_varint(ietf::Publish::ID)?;
		let publish = ietf::Publish {
			request_id,
			track_namespace: namespace.clone(),
			track_name: name.as_ref().into(),
			track_alias: request_id.0,
			largest_location: live_edge(&track).largest,
			forward: false,
			properties,
			// Draft-20 and later state the listing's subscription parameters on each PUBLISH;
			// earlier drafts carry them on PUBLISH_OK.
			subscriber_priority: Some(params.priority),
			group_order: params.order,
			filter: Some(params.filter),
		};
		let written = {
			let mut writing = std::pin::pin!(stream.writer.encode(&publish));
			kio::wait(|waiter| {
				if listed.poll_closed(waiter).is_ready() || live.poll_closed(waiter).is_ready() {
					return Poll::Ready(None);
				}
				waiter.poll_future(writing.as_mut()).map(Some)
			})
			.await
		};
		// Cut short, the stream is reset before its header is whole, which the peer ignores.
		let Some(written) = written else {
			return Ok(());
		};
		written?;

		let mut gate = crate::auth::Gate::new(self.auth.clone(), namespace.clone(), crate::auth::Direction::Publish);
		let mut finished = false;
		let mut goaways = super::super::request_stream::Messages::new(self.session.clone(), self.control.client());

		// PUBLISH_OK or REQUEST_ERROR. A track that left the listing unfinished before the
		// answer has nothing to end yet, so the request is reset. One that finished waits for
		// the answer, then ends with TRACK_ENDED, since the peer listed it on the PUBLISH.
		let answer = kio::wait(|waiter| {
			let mut cx = waiter.context();
			if session.poll_closed(&mut cx).is_ready() || (listed.poll_closed(waiter).is_ready() && !*listed.read()) {
				return Poll::Ready(None);
			}
			goaways.poll_next(&mut stream.reader, &mut cx).map(Some)
		})
		.await;
		// A NEW_GROUP_REQUEST on a PUBLISH_OK that leaves the subscription paused waits for
		// the resume.
		let mut new_group: Option<u64>;
		// A PUBLISH_OK breaking a rule ends the PUBLISH, as a refused update does.
		let mut refused = None;
		let resume = match answer {
			None => {
				stream.writer.abort(&Error::Cancel);
				return Ok(());
			}
			Some(Err(err)) => return Err(err),
			Some(Ok(None)) => return Err(goaways.failed(Error::ProtocolViolation)),
			Some(Ok(Some(super::super::request_stream::Framed(id, body)))) => match id {
				ietf::RequestError::ID => {
					let refused = body
						.message::<ietf::RequestError>(self.version)
						.map_err(|err| goaways.failed(err.into()))?;
					tracing::debug!(id = %request_id, track = %name, code = refused.error_code, reason = %refused.reason_phrase, "publish refused");
					return Ok(());
				}
				// PUBLISH_OK is a REQUEST_OK from draft-18, which still lists 0x1E for it.
				_ if id == ietf::RequestOk::ID || (id == ietf::PublishOk::ID && self.version == Version::Draft18) => {
					let ok = body
						.message::<ietf::PublishOk>(self.version)
						.map_err(|err| goaways.failed(err.into()))?;
					// Through draft-19 the answer sets the Forward State and the parameters it
					// carries, an absent FORWARD meaning 1; an absent parameter keeps the one the
					// SUBSCRIBE_TRACKS set. Draft-20 moved them off PUBLISH_OK, leaving it paused.
					if let Some(priority) = ok.subscriber_priority {
						params.priority = priority;
					}
					params.order = ok.group_order.or(params.order);
					params.filter = ok.filter.unwrap_or(params.filter);
					// The answer may ask for a new group only if the PUBLISH sent DYNAMIC_GROUPS
					// (draft-18 section 10.2.13). The listing's own asked without knowing, as a
					// SUBSCRIBE may, so it is ignored where it does not apply.
					if ok.new_group.is_some() && !dynamic {
						refused = Some(Served::UpdateFailed(Error::Unsupported));
					}
					new_group = ok.new_group.or(params.new_group.filter(|_| dynamic));
					(ok.forward && !Filter::is_draft20(self.version)).then(|| Resume {
						fill: None,
						new_group: new_group.take(),
						answer: false,
					})
				}
				_ => return Err(goaways.failed(Error::UnexpectedMessage)),
			},
		};

		if let Some(done) = refused {
			drop(join);
			return self.write_publish_done(stream, request_id, done, 0).await;
		}

		// Group streams opened over every time the subscription forwarded, which
		// PUBLISH_DONE counts.
		let mut opened = 0;
		// What to resume with, and whether an update asked for it: a forwarding PUBLISH_OK
		// resumes at once, with no update to answer.
		let mut resume: Option<Resume> = resume;
		let done = loop {
			if let Some(how) = resume.take() {
				match self
					.resume_publish(
						&mut stream,
						&track,
						&namespace,
						request_id,
						&mut params,
						how,
						dynamic,
						join.as_ref(),
						&listed,
						&mut gate,
						&mut finished,
						&mut goaways,
						&mut opened,
					)
					.await
				{
					Ok(Served::Paused(_)) => {
						self.pause_join(join.as_ref());
						self.write_request_ok(&mut stream.writer, request_id, None).await?;
						// Unlisted while it forwarded: it ends now that it is idle.
						if listed.is_closed() {
							break unlisted(&listed);
						}
						continue;
					}
					Ok(served) => break served,
					// Cancelled by the subscriber, or the session ended.
					Err(()) => return Ok(()),
				}
			}

			let wait = kio::wait(|waiter| {
				let mut cx = waiter.context();
				if session.poll_closed(&mut cx).is_ready() || stream.writer.poll_closed(&mut cx).is_ready() {
					return Poll::Ready(Wait::Cancelled);
				}
				if gate.poll_denied(waiter).is_ready() {
					return Poll::Ready(Wait::Revoked);
				}
				if listed.poll_closed(waiter).is_ready() {
					return Poll::Ready(Wait::Removed);
				}
				match ready!(self.poll_update(&mut stream, &mut finished, &mut goaways, &mut cx)) {
					UpdateRead::Update(update) => Poll::Ready(Wait::Update(update)),
					UpdateRead::Cancelled => Poll::Ready(Wait::Cancelled),
					UpdateRead::Malformed(err) => {
						if super::super::subscriber::is_protocol_violation(&err) {
							self.session.clone().close(
								super::super::error::to_session_code(&crate::SessionError::from(&err)),
								&err.to_string(),
							);
						}
						Poll::Ready(Wait::Failed(err))
					}
					UpdateRead::Failed(err) => Poll::Ready(Wait::Failed(err)),
				}
			})
			.await;

			let update = match wait {
				Wait::Update(update) => update,
				Wait::Removed => break unlisted(&listed),
				Wait::Revoked => {
					tracing::info!(broadcast = %absolute, track = %name, "publish no longer authorized");
					break Served::Track(Err(Error::Unauthorized));
				}
				Wait::Cancelled => return Ok(()),
				Wait::Failed(err) => break Served::Track(Err(err)),
			};
			// Refused like an update to any subscription: it fails, ending this one.
			let refusal = match update.refused {
				Some(Error::Decode(err)) => Some((super::super::token::reason(&err), Error::Decode(err))),
				Some(Error::GoingAway) => Some(("going away", Error::GoingAway)),
				Some(err) => Some(("REQUEST_UPDATE parameters not supported", err)),
				None if update.authorization_token.is_some()
					&& self.auth.token_decides(crate::auth::Direction::Publish) =>
				{
					Some((
						"AUTHORIZATION TOKEN on a request admitted without one",
						Error::Unsupported,
					))
				}
				None if update.new_group.is_some() && !dynamic => Some((
					"NEW_GROUP_REQUEST on a track without DYNAMIC_GROUPS",
					Error::Unsupported,
				)),
				None if update.fill.is_some() && update.forward != Some(true) => {
					Some(("FILL_PARAMETERS without resuming", Error::Unsupported))
				}
				// A paused subscription opens no groups, so a new one is asked for only as it
				// resumes.
				None if update.new_group.is_some() && update.forward != Some(true) => {
					Some(("NEW_GROUP_REQUEST without resuming", Error::Unsupported))
				}
				None => None,
			};
			if let Some((reason, err)) = refusal {
				self.write_subscribe_error(&mut stream.writer, update.request_id, &err, reason)
					.await?;
				break Served::UpdateFailed(Error::Unsupported);
			}
			if let Some(priority) = update.priority {
				params.priority = priority;
			}
			if let Some(filter) = update.filter {
				params.filter = filter;
			}
			if let Some(max_age) = update.max_age {
				params.max_age = Some(max_age);
			}
			match update.forward {
				Some(true) => {
					resume = Some(Resume {
						fill: update.fill,
						new_group: update.new_group.or(new_group.take()),
						answer: true,
					})
				}
				_ => self.write_request_ok(&mut stream.writer, request_id, None).await?,
			}
		};

		drop(join);
		self.write_publish_done(stream, request_id, done, opened).await
	}

	/// Mark a paused subscription for joining FETCHes: draft-18 section 5.1 refuses one
	/// unless the subscription forwards.
	pub(super) fn pause_join(&self, join: Option<&Join<GroupOf<Transport>>>) {
		if let Some(join) = join {
			join.joins.lock().insert(join.request_id, Some(Joined::Empty));
		}
	}

	/// End a PUBLISH whose resume failed with `err` before it joined. A REQUEST_UPDATE that
	/// asked for it is owed an answer, so it is refused, which fails the update (draft-22
	/// section 9.5). `Err(())` when the subscriber cancelled.
	async fn resume_failed(
		&self,
		stream: &mut Stream<Transport, Version>,
		request_id: RequestId,
		answer: bool,
		err: Error,
	) -> Result<Served, ()> {
		if !answer {
			return Ok(Served::Track(Err(err)));
		}
		self.write_subscribe_error(&mut stream.writer, request_id, &err, "resume failed")
			.await
			.map_err(|_| ())?;
		Ok(Served::UpdateFailed(err))
	}

	/// Forward a paused PUBLISH subscription from the cache with `params`, as a SUBSCRIBE
	/// with them would be served, until it pauses again or ends. Answers the resuming update
	/// with where the subscription now joins, when `answer` says one asked. `Err(())` when the
	/// subscriber cancelled.
	#[expect(
		clippy::too_many_arguments,
		reason = "private plumbing; a single-use argument struct would only rename the parameters"
	)]
	async fn resume_publish(
		&self,
		stream: &mut Stream<Transport, Version>,
		track: &track::Consumer,
		namespace: &crate::PathOwned,
		request_id: RequestId,
		params: &mut Params,
		how: Resume,
		dynamic: bool,
		join: Option<&Join<GroupOf<Transport>>>,
		listed: &kio::Consumer<bool>,
		gate: &mut crate::auth::Gate,
		finished: &mut bool,
		goaways: &mut super::super::request_stream::Messages,
		opened: &mut u64,
	) -> Result<Served, ()> {
		let mut subscription = serving_subscription(params.priority);
		if let Some(order) = params.order {
			subscription = subscription.with_order(order.to_order());
		}
		if let Some(max_age) = params.max_age {
			subscription.max_delay = Duration::from_millis(max_age);
		}
		// A start the live edge does not decide is no join; see `serve_subscribe_stream`.
		let fixed = subscribe_range(params.filter, LiveEdge::default(), self.version);
		if let Some(start) = fixed.start {
			ServeRange {
				start: Some(Location { object: 0, ..start }),
				..fixed
			}
			.apply(&mut subscription);
		}
		// Setting up lasts only while the track is listed and the subscriber stays: a cold
		// route may take a while to say where its live edge is.
		let mut subscribing = std::pin::pin!(track.subscribe(subscription.clone()));
		let subscribed = kio::wait(|waiter| {
			if stream.writer.poll_closed(&mut waiter.context()).is_ready() {
				return Poll::Ready(Err(()));
			}
			if listed.poll_closed(waiter).is_ready() {
				return Poll::Ready(Ok(None));
			}
			waiter
				.poll_future(subscribing.as_mut())
				.map(|subscribed| Ok(Some(subscribed)))
		})
		.await?;
		let mut subscriber = match subscribed {
			Some(Ok(subscriber)) => subscriber,
			Some(Err(err)) => return self.resume_failed(stream, request_id, how.answer, err).await,
			None => {
				return self
					.resume_failed(stream, request_id, how.answer, Error::NotFound)
					.await;
			}
		};
		let live = kio::wait(|waiter| {
			if stream.writer.poll_closed(&mut waiter.context()).is_ready() {
				return Poll::Ready(Err(()));
			}
			if listed.poll_closed(waiter).is_ready() {
				return Poll::Ready(Ok(false));
			}
			subscriber.poll_live(waiter).map(|_| Ok(true))
		})
		.await?;
		if !live {
			return self
				.resume_failed(stream, request_id, how.answer, Error::NotFound)
				.await;
		}

		let edge = live_edge(track);
		let range = subscribe_range(params.filter, edge, self.version);
		range.apply(&mut subscription);
		let _ = subscriber.update(subscription);
		let timescale = subscriber
			.info()
			.timescale
			.filter(|_| ietf::Properties::sends_timescale(self.version));
		let mut serve = TrackServe::new(
			self.session.clone(),
			subscriber,
			request_id,
			self.version,
			range,
			timescale,
		)
		.compression(self.peer_setup.get().await.flate)
		.ranked(self.priority.clone());
		serve.dynamic = dynamic;
		if let Some(value) = how.new_group {
			serve.request_new_group(value);
		}

		if let Some(join) = join {
			let joined = match (params.filter, edge.largest) {
				(Filter::NextObject, Some(largest)) => Joined::Group {
					end: Location {
						group: largest.group,
						object: largest.object + 1,
					},
					cache: track.clone(),
					namespace: namespace.clone(),
					token: false,
					ranking: serve.ranking.clone(),
				},
				(Filter::NextObject, None) => Joined::Empty,
				_ => Joined::Unsupported,
			};
			join.joins.lock().insert(join.request_id, Some(joined));
		}
		let fill = how
			.fill
			.filter(|_| Filter::is_draft20(self.version))
			.map(|fill| fill_range(fill, params.filter, edge.largest));

		if how.answer
			&& self
				.write_request_ok(&mut stream.writer, request_id, edge.largest)
				.await
				.is_err()
		{
			return Err(());
		}

		let served = {
			let ranking = serve.ranking.clone();
			let streams = serve.opened.clone();
			let fill = async {
				if let Some(fill) = fill {
					self.run_fill(fill, &ranking, track.clone(), timescale, &streams).await;
				}
			};
			let serving = self.run_subscription(stream, &mut serve, finished, goaways, fill, None);
			// A track no longer listed ends its PUBLISH even while the track itself goes on,
			// as when its broadcast's route is withdrawn.
			crate::auth::until(serving, |waiter| {
				if gate.poll_denied(waiter).is_ready() {
					return Poll::Ready(Error::Unauthorized.into());
				}
				match listed.poll_closed(waiter) {
					Poll::Ready(()) => Poll::Ready(Error::Cancel.into()),
					Poll::Pending => Poll::Pending,
				}
			})
			.await
		};
		// What an update changed while forwarding outlives a pause.
		let applied = serve.track.subscription();
		params.priority = crate::ietf::priority::to_wire(applied.priority);
		params.max_age = applied.max_delay.as_millis().try_into().ok();
		let served = match served {
			Ok(Some(served)) => served,
			Ok(None) => {
				serve.close_groups();
				*opened += serve.opened();
				return Err(());
			}
			// Unlisted while it forwarded.
			Err(refusal) if matches!(refusal.error, Error::Cancel) => unlisted(listed),
			Err(refusal) => Served::Track(Err(refusal.error)),
		};
		if matches!(served, Served::Track(Ok(()))) {
			self.write_end_of_track(stream, &mut serve, finished, goaways).await;
		}
		// Forwarding stops here: PUBLISH_DONE, or the answer to the pause, follows the close
		// of every data stream.
		serve.close_groups();
		*opened += serve.opened();
		Ok(served)
	}
}

/// How a PUBLISH whose track is no longer listed ends: TRACK_ENDED if it finished, otherwise
/// in error. A track that leaves the listing unfinished may go on elsewhere, as when its
/// broadcast's route is withdrawn, and a clean end would tell the subscriber it is over.
fn unlisted(listed: &kio::Consumer<bool>) -> Served {
	match *listed.read() {
		true => Served::Track(Ok(())),
		false => Served::Track(Err(Error::NotFound)),
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::model::ProduceTest as _;
	use crate::transport::memory;

	const TIMEOUT: Duration = Duration::from_secs(10);

	const DRAFTS: [Version; 5] = [
		Version::Draft18,
		Version::Draft19,
		Version::Draft20,
		Version::Draft21,
		Version::Draft22,
	];

	/// A publisher whose origin holds broadcast `room/cam` listing `video` and `audio`, and
	/// the peer's end of its session.
	struct Harness {
		version: Version,
		publisher: Publisher,
		session: memory::Session,
		peer: memory::Session,
		broadcast: crate::broadcast::Producer,
		tracks: HashMap<&'static str, track::Producer>,
		_origin: origin::Producer,
	}

	fn harness(version: Version) -> Harness {
		harness_with(version, memory::Config::default(), peer::Peer::default())
	}

	fn harness_with(version: Version, config: memory::Config, declared: peer::Peer) -> Harness {
		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let broadcast = origin.publish("room/cam", crate::origin::Route::default()).unwrap();
		let tracks = ["video", "audio"]
			.into_iter()
			.map(|name| (name, broadcast.create_track(name, crate::track::Info::timed()).unwrap()))
			.collect();
		let (session, peer) = memory::pair(config);
		let setup = peer::PeerSetup::default();
		setup.set(declared);
		let publisher = Publisher::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			origin.consume(),
			Control::new(None, false),
			None,
			setup,
			version,
		);
		Harness {
			version,
			publisher,
			session,
			peer,
			broadcast,
			tracks,
			_origin: origin,
		}
	}

	fn listing(forward: bool) -> ietf::SubscribeTracks<'static> {
		ietf::SubscribeTracks {
			request_id: RequestId(0),
			prefix: crate::Path::new("room"),
			forward,
			subscriber_priority: None,
			group_order: None,
			filter: None,
			fill: None,
			properties_wanted: true,
			new_group: None,
			range_filters: false,
			track_filter: false,
			authorization_token: None,
		}
	}

	impl Harness {
		/// Send `msg` on a new request stream, dispatched as the session would.
		async fn request<M: Message>(&mut self, msg: &M) -> Stream<Transport, Version> {
			let mut stream = Stream::open(&mut crate::driver::Transport::new(self.peer.clone()), self.version)
				.await
				.unwrap();
			stream.writer.varint(M::ID).await.unwrap();
			stream.writer.encode(msg).await.unwrap();

			let mut accepted = timeout(Stream::accept(
				&mut crate::driver::Transport::new(self.session.clone()),
				self.version,
			))
			.await
			.unwrap();
			let id = accepted.reader.varint().await.unwrap();
			let body: ietf::Body = accepted.reader.decode().await.unwrap();
			let task = self.publisher.handle_stream(id, body, accepted, false).unwrap();
			moq_clock::sim::spawn(task);
			stream
		}

		/// The next PUBLISH the publisher opens: its stream, namespace, track and FORWARD.
		async fn publish(&mut self) -> (Stream<Transport, Version>, String, String, bool) {
			let mut stream = timeout(Stream::accept(
				&mut crate::driver::Transport::new(self.peer.clone()),
				self.version,
			))
			.await
			.unwrap();
			assert_eq!(stream.reader.varint().await.unwrap(), ietf::Publish::ID);
			let body: ietf::Body = stream.reader.decode().await.unwrap();
			let publish: ietf::Publish = body.message(self.version).unwrap();
			let namespace = publish.track_namespace.as_str().to_string();
			let name = publish.track_name.to_string();
			(stream, namespace, name, publish.forward)
		}
	}

	async fn timeout<F: std::future::Future>(future: F) -> F::Output {
		moq_clock::timeout(TIMEOUT, future).await.expect("timed out")
	}

	/// The next message on a request stream: its type and body.
	async fn next(stream: &mut Stream<Transport, Version>) -> (u64, ietf::Body) {
		let id = timeout(stream.reader.varint()).await.unwrap();
		let body = stream.reader.decode().await.unwrap();
		(id, body)
	}

	async fn send<M: Message>(stream: &mut Stream<Transport, Version>, msg: &M) {
		stream.writer.varint(M::ID).await.unwrap();
		stream.writer.encode(msg).await.unwrap();
	}

	fn request_error(body: &ietf::Body, version: Version) -> u64 {
		body.message::<ietf::RequestError>(version).unwrap().error_code
	}

	/// Answer a PUBLISH with a PUBLISH_OK leaving it paused, which draft-20 on cannot
	/// change. From draft-18 it is sent as REQUEST_OK.
	async fn publish_ok(stream: &mut Stream<Transport, Version>) {
		stream.writer.varint(ietf::RequestOk::ID).await.unwrap();
		stream
			.writer
			.encode(&ietf::PublishOk {
				request_id: None,
				forward: false,
				subscriber_priority: Some(128),
				group_order: Some(GroupOrder::Ascending),
				filter: Some(Filter::NextObject),
				new_group: None,
			})
			.await
			.unwrap();
	}

	fn update(request_id: u64, forward: bool) -> ietf::SubscribeUpdate {
		ietf::SubscribeUpdate {
			request_id: RequestId(request_id),
			subscription_request_id: None,
			start_location: Location { group: 0, object: 0 },
			end_group: 0,
			subscriber_priority: None,
			forward: Some(forward),
			// Join at the live edge: draft-20 moved the filter off PUBLISH_OK.
			filter: forward.then_some(Filter::NextObject),
			authorization_token: None,
			new_group: None,
			max_age: None,
			fill: None,
		}
	}

	/// A paused listing PUBLISHes every track under the prefix, later ones included, each
	/// paused, and ends a track's PUBLISH with PUBLISH_DONE once it is no longer listed.
	#[moq_clock::test]
	async fn a_paused_listing_publishes_every_track() {
		for version in DRAFTS {
			let mut h = harness(version);
			let mut request = h.request(&listing(false)).await;
			assert_eq!(next(&mut request).await.0, ietf::RequestOk::ID, "{version}");

			let mut published = HashMap::new();
			for _ in 0..2 {
				let (stream, namespace, name, forward) = h.publish().await;
				assert_eq!(namespace, "room/cam", "{version}");
				assert!(!forward, "{version}: {name} forwards");
				published.insert(name, stream);
			}
			assert!(
				published.contains_key("video") && published.contains_key("audio"),
				"{version}"
			);

			let _late = h
				.broadcast
				.create_track("captions", crate::track::Info::timed())
				.unwrap();
			let (_stream, _, name, _) = h.publish().await;
			assert_eq!(name, "captions", "{version}");

			let mic = h._origin.publish("room/mic", crate::origin::Route::default()).unwrap();
			let _audio = mic.create_track("audio", crate::track::Info::timed()).unwrap();
			let (_stream, namespace, name, _) = h.publish().await;
			assert_eq!((namespace.as_str(), name.as_str()), ("room/mic", "audio"), "{version}");

			let mut audio = published.remove("audio").unwrap();
			publish_ok(&mut audio).await;
			drop(h.tracks.remove("audio"));
			let (id, body) = next(&mut audio).await;
			assert_eq!(id, ietf::PublishDone::ID, "{version}");
			let done: ietf::PublishDone = body.message(version).unwrap();
			assert_eq!(done.stream_count, 0, "{version}");
		}
	}

	/// A track that finishes while its PUBLISH awaits the answer still ends with TRACK_ENDED
	/// once answered, since the peer listed it on the PUBLISH; a reset would fail its listing.
	#[moq_clock::test]
	async fn a_track_finished_before_the_answer_ends_cleanly() {
		for version in DRAFTS {
			let mut h = harness(version);
			let mut request = h.request(&listing(false)).await;
			assert_eq!(next(&mut request).await.0, ietf::RequestOk::ID, "{version}");
			let mut published = HashMap::new();
			for _ in 0..2 {
				let (stream, _, name, _) = h.publish().await;
				published.insert(name, stream);
			}

			let mut video = published.remove("video").unwrap();
			let track = h.tracks.remove("video").unwrap();
			track.finish().unwrap();
			drop(track);
			moq_clock::sleep(Duration::from_millis(10)).await;
			publish_ok(&mut video).await;
			let (id, body) = next(&mut video).await;
			assert_eq!(id, ietf::PublishDone::ID, "{version}");
			let done: ietf::PublishDone = body.message(version).unwrap();
			assert_eq!(
				done.status_code,
				ietf::PublishDoneStatus::TrackEnded.code(version),
				"{version}"
			);
		}
	}

	/// A REQUEST_UPDATE with FORWARD=1 resumes a paused PUBLISH: its answer names where the
	/// subscription joins, and new groups flow. FORWARD=0 pauses it again.
	#[moq_clock::test]
	async fn an_update_resumes_and_pauses_a_publish() {
		for version in DRAFTS {
			let mut h = harness(version);
			let video = h.tracks.remove("video").unwrap();
			let mut group = video.append_group().unwrap();
			group.write_frame(crate::Timestamp::ZERO, b"cached".as_slice()).unwrap();
			group.finish().unwrap();

			let mut request = h.request(&listing(false)).await;
			assert_eq!(next(&mut request).await.0, ietf::RequestOk::ID, "{version}");
			let mut stream = loop {
				let (stream, _, name, _) = h.publish().await;
				if name == "video" {
					break stream;
				}
			};
			publish_ok(&mut stream).await;
			send(&mut stream, &update(2, true)).await;
			let (id, body) = next(&mut stream).await;
			assert_eq!(id, ietf::RequestOk::ID, "{version}");
			let ok: ietf::RequestOk = body.message(version).unwrap();
			assert_eq!(ok.largest, Some(Location { group: 0, object: 0 }), "{version}");

			let mut group = video.append_group().unwrap();
			group.write_frame(crate::Timestamp::ZERO, b"live".as_slice()).unwrap();
			group.finish().unwrap();
			// The joined group's remainder, then the live group.
			let mut opened = 0;
			loop {
				let recv = timeout(std::future::poll_fn(|cx| h.peer.poll_accept_uni(cx)))
					.await
					.unwrap();
				opened += 1;
				let mut reader = crate::coding::Reader::new(crate::transport::boxed::RecvStream::new(recv), version);
				let mut bytes = Vec::new();
				while let Some(chunk) = timeout(std::future::poll_fn(|cx| reader.poll_read_chunk(cx, 1024)))
					.await
					.unwrap()
				{
					bytes.extend_from_slice(&chunk);
				}
				assert!(!bytes.windows(6).any(|w| w == b"cached"), "{version}: resent the join");
				if bytes.windows(4).any(|w| w == b"live") {
					break;
				}
			}

			send(&mut stream, &update(4, false)).await;
			assert_eq!(next(&mut stream).await.0, ietf::RequestOk::ID, "{version}");
			let mut group = video.append_group().unwrap();
			group.write_frame(crate::Timestamp::ZERO, b"paused".as_slice()).unwrap();
			group.finish().unwrap();
			moq_clock::sleep(Duration::from_millis(10)).await;
			let more = futures::poll!(std::future::poll_fn(|cx| h.peer.poll_accept_uni(cx)));
			assert!(more.is_pending(), "{version}: a paused PUBLISH forwarded");

			// Ending the track while paused ends the PUBLISH, counting the streams it opened.
			drop(video);
			let (id, body) = next(&mut stream).await;
			assert_eq!(id, ietf::PublishDone::ID, "{version}");
			let done: ietf::PublishDone = body.message(version).unwrap();
			assert_eq!(done.stream_count, opened, "{version}");
		}
	}

	/// A listed compressed track is declared and served compressed only to a peer that
	/// negotiated FLATE; any other peer gets the plain copy with no declaration.
	#[moq_clock::test]
	async fn a_publish_compresses_only_when_negotiated() {
		for version in DRAFTS {
			for flate in [false, true] {
				let declared = peer::Peer {
					flate,
					..Default::default()
				};
				let mut h = harness_with(version, memory::Config::default(), declared);
				let data = h
					.broadcast
					.create_track("data", crate::track::Info::timed().with_compress(true))
					.unwrap();

				let mut request = h.request(&listing(false)).await;
				assert_eq!(next(&mut request).await.0, ietf::RequestOk::ID, "{version}");
				let mut stream = loop {
					let mut stream = timeout(Stream::accept(
						&mut crate::driver::Transport::new(h.peer.clone()),
						version,
					))
					.await
					.unwrap();
					assert_eq!(stream.reader.varint().await.unwrap(), ietf::Publish::ID);
					let body: ietf::Body = stream.reader.decode().await.unwrap();
					let publish: ietf::Publish = body.message(version).unwrap();
					if &*publish.track_name == "data" {
						assert_eq!(publish.properties.compress, flate, "{version} flate={flate}");
						break stream;
					}
				};
				publish_ok(&mut stream).await;
				send(&mut stream, &update(2, true)).await;
				assert_eq!(next(&mut stream).await.0, ietf::RequestOk::ID, "{version}");

				let payload = vec![b'a'; 1024];
				let mut group = data.append_group().unwrap();
				group.write_frame(crate::Timestamp::ZERO, payload.clone()).unwrap();
				group.finish().unwrap();
				let recv = timeout(std::future::poll_fn(|cx| h.peer.poll_accept_uni(cx)))
					.await
					.unwrap();
				let mut reader = crate::coding::Reader::new(crate::transport::boxed::RecvStream::new(recv), version);
				let mut bytes = Vec::new();
				while let Some(chunk) = timeout(std::future::poll_fn(|cx| reader.poll_read_chunk(cx, 1024)))
					.await
					.unwrap()
				{
					bytes.extend_from_slice(&chunk);
				}
				let plain = bytes.windows(payload.len()).any(|w| w == payload);
				assert_eq!(plain, !flate, "{version} flate={flate}: {} bytes", bytes.len());
			}
		}
	}

	/// FORWARD=1 asks for every track's objects, which we never push unasked: from
	/// draft-19 each track is skipped, and draft-18, with no way to skip, refuses it.
	#[moq_clock::test]
	async fn a_forwarding_listing_skips_every_track() {
		for version in DRAFTS {
			let mut h = harness(version);
			let mut request = h.request(&listing(true)).await;
			let (id, body) = next(&mut request).await;
			if version == Version::Draft18 {
				assert_eq!(id, ietf::RequestError::ID);
				assert_eq!(request_error(&body, version), request::NOT_SUPPORTED);
				continue;
			}
			assert_eq!(id, ietf::RequestOk::ID, "{version}");
			let mut skipped = Vec::new();
			for _ in 0..2 {
				let (id, body) = next(&mut request).await;
				assert_eq!(id, ietf::PublishSkipped::ID, "{version}");
				let skip: ietf::PublishSkipped = body.message(version).unwrap();
				assert_eq!(skip.suffix.as_str(), "cam", "{version}");
				skipped.push(skip.track_name.to_string());
			}
			skipped.sort();
			assert_eq!(skipped, ["audio", "video"], "{version}");
			let opened = futures::poll!(std::pin::pin!(Stream::accept(
				&mut crate::driver::Transport::new(h.peer.clone()),
				version
			)));
			assert!(opened.is_pending(), "{version}: a skipped track was PUBLISHed");
		}
	}

	/// A prefix overlapping an established listing is refused, and freed by its cancel.
	#[moq_clock::test]
	async fn an_overlapping_prefix_is_refused() {
		for version in DRAFTS {
			let mut h = harness(version);
			let mut first = h.request(&listing(false)).await;
			assert_eq!(next(&mut first).await.0, ietf::RequestOk::ID, "{version}");

			let mut nested = listing(false);
			nested.request_id = RequestId(2);
			nested.prefix = crate::Path::new("room/cam");
			let mut second = h.request(&nested).await;
			let (id, body) = next(&mut second).await;
			assert_eq!(id, ietf::RequestError::ID, "{version}");
			assert_eq!(request_error(&body, version), request::PREFIX_OVERLAP, "{version}");

			first.writer.abort(&Error::Cancel);
			moq_clock::sleep(Duration::from_millis(10)).await;
			nested.request_id = RequestId(4);
			let mut third = h.request(&nested).await;
			assert_eq!(next(&mut third).await.0, ietf::RequestOk::ID, "{version}");
		}
	}

	/// Draft-18 allows one subscription per track: a SUBSCRIBE to a PUBLISHed track is a
	/// duplicate. Later drafts serve it.
	#[moq_clock::test]
	async fn a_subscribe_to_a_published_track() {
		for version in DRAFTS {
			let mut h = harness(version);
			let mut request = h.request(&listing(false)).await;
			assert_eq!(next(&mut request).await.0, ietf::RequestOk::ID, "{version}");
			let _publishes = [h.publish().await, h.publish().await];

			let subscribe = ietf::Subscribe {
				request_id: RequestId(2),
				track_namespace: crate::Path::new("room/cam"),
				track_name: "video".into(),
				subscriber_priority: 128,
				group_order: GroupOrder::Descending,
				filter: Filter::NextObject,
				fill: None,
				properties_wanted: true,
				forward: true,
				rendezvous: false,
				range_filters: false,
				authorization_token: None,
				epoch: None,
				max_age: None,
				new_group: None,
			};
			let mut stream = h.request(&subscribe).await;
			let (id, body) = next(&mut stream).await;
			match version {
				Version::Draft18 => {
					assert_eq!(id, ietf::RequestError::ID);
					assert_eq!(request_error(&body, version), request::DUPLICATE_SUBSCRIPTION);
				}
				_ => assert_eq!(id, ietf::SubscribeOk::ID, "{version}"),
			}
		}
	}

	/// Each paused PUBLISH holds a subscription slot; past the cap the track is skipped
	/// (PUBLISH_BLOCKED, the same message, on draft-18) rather than held.
	#[moq_clock::test]
	async fn past_the_subscription_cap_a_track_is_skipped() {
		for version in DRAFTS {
			let mut h = harness(version);
			h.publisher.subscriptions = crate::session::Slots::new(1);
			let mut request = h.request(&listing(false)).await;
			assert_eq!(next(&mut request).await.0, ietf::RequestOk::ID, "{version}");
			let (_stream, _, published, _) = h.publish().await;
			let (id, body) = next(&mut request).await;
			assert_eq!(id, ietf::PublishSkipped::ID, "{version}");
			let skip: ietf::PublishSkipped = body.message(version).unwrap();
			assert_ne!(skip.track_name, published, "{version}");
		}
	}

	/// Cancelling the listing stops new PUBLISHes, while those already sent live on until
	/// their broadcast ends, which ends each with PUBLISH_DONE.
	#[moq_clock::test]
	async fn a_cancelled_listing_keeps_its_publishes() {
		for version in DRAFTS {
			let mut h = harness(version);
			let mut request = h.request(&listing(false)).await;
			assert_eq!(next(&mut request).await.0, ietf::RequestOk::ID, "{version}");
			let (mut first, ..) = h.publish().await;
			let (mut second, ..) = h.publish().await;
			publish_ok(&mut first).await;
			publish_ok(&mut second).await;

			request.writer.abort(&Error::Cancel);
			moq_clock::sleep(Duration::from_millis(10)).await;
			let _late = h
				.broadcast
				.create_track("captions", crate::track::Info::timed())
				.unwrap();
			moq_clock::sleep(Duration::from_millis(10)).await;
			let opened = futures::poll!(std::pin::pin!(Stream::accept(
				&mut crate::driver::Transport::new(h.peer.clone()),
				version
			)));
			assert!(opened.is_pending(), "{version}: PUBLISHed after the cancel");

			drop(h.broadcast);
			for stream in [&mut first, &mut second] {
				assert_eq!(next(stream).await.0, ietf::PublishDone::ID, "{version}");
			}
		}
	}

	/// On draft-18 a replacement listing skips the tracks whose PUBLISHes outlive the one it
	/// replaced, since each is still the peer's one subscription; later drafts PUBLISH them
	/// again.
	#[moq_clock::test]
	async fn a_relisting_skips_tracks_still_published_on_draft_18() {
		for version in [Version::Draft18, Version::Draft19] {
			let mut h = harness(version);
			let mut request = h.request(&listing(false)).await;
			assert_eq!(next(&mut request).await.0, ietf::RequestOk::ID, "{version}");
			let (mut first, ..) = h.publish().await;
			let (mut second, ..) = h.publish().await;
			publish_ok(&mut first).await;
			publish_ok(&mut second).await;
			request.writer.abort(&Error::Cancel);
			moq_clock::sleep(Duration::from_millis(10)).await;

			let relisting = ietf::SubscribeTracks {
				request_id: RequestId(2),
				..listing(false)
			};
			let mut relisting = h.request(&relisting).await;
			assert_eq!(next(&mut relisting).await.0, ietf::RequestOk::ID, "{version}");
			if version == Version::Draft18 {
				for _ in 0..2 {
					assert_eq!(next(&mut relisting).await.0, ietf::PublishSkipped::ID);
				}
				let opened = futures::poll!(std::pin::pin!(Stream::accept(
					&mut crate::driver::Transport::new(h.peer.clone()),
					version
				)));
				assert!(opened.is_pending(), "PUBLISHed a track twice");
			} else {
				let _publishes = [h.publish().await, h.publish().await];
			}
		}
	}

	/// On drafts 18 and 19 a PUBLISH_OK may ask for a new group only if the PUBLISH sent
	/// DYNAMIC_GROUPS; one that asks anyway ends the PUBLISH in error.
	#[moq_clock::test]
	async fn a_publish_ok_new_group_needs_dynamic_groups() {
		for version in [Version::Draft18, Version::Draft19] {
			let mut h = harness(version);
			let mut request = h.request(&listing(false)).await;
			assert_eq!(next(&mut request).await.0, ietf::RequestOk::ID, "{version}");
			let (mut stream, ..) = h.publish().await;
			stream.writer.varint(ietf::RequestOk::ID).await.unwrap();
			stream
				.writer
				.encode(&ietf::PublishOk {
					request_id: None,
					forward: true,
					subscriber_priority: None,
					group_order: None,
					filter: Some(Filter::NextObject),
					new_group: Some(0),
				})
				.await
				.unwrap();
			let (id, body) = next(&mut stream).await;
			assert_eq!(id, ietf::PublishDone::ID, "{version}");
			let done: ietf::PublishDone = body.message(version).unwrap();
			assert!(done.end(version).is_err(), "{version}: ended cleanly");
		}
	}

	/// Draft-18 and 19 let PUBLISH_OK forward at once, with no update to answer.
	#[moq_clock::test]
	async fn a_forwarding_publish_ok_resumes() {
		for version in [Version::Draft18, Version::Draft19] {
			let mut h = harness(version);
			let video = h.tracks.remove("video").unwrap();
			let mut request = h.request(&listing(false)).await;
			assert_eq!(next(&mut request).await.0, ietf::RequestOk::ID, "{version}");
			let mut stream = loop {
				let (stream, _, name, _) = h.publish().await;
				if name == "video" {
					break stream;
				}
			};
			stream.writer.varint(ietf::RequestOk::ID).await.unwrap();
			stream
				.writer
				.encode(&ietf::PublishOk {
					request_id: None,
					forward: true,
					subscriber_priority: Some(128),
					group_order: Some(GroupOrder::Ascending),
					filter: Some(Filter::NextObject),
					new_group: None,
				})
				.await
				.unwrap();

			let mut group = video.append_group().unwrap();
			group.write_frame(crate::Timestamp::ZERO, b"live".as_slice()).unwrap();
			group.finish().unwrap();
			timeout(std::future::poll_fn(|cx| h.peer.poll_accept_uni(cx)))
				.await
				.unwrap();
			drop(video);
			let (id, body) = next(&mut stream).await;
			assert_eq!(id, ietf::PublishDone::ID, "{version}: answered no update");
			let done: ietf::PublishDone = body.message(version).unwrap();
			assert_eq!(done.stream_count, 1, "{version}");
		}
	}

	/// A REQUEST_UPDATE on the listing admits its Request ID like any other: one of the
	/// wrong parity closes the session rather than only refusing the update.
	#[moq_clock::test]
	async fn a_listing_update_admits_its_request_id() {
		for version in DRAFTS {
			let mut h = harness(version);
			let mut request = h.request(&listing(false)).await;
			assert_eq!(next(&mut request).await.0, ietf::RequestOk::ID, "{version}");
			// An odd Request ID is ours to allocate, never the client peer's.
			let bare = ietf::SubscribeUpdate {
				forward: None,
				filter: None,
				..update(1, false)
			};
			send(&mut request, &bare).await;
			let closed = timeout(std::future::poll_fn(|cx| h.peer.poll_closed(cx))).await;
			assert!(
				matches!(&closed, memory::Error::Session { reason, .. } if reason == "invalid request ID"),
				"{version}: {closed:?}"
			);
		}
	}

	/// A REQUEST_UPDATE on the listing is answered with REQUEST_ERROR. From draft-20 it may
	/// carry FORWARD, which is refused the same way rather than closing the session.
	#[moq_clock::test]
	async fn a_listing_update_is_refused() {
		for version in DRAFTS {
			let mut h = harness(version);
			let mut request = h.request(&listing(false)).await;
			assert_eq!(next(&mut request).await.0, ietf::RequestOk::ID, "{version}");
			let refused = ietf::SubscribeUpdate {
				filter: None,
				forward: Filter::is_draft20(version).then_some(false),
				..update(2, false)
			};
			send(&mut request, &refused).await;
			let (id, body) = next(&mut request).await;
			assert_eq!(id, ietf::RequestError::ID, "{version}");
			assert_eq!(request_error(&body, version), request::NOT_SUPPORTED, "{version}");
		}
	}

	/// A listing update past our GOAWAY is refused as GOING_AWAY, not as unsupported.
	#[moq_clock::test]
	async fn a_listing_update_after_goaway_is_going_away() {
		for version in DRAFTS {
			let mut h = harness(version);
			let mut request = h.request(&listing(false)).await;
			assert_eq!(next(&mut request).await.0, ietf::RequestOk::ID, "{version}");
			h.publisher.control.goaway().unwrap();
			let refused = ietf::SubscribeUpdate {
				filter: None,
				forward: None,
				..update(2, false)
			};
			send(&mut request, &refused).await;
			let (id, body) = next(&mut request).await;
			assert_eq!(id, ietf::RequestError::ID, "{version}");
			let going_away = request::to_code(&Error::GoingAway, request::Kind::SubscribeNamespace, version);
			assert_eq!(request_error(&body, version), going_away, "{version}");
		}
	}

	/// NEW_GROUP_REQUEST resuming a paused PUBLISH is a join under the track's refresh
	/// ratio, as on a SUBSCRIBE, even when the filter's fixed start is not. The listing's
	/// own is each PUBLISH's default.
	#[moq_clock::test]
	async fn a_resume_asks_for_a_new_group() {
		for on_listing in [false, true] {
			resume_asks_for_a_new_group(on_listing).await;
		}
	}

	async fn resume_asks_for_a_new_group(on_listing: bool) {
		// Draft-22 sends no NEW_GROUP_REQUEST on SUBSCRIBE_TRACKS.
		let version = Version::Draft21;
		let mut h = harness(version);
		let live = h
			.broadcast
			.create_track("live", crate::track::Info::timed().with_refresh(Some(1)))
			.unwrap();
		let mut group = live.create_group(group::Info { sequence: 4 }).unwrap();
		group.write_frame(crate::Timestamp::ZERO, b"key".as_slice()).unwrap();
		group.write_frame(crate::Timestamp::ZERO, b"delta".as_slice()).unwrap();

		let request = ietf::SubscribeTracks {
			new_group: on_listing.then_some(0),
			..listing(false)
		};
		let mut request = h.request(&request).await;
		assert_eq!(next(&mut request).await.0, ietf::RequestOk::ID);
		let mut stream = loop {
			let (stream, _, name, _) = h.publish().await;
			if name == "live" {
				break stream;
			}
		};
		publish_ok(&mut stream).await;
		send(
			&mut stream,
			&ietf::SubscribeUpdate {
				filter: Some(Filter::Absolute {
					start: Location { group: 4, object: 0 },
					end: None,
				}),
				new_group: (!on_listing).then_some(0),
				..update(2, true)
			},
		)
		.await;
		assert_eq!(next(&mut stream).await.0, ietf::RequestOk::ID);
		assert_eq!(
			live.demand().wanted(),
			Some(5),
			"on_listing={on_listing}: the join never reached the track"
		);
	}

	/// From draft-20 an update may ask for a new group only if its PUBLISH sent
	/// DYNAMIC_GROUPS, which INCLUDE_PROPERTIES=0 leaves out even for a dynamic track.
	#[moq_clock::test]
	async fn a_new_group_needs_the_advertised_dynamic_groups() {
		for version in [Version::Draft20, Version::Draft21, Version::Draft22] {
			let mut h = harness(version);
			let live = h
				.broadcast
				.create_track("live", crate::track::Info::timed().with_refresh(Some(1)))
				.unwrap();
			let request = ietf::SubscribeTracks {
				properties_wanted: false,
				..listing(false)
			};
			let mut request = h.request(&request).await;
			assert_eq!(next(&mut request).await.0, ietf::RequestOk::ID, "{version}");
			let mut stream = loop {
				let (stream, _, name, _) = h.publish().await;
				if name == "live" {
					break stream;
				}
			};
			publish_ok(&mut stream).await;
			send(
				&mut stream,
				&ietf::SubscribeUpdate {
					new_group: Some(0),
					..update(2, true)
				},
			)
			.await;
			let (id, body) = next(&mut stream).await;
			assert_eq!(id, ietf::RequestError::ID, "{version}");
			assert_eq!(request_error(&body, version), request::NOT_SUPPORTED, "{version}");
			assert_eq!(
				live.demand().wanted(),
				None,
				"{version}: the refused join reached the track"
			);
		}
	}

	/// While forwarding, an update's NEW_GROUP_REQUEST also needs the advertised
	/// DYNAMIC_GROUPS, which INCLUDE_PROPERTIES=0 leaves out.
	#[moq_clock::test]
	async fn a_forwarding_new_group_needs_the_advertised_dynamic_groups() {
		let version = Version::Draft22;
		let mut h = harness(version);
		let _live = h
			.broadcast
			.create_track("live", crate::track::Info::timed().with_refresh(Some(1)))
			.unwrap();
		let request = ietf::SubscribeTracks {
			properties_wanted: false,
			..listing(false)
		};
		let mut request = h.request(&request).await;
		assert_eq!(next(&mut request).await.0, ietf::RequestOk::ID);
		let mut stream = loop {
			let (stream, _, name, _) = h.publish().await;
			if name == "live" {
				break stream;
			}
		};
		publish_ok(&mut stream).await;
		send(&mut stream, &update(2, true)).await;
		assert_eq!(next(&mut stream).await.0, ietf::RequestOk::ID);
		let ask = ietf::SubscribeUpdate {
			forward: None,
			filter: None,
			new_group: Some(0),
			..update(4, true)
		};
		send(&mut stream, &ask).await;
		let (id, body) = next(&mut stream).await;
		assert_eq!(id, ietf::RequestError::ID);
		assert_eq!(request_error(&body, version), request::NOT_SUPPORTED);
	}

	/// A paused subscription opens no groups, so an update that pauses may not ask for one,
	/// while forwarding as while paused.
	#[moq_clock::test]
	async fn a_pausing_new_group_is_refused() {
		for version in [Version::Draft18, Version::Draft22] {
			let mut h = harness(version);
			let _live = h
				.broadcast
				.create_track("live", crate::track::Info::timed().with_refresh(Some(1)))
				.unwrap();
			let mut request = h.request(&listing(false)).await;
			assert_eq!(next(&mut request).await.0, ietf::RequestOk::ID, "{version}");
			let mut stream = loop {
				let (stream, _, name, _) = h.publish().await;
				if name == "live" {
					break stream;
				}
			};
			publish_ok(&mut stream).await;
			send(&mut stream, &update(2, true)).await;
			assert_eq!(next(&mut stream).await.0, ietf::RequestOk::ID, "{version}");
			let ask = ietf::SubscribeUpdate {
				new_group: Some(0),
				..update(4, false)
			};
			send(&mut stream, &ask).await;
			let (id, body) = next(&mut stream).await;
			assert_eq!(id, ietf::RequestError::ID, "{version}");
			assert_eq!(request_error(&body, version), request::NOT_SUPPORTED, "{version}");
		}
	}

	/// An update taken while forwarding outlives a pause: the next resume keeps it.
	#[moq_clock::test]
	async fn a_forwarding_update_outlives_a_pause() {
		for version in [Version::Draft18, Version::Draft22] {
			let mut h = harness(version);
			let video = h.tracks.remove("video").unwrap();
			let mut request = h.request(&listing(false)).await;
			assert_eq!(next(&mut request).await.0, ietf::RequestOk::ID, "{version}");
			let mut stream = loop {
				let (stream, _, name, _) = h.publish().await;
				if name == "video" {
					break stream;
				}
			};
			publish_ok(&mut stream).await;
			send(&mut stream, &update(2, true)).await;
			assert_eq!(next(&mut stream).await.0, ietf::RequestOk::ID, "{version}");
			let reprice = ietf::SubscribeUpdate {
				subscriber_priority: Some(7),
				filter: None,
				..update(4, false)
			};
			send(&mut stream, &reprice).await;
			assert_eq!(next(&mut stream).await.0, ietf::RequestOk::ID, "{version}");
			send(&mut stream, &update(6, true)).await;
			assert_eq!(next(&mut stream).await.0, ietf::RequestOk::ID, "{version}");
			let priority = video.subscription().map(|subscription| subscription.priority);
			assert_eq!(priority, Some(crate::ietf::priority::from_wire(7)), "{version}");
		}
	}

	/// A paused listing's FILL_PARAMETERS (draft-20 and draft-21; draft-22 sends none on
	/// SUBSCRIBE_TRACKS) open no fill, even once a PUBLISH resumes without its own
	/// (draft-22 section 3.4.1).
	#[moq_clock::test]
	async fn a_listing_fill_opens_nothing() {
		for version in [Version::Draft20, Version::Draft21] {
			let mut h = harness(version);
			let video = h.tracks.remove("video").unwrap();
			let mut group = video.append_group().unwrap();
			group.write_frame(crate::Timestamp::ZERO, b"cached".as_slice()).unwrap();

			let request = ietf::SubscribeTracks {
				filter: Some(Filter::NextObject),
				fill: Some(ietf::Fill {
					filter: Some(Filter::Relative(1)),
					range_filters: false,
				}),
				..listing(false)
			};
			let mut request = h.request(&request).await;
			assert_eq!(next(&mut request).await.0, ietf::RequestOk::ID, "{version}");
			let mut stream = loop {
				let (stream, _, name, _) = h.publish().await;
				if name == "video" {
					break stream;
				}
			};
			publish_ok(&mut stream).await;
			let resume = ietf::SubscribeUpdate {
				filter: None,
				..update(2, true)
			};
			send(&mut stream, &resume).await;
			assert_eq!(next(&mut stream).await.0, ietf::RequestOk::ID, "{version}");

			// The joined group's own stream carries only what follows the join; a fill would
			// carry the cached object.
			group.finish().unwrap();
			moq_clock::sleep(Duration::from_millis(10)).await;
			while let Poll::Ready(recv) = futures::poll!(std::future::poll_fn(|cx| h.peer.poll_accept_uni(cx))) {
				let mut reader =
					crate::coding::Reader::new(crate::transport::boxed::RecvStream::new(recv.unwrap()), version);
				let mut bytes = Vec::new();
				while let Some(chunk) = timeout(std::future::poll_fn(|cx| reader.poll_read_chunk(cx, 1024)))
					.await
					.unwrap()
				{
					bytes.extend_from_slice(&chunk);
				}
				assert!(
					!bytes.windows(6).any(|w| w == b"cached"),
					"{version}: the listing's fill opened"
				);
			}
		}
	}

	/// On draft-18 a track the peer already SUBSCRIBEd to is skipped rather than PUBLISHed
	/// a second time; later drafts PUBLISH it.
	#[moq_clock::test]
	async fn a_subscribed_track_is_skipped_on_draft_18() {
		for version in [Version::Draft18, Version::Draft19] {
			let mut h = harness(version);
			let subscribe = ietf::Subscribe {
				request_id: RequestId(0),
				track_namespace: crate::Path::new("room/cam"),
				track_name: "video".into(),
				subscriber_priority: 128,
				group_order: GroupOrder::Descending,
				filter: Filter::NextObject,
				fill: None,
				properties_wanted: true,
				forward: true,
				rendezvous: false,
				range_filters: false,
				authorization_token: None,
				epoch: None,
				max_age: None,
				new_group: None,
			};
			let mut subscription = h.request(&subscribe).await;
			assert_eq!(next(&mut subscription).await.0, ietf::SubscribeOk::ID, "{version}");

			let request = ietf::SubscribeTracks {
				request_id: RequestId(2),
				..listing(false)
			};
			let mut request = h.request(&request).await;
			assert_eq!(next(&mut request).await.0, ietf::RequestOk::ID, "{version}");
			let (_audio, _, name, _) = h.publish().await;
			if version == Version::Draft18 {
				assert_eq!(name, "audio");
				let (id, body) = next(&mut request).await;
				assert_eq!(id, ietf::PublishSkipped::ID);
				let skip: ietf::PublishSkipped = body.message(version).unwrap();
				assert_eq!(skip.track_name, "video");
			} else {
				let (_, _, other, _) = h.publish().await;
				assert!([name, other].contains(&"video".to_string()), "{version}");
			}
		}
	}

	/// From draft-20 each PUBLISH states the listing's subscription parameters, which the
	/// subscriber otherwise takes as the defaults; earlier drafts carry them on PUBLISH_OK.
	/// Draft-22 sends none of them on SUBSCRIBE_TRACKS.
	#[moq_clock::test]
	async fn a_listing_states_its_parameters_on_each_publish() {
		for version in DRAFTS.into_iter().filter(|version| *version != Version::Draft22) {
			let mut h = harness(version);
			let request = ietf::SubscribeTracks {
				subscriber_priority: Some(7),
				group_order: Some(GroupOrder::Ascending),
				filter: Some(Filter::NextObject),
				..listing(false)
			};
			let mut request = h.request(&request).await;
			assert_eq!(next(&mut request).await.0, ietf::RequestOk::ID, "{version}");
			let mut stream = timeout(Stream::accept(
				&mut crate::driver::Transport::new(h.peer.clone()),
				version,
			))
			.await
			.unwrap();
			assert_eq!(stream.reader.varint().await.unwrap(), ietf::Publish::ID);
			let body: ietf::Body = stream.reader.decode().await.unwrap();
			let publish: ietf::Publish = body.message(version).unwrap();
			let stated = Filter::is_draft20(version);
			assert_eq!(publish.subscriber_priority, stated.then_some(7), "{version}");
			assert_eq!(
				publish.group_order,
				stated.then_some(GroupOrder::Ascending),
				"{version}"
			);
			assert_eq!(publish.filter, stated.then_some(Filter::NextObject), "{version}");
		}
	}

	/// A forwarding PUBLISH ends with PUBLISH_DONE once its track is no longer listed, here
	/// because its broadcast's route went, even while the track itself goes on. The track
	/// has not ended, so neither does the PUBLISH cleanly.
	#[moq_clock::test]
	async fn an_unlisted_forwarding_publish_ends() {
		for version in [Version::Draft18, Version::Draft22] {
			let mut h = harness(version);
			let video = h.tracks.remove("video").unwrap();
			let mut request = h.request(&listing(false)).await;
			assert_eq!(next(&mut request).await.0, ietf::RequestOk::ID, "{version}");
			let mut stream = loop {
				let (stream, _, name, _) = h.publish().await;
				if name == "video" {
					break stream;
				}
			};
			publish_ok(&mut stream).await;
			send(&mut stream, &update(2, true)).await;
			assert_eq!(next(&mut stream).await.0, ietf::RequestOk::ID, "{version}");

			drop(h.broadcast);
			let mut group = video.append_group().unwrap();
			group.write_frame(crate::Timestamp::ZERO, b"after".as_slice()).unwrap();
			group.finish().unwrap();
			let (id, body) = next(&mut stream).await;
			assert_eq!(id, ietf::PublishDone::ID, "{version}");
			let done: ietf::PublishDone = body.message(version).unwrap();
			assert!(done.end(version).is_err(), "{version}: ended cleanly");
		}
	}

	/// A paused PUBLISH ends in error once its track is unlisted, here because its
	/// broadcast's route went, never with the clean end that says the track is over.
	#[moq_clock::test]
	async fn an_unlisted_paused_publish_ends_in_error() {
		for version in DRAFTS {
			let mut h = harness(version);
			let mut request = h.request(&listing(false)).await;
			assert_eq!(next(&mut request).await.0, ietf::RequestOk::ID, "{version}");
			let (mut stream, ..) = h.publish().await;
			publish_ok(&mut stream).await;

			drop(h.broadcast);
			let (id, body) = next(&mut stream).await;
			assert_eq!(id, ietf::PublishDone::ID, "{version}");
			let done: ietf::PublishDone = body.message(version).unwrap();
			assert!(done.end(version).is_err(), "{version}: ended cleanly");
		}
	}

	/// A listing ending through a refused update still unlists its tracks while the peer
	/// withholds the credit its REQUEST_ERROR needs.
	#[moq_clock::test]
	async fn a_closing_listing_still_unlists() {
		for version in [Version::Draft18, Version::Draft22] {
			let config = memory::Config {
				stream_buffer: 32,
				..Default::default()
			};
			let mut h = harness_with(version, config, peer::Peer::default());
			// The REQUEST_OK stays unread, leaving no room for the REQUEST_ERROR.
			let mut request = h.request(&listing(false)).await;
			let (mut stream, ..) = h.publish().await;
			publish_ok(&mut stream).await;
			let refused = ietf::SubscribeUpdate {
				filter: None,
				forward: None,
				..update(2, false)
			};
			send(&mut request, &refused).await;
			moq_clock::sleep(Duration::from_millis(10)).await;

			// A new instance takes the path while the old one goes on: only its route ends.
			let _restarted = h._origin.publish("room/cam", crate::origin::Route::default()).unwrap();
			let (id, body) = next(&mut stream).await;
			assert_eq!(id, ietf::PublishDone::ID, "{version}");
			let done: ietf::PublishDone = body.message(version).unwrap();
			assert!(done.end(version).is_err(), "{version}: ended cleanly");
			assert_eq!(next(&mut request).await.0, ietf::RequestOk::ID, "{version}");
			assert_eq!(next(&mut request).await.0, ietf::RequestError::ID, "{version}");
		}
	}

	/// With MoQ Auth, no track is listed until the peer answers our setup credential, whose
	/// grant may narrow what the listing shows, and a listing cancelled while waiting frees
	/// its prefix.
	#[moq_clock::test]
	async fn a_listing_waits_for_the_setup_answer() {
		for version in [Version::Draft18, Version::Draft22] {
			let mut h = harness(version);
			let auth = crate::auth::Link::new(true);
			let _cred = auth.present(bytes::Bytes::from_static(b"cred"), true).unwrap();
			let setup = peer::PeerSetup::default();
			setup.set(peer::Peer {
				auth: true,
				..Default::default()
			});
			h.publisher = Publisher::new(
				crate::time::sim(),
				crate::driver::Transport::new(h.session.clone()),
				h._origin.consume(),
				Control::new(None, false),
				None,
				setup,
				version,
			)
			.with_auth(auth.clone());

			let first = h.request(&listing(false)).await;
			moq_clock::sleep(Duration::from_millis(10)).await;
			let opened = futures::poll!(std::pin::pin!(Stream::accept(
				&mut crate::driver::Transport::new(h.peer.clone()),
				version
			)));
			assert!(opened.is_pending(), "{version}: listed a track before the setup answer");

			first.writer.abort(&Error::Cancel);
			moq_clock::sleep(Duration::from_millis(10)).await;
			let mut again = listing(false);
			again.request_id = RequestId(2);
			let mut request = h.request(&again).await;
			assert_eq!(next(&mut request).await.0, ietf::RequestOk::ID, "{version}");

			auth.granted(0, crate::auth::Grant::all());
			let (mut stream, ..) = h.publish().await;
			publish_ok(&mut stream).await;
		}
	}

	/// Tracks under a namespace our grant does not cover are not listed until it does, and
	/// a grant that stops covering them ends their PUBLISHes.
	#[moq_clock::test]
	async fn a_listing_follows_the_publish_grant() {
		let nothing = || crate::auth::Grant {
			publish: crate::Patterns::new(),
			subscribe: crate::Patterns::new(),
			expires: None,
		};
		for version in [Version::Draft18, Version::Draft22] {
			let mut h = harness(version);
			let auth = crate::auth::Link::new(false);
			auth.handle().authorize(&nothing());
			h.publisher = h.publisher.clone().with_auth(auth.clone());

			let mut request = h.request(&listing(false)).await;
			assert_eq!(next(&mut request).await.0, ietf::RequestOk::ID, "{version}");
			moq_clock::sleep(Duration::from_millis(10)).await;
			let opened = futures::poll!(std::pin::pin!(Stream::accept(
				&mut crate::driver::Transport::new(h.peer.clone()),
				version
			)));
			assert!(opened.is_pending(), "{version}: listed a track the grant denies");

			auth.handle().authorize(&crate::auth::Grant::all());
			let (mut first, ..) = h.publish().await;
			let (mut second, ..) = h.publish().await;
			publish_ok(&mut first).await;
			publish_ok(&mut second).await;

			auth.handle().authorize(&nothing());
			for stream in [&mut first, &mut second] {
				assert_eq!(next(stream).await.0, ietf::PublishDone::ID, "{version}");
			}
		}
	}
}
