//! Lists a moq-transport peer's tracks through the TRACKS hook (draft-18+).
//!
//! A listed broadcast sends SUBSCRIBE_TRACKS with FORWARD=0, and each track it lists is
//! one paused subscription held for the session: the PUBLISH the peer answers with,
//! accepted paused, or for a track the peer skips, a SUBSCRIBE we open paused. That
//! subscription is both the listing entry, removed only by its PUBLISH_DONE with TRACK_ENDED,
//! and the track's upstream copy: a reader resumes it with REQUEST_UPDATE and pauses it
//! again once the last one leaves, rather than opening a second subscription.
//!
//! One SUBSCRIBE_TRACKS covers each nesting chain of listed broadcasts, since the peer
//! refuses an overlapping prefix: a nested listing reuses the broader request and filters
//! locally, and a broader one replaces the nested requests once the peer has closed them,
//! keeping the subscriptions they made. Any end of a held subscription other than
//! TRACK_ENDED fails every listing its request serves, which then asks again, rather
//! than hide a track that may still be live.
//!
//! Nothing goes out until a broadcast is listed: many peers do not implement
//! SUBSCRIBE_TRACKS yet, and one that refuses, resets, or ignores it fails or empties only
//! that listing, leaving the session and its SUBSCRIBEs alone.

use crate::driver::Transport;
use std::sync::Arc;

use super::*;
use crate::ietf::request_stream::{self, Framed, Messages};

/// A listed track, by its namespace and name.
pub(super) type TrackKey = (Path, Arc<str>);

/// How a PUBLISH_DONE leaves the listing: only TRACK_ENDED removes the track. Any other
/// status, INTERNAL_ERROR included, fails the listing, so it asks again rather than hide a
/// track that may still be live.
pub(super) fn listing_end(done: &ietf::PublishDone, version: Version) -> Result<(), Error> {
	match done.status_code == ietf::PublishDoneStatus::TrackEnded.code() {
		true => Ok(()),
		false => Err(done
			.end(version)
			.err()
			.unwrap_or(Error::Remote(u32::try_from(done.status_code).unwrap_or(u32::MAX)))),
	}
}

/// A paused upstream subscription and the request stream that controls it.
pub(super) struct Upstream {
	pub(super) token_grant: Option<crate::auth::RequestGrant>,
	pub(super) stream: Stream<Transport, Version>,
	pub(super) messages: Messages,
	pub(super) request_id: RequestId,
	pub(super) alias: u64,
	/// The Track Properties the PUBLISH or SUBSCRIBE_OK declared.
	pub(super) properties: ietf::Properties,
	/// REQUEST_UPDATE answers still owed, read before anything else.
	pub(super) owed: u64,
	/// The copy a reader left lingering, which a failed upstream aborts.
	pub(super) copy: Option<track::Producer>,
}

/// Hands a paused subscription from the task watching it to a subscription resuming it.
#[derive(Default)]
pub(super) struct Handoff {
	/// A subscription asked for it.
	wanted: bool,
	/// Handed over by the watcher.
	upstream: Option<Upstream>,
	/// The entry was let go, so the watcher cancels the subscription.
	gone: bool,
}

/// One listed track and the upstream subscription that holds it.
struct Entry {
	/// Its TRACK_INFO, once known: a paused SUBSCRIBE learns it from its SUBSCRIBE_OK.
	info: Option<track::Info>,
	/// The subscription holding it, once known.
	request_id: Option<RequestId>,
	/// Set while it is paused, or still opening, and a watcher owns it. `None` while a
	/// subscription forwarding it owns it.
	handoff: Option<kio::Shared<Handoff>>,
	/// Discovery state the peer makes us hold, as for a lite listed track.
	_slot: crate::session::Slot,
}

impl Drop for Entry {
	fn drop(&mut self) {
		if let Some(handoff) = &self.handoff {
			handoff.lock().gone = true;
		}
	}
}

/// A broadcast listed through this session.
struct Listener {
	path: Path,
	lister: Arc<broadcast::Lister>,
	/// Set to the error that fails the listing.
	failed: kio::Producer<Option<Error>>,
}

/// One SUBSCRIBE_TRACKS, from when it is wanted until the peer has closed it.
struct Request {
	prefix: Path,
	/// Dropped to cancel it; `None` once cancelled, while it leaves the wire.
	live: Option<kio::Producer<()>>,
	/// Closes once it is off the wire, so an overlapping one may go out.
	gone: kio::Consumer<()>,
}

/// The session's listings of the peer's tracks.
#[derive(Default)]
pub(super) struct Listings {
	listeners: HashMap<u64, Listener>,
	requests: HashMap<u64, Request>,
	entries: HashMap<TrackKey, Entry>,
	next: u64,
}

/// A SUBSCRIBE_TRACKS for its task to send.
struct Opening {
	key: u64,
	prefix: Path,
	/// The overlapping requests that must leave the wire first.
	blockers: Vec<kio::Consumer<()>>,
	live: kio::Consumer<()>,
	gone: kio::Producer<()>,
}

impl Listings {
	fn key(&mut self) -> u64 {
		self.next += 1;
		self.next
	}

	/// The standing request that covers `namespace`; at most one does.
	fn covering(&self, namespace: &Path) -> Option<u64> {
		self.requests
			.iter()
			.find(|(_, request)| request.live.is_some() && namespace.has_prefix(&request.prefix))
			.map(|(key, _)| *key)
	}

	fn listers<'a>(&'a self, namespace: &'a Path) -> impl Iterator<Item = &'a broadcast::Lister> + 'a {
		self.listeners
			.values()
			.filter(move |listener| &listener.path == namespace)
			.map(|listener| &*listener.lister)
	}

	fn listen(
		&mut self,
		path: Path,
		lister: Arc<broadcast::Lister>,
	) -> (u64, kio::Consumer<Option<Error>>, Option<Opening>) {
		// What a broader listing already holds is listed at once.
		for ((namespace, name), entry) in &self.entries {
			if namespace == &path
				&& let Some(info) = &entry.info
			{
				let _ = lister.add(name.clone(), info.clone());
			}
		}
		// This hop never waits on its upstream: later tracks follow as live adds.
		lister.synced();
		let failed = kio::Producer::new(None);
		let consumer = failed.consume();
		let key = self.key();
		self.listeners.insert(
			key,
			Listener {
				path: path.clone(),
				lister,
				failed,
			},
		);
		if self.covering(&path).is_some() {
			return (key, consumer, None);
		}
		// Nested requests give way to this broader one, which keeps what they hold.
		for request in self.requests.values_mut() {
			if request.prefix.has_prefix(&path) {
				request.live = None;
			}
		}
		let blockers = self
			.requests
			.values()
			.filter(|request| request.prefix.has_prefix(&path) || path.has_prefix(&request.prefix))
			.map(|request| request.gone.clone())
			.collect();
		let live = kio::Producer::new(());
		let gone = kio::Producer::new(());
		let request = self.key();
		self.requests.insert(
			request,
			Request {
				prefix: path.clone(),
				gone: gone.consume(),
				live: Some(live.clone()),
			},
		);
		let opening = Opening {
			key: request,
			prefix: path,
			blockers,
			live: live.consume(),
			gone,
		};
		(key, consumer, Some(opening))
	}

	fn unlisten(&mut self, key: u64) {
		self.listeners.remove(&key);
		self.release();
	}

	/// Cancel each standing request nobody lists under, and let go of each held track no
	/// standing request covers.
	fn release(&mut self) {
		for request in self.requests.values_mut() {
			if request.live.is_some()
				&& !self
					.listeners
					.values()
					.any(|listener| listener.path.has_prefix(&request.prefix))
			{
				request.live = None;
			}
		}
		let standing: Vec<Path> = self
			.requests
			.values()
			.filter(|request| request.live.is_some())
			.map(|request| request.prefix.clone())
			.collect();
		self.entries
			.retain(|(namespace, _), _| standing.iter().any(|prefix| namespace.has_prefix(prefix)));
	}

	/// Fail every listing the request covering `namespace` serves with `err`, and cancel it:
	/// they ask again rather than miss a track that may still be live.
	fn fail(&mut self, namespace: &Path, err: Error) {
		let Some(key) = self.covering(namespace) else {
			return;
		};
		let request = self.requests.get_mut(&key).expect("covering");
		request.live = None;
		let prefix = request.prefix.clone();
		tracing::debug!(%prefix, %err, "track listing failed");
		for listener in self.listeners.values() {
			if listener.path.has_prefix(&prefix)
				&& let Ok(mut failed) = listener.failed.write()
			{
				*failed = Some(err.clone());
			}
		}
		self.release();
	}

	fn insert(&mut self, key: TrackKey, entry: Entry) {
		if let Some(info) = &entry.info {
			for lister in self.listers(&key.0) {
				let _ = lister.add(key.1.clone(), info.clone());
			}
		}
		self.entries.insert(key, entry);
	}

	/// Unlist the entry for `key`, as a track that `finished` or left unfinished.
	fn remove(&mut self, key: &TrackKey, finished: bool) {
		let Some(entry) = self.entries.remove(key) else {
			return;
		};
		if entry.info.is_some() {
			for lister in self.listers(&key.0) {
				let _ = match finished {
					true => lister.finish(&key.1),
					false => lister.remove(&key.1),
				};
			}
		}
	}

	/// The entry held by the subscription with `request_id`, if this is it.
	fn held_by(&mut self, key: &TrackKey, request_id: RequestId) -> Option<&mut Entry> {
		self.entries
			.get_mut(key)
			.filter(|entry| entry.request_id == Some(request_id))
	}

	/// The entry `handoff` belongs to, if it still stands.
	fn watched_by(&mut self, key: &TrackKey, handoff: &kio::Shared<Handoff>) -> Option<&mut Entry> {
		self.entries
			.get_mut(key)
			.filter(|entry| entry.handoff.as_ref().is_some_and(|held| held.same_channel(handoff)))
	}

	/// End the entry held by the subscription with `request_id`: TRACK_ENDED unlists it as
	/// finished, and anything else fails its listings.
	fn ended(&mut self, key: &TrackKey, request_id: RequestId, end: Result<(), Error>) {
		if self.held_by(key, request_id).is_none() {
			return;
		}
		match end {
			Ok(()) => self.remove(key, true),
			Err(err) => {
				self.remove(key, false);
				self.fail(&key.0, err);
			}
		}
	}
}

/// A broadcast's listing through this session, until dropped.
pub(super) struct Listening {
	listings: kio::Lock<Listings>,
	key: u64,
	failed: kio::Consumer<Option<Error>>,
}

impl Listening {
	/// Ready with the error that failed the listing.
	pub(super) fn poll_failed(&self, waiter: &kio::Waiter) -> Poll<Error> {
		match self.failed.poll(waiter, |failed| match &**failed {
			Some(err) => Poll::Ready(err.clone()),
			None => Poll::Pending,
		}) {
			Poll::Ready(Ok(err)) => Poll::Ready(err),
			Poll::Ready(Err(_)) => Poll::Ready(Error::Cancel),
			Poll::Pending => Poll::Pending,
		}
	}
}

impl Drop for Listening {
	fn drop(&mut self) {
		self.listings.lock().unlisten(self.key);
	}
}

/// What to do with a PUBLISH.
enum Offer {
	/// No listing asked for it.
	Unsolicited,
	/// Refused with this code and reason.
	Refuse(u64, &'static str),
	/// Accepted paused and held as the entry `handoff` watches.
	Hold(kio::Shared<Handoff>),
}

impl Subscriber {
	/// List `path`'s tracks into `lister` until the returned guard drops, through a
	/// SUBSCRIBE_TRACKS covering it.
	pub(super) fn listen(&self, path: Path, lister: Arc<broadcast::Lister>) -> Listening {
		let (key, failed, opening) = self.listings.lock().listen(path, lister);
		if let Some(opening) = opening {
			let mut task = SubscribeTracks::new(self.clone(), opening);
			self.tasks.push(move |waiter: &kio::Waiter| task.poll(waiter));
		}
		Listening {
			listings: self.listings.clone(),
			key,
			failed,
		}
	}

	/// A fresh subscription of ours to `name` in `namespace`, and its info once accepted.
	fn subscribed(&self, namespace: &Path, name: &str) -> Option<(RequestId, Option<track::Info>)> {
		let state = self.state.lock();
		let aliases = state.aliases.read();
		// A paused one is on its way out with the entry it held.
		let paused = |id: &RequestId, track: &TrackState| {
			track
				.alias
				.is_some_and(|alias| aliases.map.get(&alias) == Some(&Alias::Paused(*id)))
		};
		state
			.subscribes
			.iter()
			.find(|(id, track)| track.broadcast == *namespace && track.name == name && !paused(id, track))
			.map(|(id, track)| (*id, track.producer.as_ref().map(|producer| producer.info().clone())))
	}

	/// The peer skipped a listed track: list it through a subscription of ours, opening
	/// one paused if we have none.
	fn skipped(&self, namespace: Path, name: Arc<str>) {
		let key = (namespace, name);
		let mut listings = self.listings.lock();
		if listings.entries.contains_key(&key) || listings.covering(&key.0).is_none() {
			return;
		}
		let slot = match self.announces.acquire() {
			Ok(slot) => slot,
			Err(err) => return listings.fail(&key.0, err),
		};
		if let Some((request_id, info)) = self.subscribed(&key.0, &key.1) {
			let entry = Entry {
				info,
				request_id: Some(request_id),
				handoff: None,
				_slot: slot,
			};
			return listings.insert(key, entry);
		}
		let handoff = kio::Shared::<Handoff>::default();
		let entry = Entry {
			info: None,
			request_id: None,
			handoff: Some(handoff.clone()),
			_slot: slot,
		};
		listings.insert(key.clone(), entry);
		drop(listings);
		let mut task = Skipped::Opening {
			this: self.clone(),
			key,
			handoff,
			open: OpenPaused::new(),
		};
		self.tasks.push(move |waiter: &kio::Waiter| task.poll(waiter));
	}

	/// Bind the alias a held subscription's PUBLISH or SUBSCRIBE_OK chose, paused, closing
	/// the session if it names another track.
	fn bind_alias(&self, request_id: RequestId, alias: u64) -> Result<(), Error> {
		let err = match self.register_alias(request_id, alias) {
			Ok(()) => {
				self.rebind_alias(request_id, Alias::Paused(request_id));
				return Ok(());
			}
			Err(err) => err,
		};
		if matches!(err, Error::Duplicate) {
			tracing::warn!(track_alias = %alias, "publisher reused a live track alias for another track");
			self.session.clone().close(
				super::super::error::to_session_code(&SessionError::from(&err)),
				err.to_string().as_ref(),
			);
		}
		Err(err)
	}

	/// What to do with a PUBLISH whose token, if it decides, granted it.
	fn offer(
		&self,
		stream: Stream<Transport, Version>,
		msg: ietf::Publish<'static>,
		token_grant: Option<crate::auth::RequestGrant>,
		flate: bool,
	) -> ServePublishState {
		let key: TrackKey = (msg.track_namespace.clone(), msg.track_name.as_ref().into());
		let info = accepted_info(&msg.properties, self.version);
		let offer = {
			let mut listings = self.listings.lock();
			// Draft-18 allows one subscription per track (section 5.1); later drafts allow
			// more, but the one we hold already lists the track and carries its data.
			let duplicate = match self.version {
				Version::Draft18 => request::DUPLICATE_SUBSCRIPTION,
				_ => request::to_code(&Error::Unroutable, request::Kind::Publish, self.version),
			};
			if listings.covering(&key.0).is_none() {
				match listings
					.requests
					.values()
					.any(|request| key.0.has_prefix(&request.prefix))
				{
					true => Offer::Refuse(
						request::to_code(&Error::Unroutable, request::Kind::Publish, self.version),
						"listing cancelled",
					),
					false => Offer::Unsolicited,
				}
			} else if listings.entries.contains_key(&key) {
				Offer::Refuse(duplicate, "already subscribed")
			} else if msg.forward && !matches!(self.version, Version::Draft18) {
				// From draft-22 the PUBLISH sets its own Forward State, and PUBLISH_OK has no
				// FORWARD to pause it: its objects would be refused unseen behind a healthy
				// listing.
				listings.fail(&key.0, Error::Unsupported);
				Offer::Refuse(
					request::to_code(&Error::Unsupported, request::Kind::Publish, self.version),
					"PUBLISH forwards for a paused listing",
				)
			} else if let Some(err) = refused(&msg.properties, self.version, flate) {
				// The peer sends nothing else for the track, so a listing left standing would
				// silently miss it, as with a refused paused SUBSCRIBE.
				let code = request::to_code(&err, request::Kind::Publish, self.version);
				listings.fail(&key.0, err);
				Offer::Refuse(code, "track properties not supported")
			} else {
				match self.announces.acquire() {
					Err(err) => {
						listings.fail(&key.0, err);
						Offer::Refuse(request::EXCESSIVE_LOAD, "too many listed tracks")
					}
					Ok(slot) => match self.subscribed(&key.0, &key.1) {
						// Our own SUBSCRIBE holds the track, so it is the entry.
						Some((request_id, _)) => {
							let entry = Entry {
								info: Some(info),
								request_id: Some(request_id),
								handoff: None,
								_slot: slot,
							};
							listings.insert(key.clone(), entry);
							Offer::Refuse(duplicate, "already subscribed")
						}
						None => {
							let handoff = kio::Shared::<Handoff>::default();
							let entry = Entry {
								info: Some(info),
								request_id: Some(msg.request_id),
								handoff: Some(handoff.clone()),
								_slot: slot,
							};
							listings.insert(key.clone(), entry);
							Offer::Hold(handoff)
						}
					},
				}
			}
		};
		let reject = match offer {
			Offer::Unsolicited => self.decline_publish(stream, msg),
			Offer::Refuse(code, reason) => {
				tracing::debug!(broadcast = %key.0, track = %key.1, reason, "refusing publish");
				let error = ietf::RequestError {
					error_code: code,
					reason_phrase: reason.into(),
					// The rule judges the refusal as the peer reads it.
					retry_interval: request::retry_interval(
						&request::from_code(code, request::Kind::Publish, self.version),
						code,
					),
				};
				request_stream::Reject::new(stream, error)
			}
			Offer::Hold(handoff) => {
				self.state.lock().subscribes.insert(
					msg.request_id,
					TrackState {
						..TrackState::pending(key.1.to_string(), key.0.clone(), kio::Producer::new(Fill::Done))
					},
				);
				return ServePublishState::Accepting {
					stream,
					msg,
					token_grant,
					key,
					handoff,
					answered: false,
				};
			}
		};
		ServePublishState::Rejecting {
			reject,
			_credit: None,
			_grant: token_grant,
		}
	}

	/// Decline a PUBLISH no listing asked for.
	///
	/// PUBLISH offers a single track, so honoring it means routing per
	/// (namespace, track). Our model routes per namespace: a source attaches at a
	/// path and serves every track under it, resolved on demand via SUBSCRIBE.
	/// Accepting a PUBLISH would mean inventing a namespace-level source out of a
	/// track-level offer, and that fiction then contradicts any real
	/// PUBLISH_NAMESPACE for the same path.
	///
	/// Declining the request rather than failing the session, since a peer using
	/// a feature we don't implement is not a protocol violation. The rejection is the
	/// whole exchange, but it still has to arrive: [`request_stream::Reject`] delivers it.
	pub(super) fn decline_publish(
		&self,
		mut stream: Stream<Transport, Version>,
		msg: ietf::Publish<'static>,
	) -> request_stream::Reject {
		if let Some(error) = super::super::namespace::refusal(&msg.track_namespace, self.version) {
			return request_stream::Reject::new(stream, error);
		}
		tracing::debug!(broadcast = %msg.track_namespace, track = %msg.track_name, "rejecting publish");

		// We decline the method itself rather than this particular track, which would be
		// UNINTERESTED.
		//
		// The alias the message carries is deliberately not recorded. Nothing will ever bind
		// it, and a rejected request has no lifetime of ours to hang the cleanup on, so the
		// entry would have to be swept asynchronously. Any data streams the publisher opened
		// before reading this are dropped by the unknown-alias path instead.
		let (kind, err) = (request::Kind::Publish, &Error::Unsupported);
		let buffered =
			request_stream::buffer_error(&mut stream.writer, kind, err, "PUBLISH is not supported", self.version);
		request_stream::Reject::buffered(stream, buffered)
	}

	/// Ask for the paused subscription a listing holds for this track, to resume it.
	pub(super) fn want(&self, key: &TrackKey) -> Option<kio::Shared<Handoff>> {
		let listings = self.listings.lock();
		let handoff = listings.entries.get(key)?.handoff.clone()?;
		handoff.lock().wanted = true;
		Some(handoff)
	}

	/// Ready with the subscription `handoff` hands over, or `None` once it ended instead.
	pub(super) fn poll_handoff(handoff: &kio::Shared<Handoff>, waiter: &kio::Waiter) -> Poll<Option<Upstream>> {
		let mut handoff = ready!(
			handoff.poll(waiter, |handoff| match handoff.gone || handoff.upstream.is_some() {
				true => Poll::Ready(()),
				false => Poll::Pending,
			})
		);
		Poll::Ready(handoff.upstream.take())
	}

	/// Give up wanting `handoff`, holding again what it already handed over.
	pub(super) fn unwant(&self, key: TrackKey, handoff: &kio::Shared<Handoff>) {
		let upstream = {
			let mut held = handoff.lock();
			held.wanted = false;
			held.upstream.take()
		};
		if let Some(upstream) = upstream {
			self.hold(key, upstream);
		}
	}

	/// The subscription with `request_id` took over the entry it resumes.
	pub(super) fn adopted(&self, key: &TrackKey, request_id: RequestId) {
		if let Some(entry) = self.listings.lock().held_by(key, request_id) {
			entry.handoff = None;
		}
	}

	/// Whether the subscription with `request_id` holds this track's listing entry, so
	/// it pauses rather than ends once nobody reads it.
	pub(super) fn is_entry(&self, key: &TrackKey, request_id: RequestId) -> bool {
		self.listings
			.lock()
			.held_by(key, request_id)
			.is_some_and(|entry| entry.handoff.is_none())
	}

	/// The subscription with `request_id` learned the info of the track it holds, which
	/// lists it if a skip made it the entry before its SUBSCRIBE_OK.
	pub(super) fn entry_info(&self, key: &TrackKey, request_id: RequestId, info: &track::Info) {
		let mut listings = self.listings.lock();
		let Some(entry) = listings.held_by(key, request_id) else {
			return;
		};
		if entry.info.is_some() {
			return;
		}
		entry.info = Some(info.clone());
		for lister in listings.listers(&key.0) {
			let _ = lister.add(key.1.clone(), info.clone());
		}
	}

	/// The subscription with `request_id` ended: so does the entry it held.
	pub(super) fn entry_ended(&self, key: &TrackKey, request_id: RequestId, end: Result<(), Error>) {
		self.listings.lock().ended(key, request_id, end);
	}

	/// Hold a paused subscription again as its track's entry, watched until a reader
	/// resumes it, or cancel it if the entry is gone.
	pub(super) fn hold(&self, key: TrackKey, upstream: Upstream) {
		let handoff = kio::Shared::<Handoff>::default();
		let held = match self.listings.lock().held_by(&key, upstream.request_id) {
			Some(entry) => {
				entry.handoff = Some(handoff.clone());
				true
			}
			None => false,
		};
		let mut task = Watch::new(self.clone(), key, upstream, handoff, held);
		self.tasks.push(move |waiter: &kio::Waiter| task.poll(waiter));
	}

	/// Pause a subscription nobody reads that is its track's entry, and hold it again. The
	/// held watch flushes the pause, so the entry going still ends it under send pressure.
	pub(super) fn pause(&self, key: TrackKey, mut upstream: Upstream) {
		self.park_subscribe(upstream.request_id);
		let pause = Forward {
			forward: false,
			..Default::default()
		};
		match self.buffer_forward(&mut upstream.stream.writer, &pause) {
			Ok(()) => {
				upstream.owed += 1;
				tracing::debug!(broadcast = %key.0, track = %key.1, "subscription paused");
				self.hold(key, upstream);
			}
			Err(err) => {
				tracing::debug!(%err, "failed to pause the subscription");
				self.remove_subscribe(upstream.request_id);
				self.entry_ended(&key, upstream.request_id, Err(err));
			}
		}
	}

	/// Reset a subscription's state to paused, keeping the alias it is bound to: objects
	/// still arriving for it are refused rather than buffered.
	fn park_subscribe(&self, request_id: RequestId) {
		let mut state = self.state.lock();
		let Some(held) = state.subscribes.get_mut(&request_id) else {
			return;
		};
		if let Ok(mut fill) = held.fill.write() {
			fill.cancel();
		}
		let parked = TrackState {
			alias: held.alias,
			tail: std::mem::take(&mut held.tail),
			..TrackState::pending(
				std::mem::take(&mut held.name),
				held.broadcast.clone(),
				kio::Producer::new(Fill::Done),
			)
		};
		let fetch_id = std::mem::replace(held, parked).fetch_id;
		if let Some(fetch_id) = fetch_id {
			state.fetches.remove(&fetch_id);
		}
		if let Some(alias) = state.subscribes.get(&request_id).and_then(|track| track.alias) {
			rebind_track_alias(&state.aliases, alias, request_id, Alias::Paused(request_id));
		}
	}

	/// Buffer the REQUEST_UPDATE setting a held subscription's Forward State, for the
	/// caller to flush.
	pub(super) fn buffer_forward(
		&self,
		writer: &mut Writer<crate::transport::boxed::SendStream, Version>,
		forward: &Forward,
	) -> Result<(), Error> {
		let request_id = self.control.next_request_id();
		writer.buffer_message(
			ietf::SubscribeUpdate::ID,
			&ietf::SubscribeUpdate {
				max_age: forward.max_age,
				request_id,
				subscriber_priority: forward.priority.map(super::super::priority::to_wire),
				forward: Some(forward.forward),
				filter: forward.filter,
				authorization_token: None,
				new_group: forward.new_group,
				fill: forward.fill,
			},
		)
	}

	/// Check the answer to a REQUEST_UPDATE a held subscription still owes. It carries
	/// nothing to act on: a refused one is followed by the PUBLISH_DONE that ends it.
	fn owed_answer(&self, id: u64, body: &ietf::Body) -> Result<(), Error> {
		match id {
			ietf::RequestOk::ID => body.message::<ietf::RequestOk>(self.version).map(|_| ())?,
			_ => body.message::<ietf::RequestError>(self.version).map(|_| ())?,
		}
		Ok(())
	}

	/// Read the answer to a SUBSCRIBE, or to the REQUEST_UPDATE resuming `held`.
	pub(super) fn poll_read_answer(
		&self,
		waiter: &kio::Waiter,
		stream: &mut Stream<Transport, Version>,
		messages: &mut Messages,
		held: Option<&mut Resumed>,
	) -> Poll<Result<Answer, Error>> {
		let Some(held) = held else {
			let Framed(id, body) = ready!(messages.poll_message(&mut stream.reader, waiter))?;
			return Poll::Ready(self.subscribe_response(id, &body));
		};
		loop {
			let Framed(id, body) = ready!(messages.poll_message(&mut stream.reader, waiter))?;
			match id {
				// The answer to a pause that went out before this resume.
				ietf::RequestOk::ID | ietf::RequestError::ID if held.owed > 0 => {
					self.owed_answer(id, &body)?;
					held.owed -= 1;
				}
				ietf::RequestOk::ID => {
					let ok = body.message::<ietf::RequestOk>(self.version)?;
					tracing::debug!(message = ?ok, "subscription resumed");
					return Poll::Ready(Ok(Answer::Accepted(Accepted {
						alias: held.alias,
						properties: held.properties.clone(),
						largest: ok.largest,
					})));
				}
				ietf::RequestError::ID => {
					let refused = body.message::<ietf::RequestError>(self.version)?;
					tracing::debug!(message = ?refused, "resume refused");
					return Poll::Ready(Ok(self.refused(&refused)));
				}
				ietf::PublishDone::ID => {
					let done = body.message::<ietf::PublishDone>(self.version)?;
					let end = listing_end(&done, self.version);
					held.ended = Some(end.clone());
					return Poll::Ready(Err(end.err().unwrap_or(Error::NotFound)));
				}
				_ => return Poll::Ready(Err(Error::UnexpectedMessage)),
			}
		}
	}

	/// The answer to a SUBSCRIBE: its SUBSCRIBE_OK, or the REQUEST_ERROR refusing it.
	fn subscribe_response(&self, id: u64, body: &ietf::Body) -> Result<Answer, Error> {
		let mut data = body.decoder(self.version);
		match id {
			ietf::SubscribeOk::ID => {
				let msg = ietf::SubscribeOk::decode_msg(&mut data, self.version)?;
				tracing::debug!(message = ?msg, "received subscribe ok");
				Ok(Answer::Accepted(Accepted {
					alias: msg.track_alias,
					properties: msg.properties,
					largest: msg.largest,
				}))
			}
			ietf::RequestError::ID => {
				let msg = ietf::RequestError::decode_msg(&mut data, self.version)?;
				tracing::warn!(message = ?msg, "request error");
				Ok(self.refused(&msg))
			}
			_ => Err(Error::UnexpectedMessage),
		}
	}

	/// The REQUEST_ERROR refusing a SUBSCRIBE or its resume, as an [`Answer`].
	fn refused(&self, msg: &ietf::RequestError<'_>) -> Answer {
		let err = request::from_code(msg.error_code, request::Kind::Subscribe, self.version);
		Answer::Refused(err, Duration::from_millis(msg.retry_interval))
	}
}

/// One SUBSCRIBE_TRACKS, sent once the overlapping requests leave the wire, then read until
/// it is cancelled and the peer has closed it, or it fails.
struct SubscribeTracks {
	this: Subscriber,
	key: u64,
	prefix: Path,
	/// The overlapping requests that must leave the wire first.
	blockers: Vec<kio::Consumer<()>>,
	live: kio::Consumer<()>,
	/// Dropped once it is off the wire, so an overlapping one may go out.
	gone: Option<kio::Producer<()>>,
	state: SubscribeTracksState,
}

enum SubscribeTracksState {
	/// Waiting for the overlapping requests to leave the wire.
	Blocked,
	/// Waiting for a stream to send it on.
	Opening(crate::transport::open::Open),
	/// Reading its answers until it is cancelled.
	Listing {
		stream: Stream<Transport, Version>,
		messages: Messages,
		/// Its REQUEST_OK arrived.
		established: bool,
		/// A peer streaming skips must not hold the session's other tasks.
		budget: kio::coop::Budget,
	},
	/// Cancelled: reading until the peer closes its own half. A draft-18 FIN leaves the
	/// writer to reset only once that is done.
	Draining {
		reader: crate::coding::Reader<crate::transport::boxed::RecvStream, Version>,
		_writer: Option<Writer<crate::transport::boxed::SendStream, Version>>,
		messages: Messages,
		budget: kio::coop::Budget,
	},
	Done,
}

impl SubscribeTracks {
	fn new(this: Subscriber, opening: Opening) -> Self {
		let Opening {
			key,
			prefix,
			blockers,
			live,
			gone,
		} = opening;
		Self {
			this,
			key,
			prefix,
			blockers,
			live,
			gone: Some(gone),
			state: SubscribeTracksState::Blocked,
		}
	}

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		let result = ready!(self.poll_request(waiter));
		self.state = SubscribeTracksState::Done;
		let mut listings = self.this.listings.lock();
		if let Err(err) = result
			&& listings
				.requests
				.get(&self.key)
				.is_some_and(|request| request.live.is_some())
		{
			listings.fail(&self.prefix, err);
		}
		listings.requests.remove(&self.key);
		listings.release();
		drop(listings);
		// Off the wire: an overlapping request may go out.
		self.gone = None;
		Poll::Ready(())
	}

	fn poll_request(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		let this = &self.this;
		let prefix = &self.prefix;
		loop {
			match &mut self.state {
				SubscribeTracksState::Blocked => {
					if self.live.poll_closed(waiter).is_ready() {
						return Poll::Ready(Ok(()));
					}
					// The peer refuses a prefix overlapping one it still holds, so wait for each
					// overlapping request to leave the wire.
					for blocker in &self.blockers {
						if blocker.poll_closed(waiter).is_pending() {
							return Poll::Pending;
						}
					}
					self.state = SubscribeTracksState::Opening(Default::default());
				}
				SubscribeTracksState::Opening(open) => {
					if self.live.poll_closed(waiter).is_ready() {
						return Poll::Ready(Ok(()));
					}
					let stream = ready!(Stream::poll_open(&this.session, open, this.version, waiter))?;
					let mut stream = stream.with_stats(this.origin.stats());
					let request_id = this.control.next_request_id();
					stream.writer.buffer_message(
						ietf::SubscribeTracks::ID,
						&ietf::SubscribeTracks {
							request_id,
							prefix: prefix.clone(),
							forward: false,
							group_order: None,
							properties_wanted: true,
							authorization_token: None,
						},
					)?;
					tracing::debug!(prefix = %crate::path::Rooted(this.origin.root(), prefix), id = %request_id, "subscribe_tracks started");
					self.state = SubscribeTracksState::Listing {
						stream,
						messages: Messages::new(this.session.clone(), this.control.client()),
						established: false,
						budget: kio::coop::Budget::new(32),
					};
				}
				SubscribeTracksState::Listing {
					stream,
					messages,
					established,
					budget,
				} => {
					loop {
						ready!(budget.poll_yield(waiter));
						if this.session.poll_closed(waiter).is_ready() {
							return Poll::Ready(Ok(()));
						}
						if self.live.poll_closed(waiter).is_ready() {
							break;
						}
						ready!(stream.writer.poll_flush(waiter))?;
						let Framed(id, body) = match ready!(messages.poll_next(&mut stream.reader, waiter))? {
							// The peer ended the listing, which only a cancel of ours should do.
							None if *established => return Poll::Ready(Err(Error::Closed)),
							None => return Poll::Ready(Err(messages.failed(Error::ProtocolViolation))),
							Some(framed) => framed,
						};
						match id {
							ietf::RequestOk::ID if !*established => {
								body.message::<ietf::RequestOk>(this.version)
									.map_err(|err| messages.failed(err.into()))?;
								*established = true;
							}
							ietf::RequestError::ID if !*established => {
								let refused = body
									.message::<ietf::RequestError>(this.version)
									.map_err(|err| messages.failed(err.into()))?;
								tracing::debug!(prefix = %prefix, code = refused.error_code, reason = %refused.reason_phrase, "subscribe_tracks refused");
								return Poll::Ready(Err(request::from_code(
									refused.error_code,
									request::Kind::SubscribeNamespace,
									this.version,
								)));
							}
							ietf::PublishSkipped::ID if *established => {
								let skipped = body
									.message::<ietf::PublishSkipped>(this.version)
									.map_err(|err| messages.failed(err.into()))?;
								let namespace = prefix
									.join(&skipped.suffix)
									.map_err(|err| messages.failed(err.into()))?;
								this.skipped(namespace, skipped.track_name.as_ref().into());
							}
							_ => return Poll::Ready(Err(messages.failed(Error::UnexpectedMessage))),
						}
					}

					// Cancelled: close our half (FIN on draft-18, RESET_STREAM from draft-22, where
					// a FIN is not a cancel), then read until the peer closes its own. The
					// PUBLISHes it made stay ours.
					tracing::debug!(prefix = %crate::path::Rooted(this.origin.root(), prefix), "subscribe_tracks cancelled");
					let SubscribeTracksState::Listing {
						stream: Stream { writer, mut reader },
						messages,
						budget,
						..
					} = std::mem::replace(&mut self.state, SubscribeTracksState::Done)
					else {
						unreachable!()
					};
					// The request never reached the peer whole, so it holds nothing to wait on.
					if !writer.is_flushed() {
						reader.abort(&Error::Cancel);
						writer.abort(&Error::Cancel);
						return Poll::Ready(Ok(()));
					}
					let writer = match request_stream::fin_cancels(this.version) {
						true => {
							let mut writer = writer;
							let _ = writer.finish();
							Some(writer)
						}
						false => {
							writer.abort(&Error::Cancel);
							None
						}
					};
					self.state = SubscribeTracksState::Draining {
						reader,
						_writer: writer,
						messages,
						budget,
					};
				}
				SubscribeTracksState::Draining {
					reader,
					messages,
					budget,
					..
				} => {
					if this.session.poll_closed(waiter).is_ready() {
						return Poll::Ready(Ok(()));
					}
					loop {
						ready!(budget.poll_yield(waiter));
						match ready!(messages.poll_next(reader, waiter)) {
							Ok(Some(_)) => continue,
							Ok(None) | Err(_) => return Poll::Ready(Ok(())),
						}
					}
				}
				SubscribeTracksState::Done => unreachable!("polled after it ended"),
			}
		}
	}
}

/// A skipped track's paused SUBSCRIBE: opened, then watched as the track's entry.
#[expect(
	clippy::large_enum_variant,
	reason = "a state machine's enum is its storage: one transient instance per stream, so the big variant is the working state, not padding held in bulk"
)]
enum Skipped {
	Opening {
		this: Subscriber,
		key: TrackKey,
		handoff: kio::Shared<Handoff>,
		open: OpenPaused,
	},
	Watching(Watch),
	Done,
}

impl Skipped {
	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		loop {
			match self {
				Self::Opening {
					this,
					key,
					handoff,
					open,
				} => {
					let result = ready!(open.poll(waiter, this, key, handoff));
					let Self::Opening { this, key, handoff, .. } = std::mem::replace(self, Self::Done) else {
						unreachable!()
					};
					match result {
						Ok(Some(upstream)) => *self = Self::Watching(Watch::new(this, key, upstream, handoff, true)),
						Ok(None) => return Poll::Ready(()),
						Err(err) => {
							tracing::debug!(broadcast = %key.0, track = %key.1, %err, "paused subscribe failed");
							let mut listings = this.listings.lock();
							if listings.watched_by(&key, &handoff).is_some() {
								listings.remove(&key, false);
								listings.fail(&key.0, err);
							}
							return Poll::Ready(());
						}
					}
				}
				Self::Watching(watch) => return watch.poll(waiter),
				Self::Done => return Poll::Ready(()),
			}
		}
	}
}

/// Opens a paused SUBSCRIBE for a skipped track and lists it once its SUBSCRIBE_OK
/// arrives. `None` once its entry was let go.
struct OpenPaused {
	state: OpenPausedState,
}

#[expect(
	clippy::large_enum_variant,
	reason = "a state machine's enum is its storage: one transient instance per stream, so the big variant is the working state, not padding held in bulk"
)]
enum OpenPausedState {
	/// Waiting for a stream to send it on.
	Opening(crate::transport::open::Open),
	/// Flushed while waiting for the answer, so the entry going still ends a request the
	/// peer's send window holds back.
	Answering {
		stream: Stream<Transport, Version>,
		messages: Messages,
		request_id: RequestId,
	},
	/// Accepted, waiting for the peer's SETUP to judge its properties.
	Accepted {
		stream: Stream<Transport, Version>,
		messages: Messages,
		request_id: RequestId,
		accepted: Accepted,
	},
	/// Abandoned: cancelling it before resolving with `result`.
	Cancelling {
		cancel: request_stream::Cancel,
		result: Option<Result<Option<Upstream>, Error>>,
	},
	Done,
}

impl OpenPausedState {
	/// Remove and cancel the subscription, then resolve with `result`.
	fn cancel(
		this: &Subscriber,
		request_id: RequestId,
		stream: Stream<Transport, Version>,
		result: Result<Option<Upstream>, Error>,
	) -> Self {
		this.remove_subscribe(request_id);
		Self::Cancelling {
			cancel: request_stream::Cancel::new(stream),
			result: Some(result),
		}
	}
}

impl OpenPaused {
	fn new() -> Self {
		Self {
			state: OpenPausedState::Opening(Default::default()),
		}
	}

	fn poll(
		&mut self,
		waiter: &kio::Waiter,
		this: &Subscriber,
		key: &TrackKey,
		handoff: &kio::Shared<Handoff>,
	) -> Poll<Result<Option<Upstream>, Error>> {
		let gone = |waiter: &kio::Waiter| {
			handoff
				.poll(waiter, |handoff| match handoff.gone {
					true => Poll::Ready(()),
					false => Poll::Pending,
				})
				.is_ready()
		};
		loop {
			match &mut self.state {
				OpenPausedState::Opening(open) => {
					if gone(waiter) {
						return Poll::Ready(Ok(None));
					}
					let stream = ready!(Stream::poll_open(&this.session, open, this.version, waiter))?;
					let mut stream = stream.with_stats(this.origin.stats());
					let request_id = this.control.next_request_id();
					this.state.lock().subscribes.insert(
						request_id,
						TrackState {
							..TrackState::pending(key.1.to_string(), key.0.clone(), kio::Producer::new(Fill::Done))
						},
					);
					let messages = Messages::new(this.session.clone(), this.control.client());
					let buffered = stream.writer.buffer_message(
						ietf::Subscribe::ID,
						&ietf::Subscribe {
							max_age: None,
							new_group: None,
							request_id,
							track_namespace: key.0.clone(),
							track_name: key.1.as_ref().into(),
							subscriber_priority: super::super::priority::DEFAULT_WIRE,
							// No update can change it, so it takes the order a reader gets by default.
							group_order: track::Order::default().into(),
							filter: Filter::NextObject,
							fill: None,
							properties_wanted: true,
							forward: false,
							rendezvous: false,
							range_filters: false,
							authorization_token: None,
						},
					);
					if let Err(err) = buffered {
						this.remove_subscribe(request_id);
						return Poll::Ready(Err(err));
					}
					self.state = OpenPausedState::Answering {
						stream,
						messages,
						request_id,
					};
				}
				OpenPausedState::Answering {
					stream,
					messages,
					request_id,
				} => {
					let request_id = *request_id;
					if gone(waiter) {
						let OpenPausedState::Answering { stream, .. } =
							std::mem::replace(&mut self.state, OpenPausedState::Done)
						else {
							unreachable!()
						};
						self.state = OpenPausedState::cancel(this, request_id, stream, Ok(None));
						continue;
					}
					let answered = match stream.writer.poll_flush(waiter) {
						Poll::Ready(Ok(())) => this.poll_read_answer(waiter, stream, messages, None),
						Poll::Ready(Err(err)) => Poll::Ready(Err(err)),
						Poll::Pending => Poll::Pending,
					};
					let accepted = match ready!(answered) {
						Ok(Answer::Accepted(accepted)) => accepted,
						Ok(Answer::Refused(err, _)) | Err(err) => {
							this.remove_subscribe(request_id);
							return Poll::Ready(Err(err));
						}
					};
					let OpenPausedState::Answering { stream, messages, .. } =
						std::mem::replace(&mut self.state, OpenPausedState::Done)
					else {
						unreachable!()
					};
					self.state = OpenPausedState::Accepted {
						stream,
						messages,
						request_id,
						accepted,
					};
				}
				OpenPausedState::Accepted { .. } => {
					let flate = ready!(this.peer_setup.poll_get(waiter)).flate;
					let OpenPausedState::Accepted {
						stream,
						messages,
						request_id,
						accepted,
					} = std::mem::replace(&mut self.state, OpenPausedState::Done)
					else {
						unreachable!()
					};
					if let Some(err) = refused(&accepted.properties, this.version, flate) {
						self.state = OpenPausedState::cancel(this, request_id, stream, Err(err));
						continue;
					}
					if let Err(err) = this.bind_alias(request_id, accepted.alias) {
						self.state = OpenPausedState::cancel(this, request_id, stream, Err(err));
						continue;
					}
					let info = accepted_info(&accepted.properties, this.version);
					let listed = {
						let mut listings = this.listings.lock();
						match listings.watched_by(key, handoff) {
							Some(entry) => {
								entry.request_id = Some(request_id);
								entry.info = Some(info.clone());
								for lister in listings.listers(&key.0) {
									let _ = lister.add(key.1.clone(), info.clone());
								}
								true
							}
							None => false,
						}
					};
					if !listed {
						self.state = OpenPausedState::cancel(this, request_id, stream, Ok(None));
						continue;
					}
					return Poll::Ready(Ok(Some(Upstream {
						token_grant: None,
						stream,
						messages,
						request_id,
						alias: accepted.alias,
						properties: accepted.properties,
						owed: 0,
						copy: None,
					})));
				}
				OpenPausedState::Cancelling { cancel, result } => {
					ready!(cancel.poll(waiter));
					let result = result.take().expect("resolved once");
					self.state = OpenPausedState::Done;
					return Poll::Ready(result);
				}
				OpenPausedState::Done => unreachable!("polled after it resolved"),
			}
		}
	}
}

/// Watches a paused subscription until it ends, its entry is let go, or a subscription
/// takes it to resume.
struct Watch {
	this: Subscriber,
	key: TrackKey,
	/// Taken once it is handed over, cancelled, or ended.
	upstream: Option<Upstream>,
	handoff: kio::Shared<Handoff>,
	state: WatchState,
}

#[expect(
	clippy::large_enum_variant,
	reason = "a state machine's enum is its storage: one transient instance per stream, so the big variant is the working state, not padding held in bulk"
)]
enum WatchState {
	/// Its entry was already gone when it was held, so it is cancelled at once.
	Unheld,
	Watching,
	/// Cancelling it. The rest of the subscription, its grant and lingering copy included,
	/// is released only after.
	Cancelling {
		cancel: request_stream::Cancel,
		_rest: (Option<crate::auth::RequestGrant>, Messages, Option<track::Producer>),
	},
	Done,
}

impl Watch {
	/// Watch `upstream` as the entry `handoff` belongs to, or cancel it unless `held`.
	fn new(this: Subscriber, key: TrackKey, upstream: Upstream, handoff: kio::Shared<Handoff>, held: bool) -> Self {
		Self {
			this,
			key,
			upstream: Some(upstream),
			handoff,
			state: match held {
				true => WatchState::Watching,
				false => WatchState::Unheld,
			},
		}
	}

	/// Remove and cancel the subscription.
	fn cancel(&mut self) {
		let Upstream {
			token_grant,
			stream,
			messages,
			request_id,
			copy,
			..
		} = self.upstream.take().expect("cancelled once");
		self.this.remove_subscribe(request_id);
		self.state = WatchState::Cancelling {
			cancel: request_stream::Cancel::new(stream),
			_rest: (token_grant, messages, copy),
		};
	}

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		enum Step {
			Closed,
			Gone,
			Wanted,
			Message(Result<Option<Framed>, Error>),
		}
		let end = loop {
			match &mut self.state {
				WatchState::Unheld => {
					self.cancel();
					continue;
				}
				WatchState::Cancelling { cancel, .. } => {
					ready!(cancel.poll(waiter));
					self.state = WatchState::Done;
					return Poll::Ready(());
				}
				WatchState::Done => return Poll::Ready(()),
				WatchState::Watching => {}
			}
			let upstream = self.upstream.as_mut().expect("watched");
			let step = 'step: {
				if self.this.session.poll_closed(waiter).is_ready() {
					break 'step Step::Closed;
				}
				// A held subscription is handed over only once flushed, so resumes and pauses
				// can't pile up behind a peer withholding credit.
				let flushed = match upstream.stream.writer.poll_flush(waiter) {
					Poll::Ready(Err(err)) => break 'step Step::Message(Err(err)),
					Poll::Ready(Ok(())) => true,
					Poll::Pending => false,
				};
				if let Poll::Ready(handoff) =
					self.handoff
						.poll(waiter, |handoff| match handoff.gone || (handoff.wanted && flushed) {
							true => Poll::Ready(()),
							false => Poll::Pending,
						}) {
					break 'step match handoff.gone {
						true => Step::Gone,
						false => Step::Wanted,
					};
				}
				if let Some(grant) = &upstream.token_grant
					&& let Poll::Ready(refusal) = grant.poll_ended(waiter)
				{
					break 'step Step::Message(Err(refusal.error));
				}
				Step::Message(ready!(upstream.messages.poll_next(&mut upstream.stream.reader, waiter)))
			};
			match step {
				Step::Closed => {
					self.upstream = None;
					self.state = WatchState::Done;
					return Poll::Ready(());
				}
				Step::Wanted => {
					let mut held = self.handoff.lock();
					match (held.gone, held.wanted) {
						(true, _) => {}
						// The subscription gave up before it was handed over.
						(false, false) => continue,
						(false, true) => {
							held.upstream = self.upstream.take();
							drop(held);
							self.state = WatchState::Done;
							return Poll::Ready(());
						}
					}
					drop(held);
					self.cancel();
				}
				Step::Gone => self.cancel(),
				Step::Message(Ok(Some(Framed(id, body)))) => match id {
					// The answer to a pause, or to a resume that paused again before it landed.
					ietf::RequestOk::ID | ietf::RequestError::ID if upstream.owed > 0 => {
						if let Err(err) = self.this.owed_answer(id, &body) {
							break Err(upstream.messages.failed(err));
						}
						upstream.owed -= 1;
					}
					ietf::PublishDone::ID => {
						break body
							.message::<ietf::PublishDone>(self.this.version)
							.map_err(|err| upstream.messages.failed(err.into()))
							.and_then(|done| listing_end(&done, self.this.version));
					}
					_ => break Err(upstream.messages.failed(Error::UnexpectedMessage)),
				},
				// PUBLISH_DONE must come before the FIN (draft-22 section 6.4.2.2).
				Step::Message(Ok(None)) => break Err(Error::ProtocolViolation),
				Step::Message(Err(err)) => break Err(err),
			}
		};

		self.state = WatchState::Done;
		let key = &self.key;
		let mut upstream = self.upstream.take().expect("watched");
		tracing::debug!(broadcast = %key.0, track = %key.1, ?end, "held subscription ended");
		self.this.remove_subscribe(upstream.request_id);
		// A pause still held back would go out truncated behind a FIN.
		match upstream.stream.writer.is_flushed() {
			true => {
				let _ = upstream.stream.writer.finish();
			}
			false => upstream.stream.writer.abort(&Error::Cancel),
		}
		// A failed upstream fails the copy, as it would while forwarding. TRACK_ENDED leaves it
		// idle: the end is past groups it never saw, so a returning reader's SUBSCRIBE learns
		// where, as for a copy whose subscription was cancelled.
		if let (Err(err), Some(copy)) = (&end, upstream.copy.take()) {
			let _ = copy.abort(err.clone());
		}
		let mut listings = self.this.listings.lock();
		if listings.watched_by(key, &self.handoff).is_some() {
			match end {
				Ok(()) => listings.remove(key, true),
				Err(err) => {
					listings.remove(key, false);
					listings.fail(&key.0, err);
				}
			}
		}
		Poll::Ready(())
	}
}

/// Serve a PUBLISH: hold one answering our SUBSCRIBE_TRACKS paused as its track's entry,
/// refuse a redundant one, and decline one no listing asked for.
pub(super) struct ServePublish {
	this: Subscriber,
	state: ServePublishState,
}

#[expect(
	clippy::large_enum_variant,
	reason = "a state machine's enum is its storage: one transient instance per stream, so the big variant is the working state, not padding held in bulk"
)]
enum ServePublishState {
	/// Waiting for the acceptor to judge the PUBLISH's token.
	Verifying {
		stream: Stream<Transport, Version>,
		msg: ietf::Publish<'static>,
		verdict: crate::auth::RequestVerdict,
		/// What the acceptor holds while it decides: its credit lasts as long as this.
		credit: kio::Producer<()>,
	},
	/// The requester withdrew before the verdict: waiting for the acceptor to let go.
	Withdrawn {
		credit: kio::Producer<()>,
		_stream: Stream<Transport, Version>,
	},
	/// Waiting for the peer's SETUP to judge the track's properties.
	Setup {
		stream: Stream<Transport, Version>,
		msg: ietf::Publish<'static>,
		token_grant: Option<crate::auth::RequestGrant>,
	},
	/// Refusing or declining it, holding what the request held until it is delivered.
	Rejecting {
		reject: request_stream::Reject,
		_credit: Option<kio::Producer<()>>,
		_grant: Option<crate::auth::RequestGrant>,
	},
	/// Answering PUBLISH_OK, under the token's grant.
	Accepting {
		stream: Stream<Transport, Version>,
		msg: ietf::Publish<'static>,
		token_grant: Option<crate::auth::RequestGrant>,
		key: TrackKey,
		handoff: kio::Shared<Handoff>,
		/// The alias is bound and PUBLISH_OK buffered.
		answered: bool,
	},
	/// Held as its track's entry.
	Watching(Watch),
	Done,
}

impl ServePublish {
	/// Start serving `msg`, the PUBLISH that opened `stream`.
	pub(super) fn new(this: Subscriber, stream: Stream<Transport, Version>, msg: ietf::Publish<'static>) -> Self {
		let state = match msg
			.authorization_token
			.clone()
			.filter(|_| this.auth.token_decides(crate::auth::Direction::Subscribe))
		{
			Some(token) => {
				let credit = kio::Producer::<()>::default();
				let hold: crate::auth::Hold = std::sync::Arc::new(credit.consume());
				let verdict = this.auth.verify_request(token.0, Some(hold));
				ServePublishState::Verifying {
					stream,
					msg,
					verdict,
					credit,
				}
			}
			None => ServePublishState::Setup {
				stream,
				msg,
				token_grant: None,
			},
		};
		Self { this, state }
	}

	/// Ready once the PUBLISH is refused, declined, or its held subscription ends.
	pub(super) fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		let this = &self.this;
		loop {
			match &mut self.state {
				ServePublishState::Verifying { stream, verdict, .. } => {
					let withdrawn = this.session.poll_closed(waiter).is_ready()
						|| request_stream::poll_withdrawn(stream, request_stream::fin_cancels(this.version), waiter)
							.is_ready();
					if withdrawn {
						let ServePublishState::Verifying { stream, credit, .. } =
							std::mem::replace(&mut self.state, ServePublishState::Done)
						else {
							unreachable!()
						};
						self.state = ServePublishState::Withdrawn {
							credit,
							_stream: stream,
						};
						continue;
					}
					let verified = ready!(verdict.poll_grant(waiter));
					let ServePublishState::Verifying {
						mut stream,
						msg,
						verdict,
						credit,
					} = std::mem::replace(&mut self.state, ServePublishState::Done)
					else {
						unreachable!()
					};
					let granted = crate::auth::RequestGrant::new(
						&this.runtime,
						verdict,
						verified,
						msg.track_namespace.clone(),
						crate::auth::RequestKind::Publish,
						None,
					);
					self.state = match granted {
						Ok(grant) => ServePublishState::Setup {
							stream,
							msg,
							token_grant: Some(grant),
						},
						Err(refusal) => {
							let (kind, err) = (request::Kind::Publish, &refusal.error);
							let buffered = request_stream::buffer_error(
								&mut stream.writer,
								kind,
								err,
								&refusal.reason,
								this.version,
							);
							// Dropping an unfinished writer resets it, which could discard the refusal.
							ServePublishState::Rejecting {
								reject: request_stream::Reject::buffered(stream, buffered),
								_credit: Some(credit),
								_grant: None,
							}
						}
					};
				}
				ServePublishState::Withdrawn { credit, .. } => {
					if this.session.poll_closed(waiter).is_ready()
						|| credit.poll_wanted_changed(waiter, true).is_ready()
					{
						self.state = ServePublishState::Done;
						return Poll::Ready(Ok(()));
					}
					return Poll::Pending;
				}
				ServePublishState::Setup { .. } => {
					let flate = ready!(this.peer_setup.poll_get(waiter)).flate;
					let ServePublishState::Setup {
						stream,
						msg,
						token_grant,
					} = std::mem::replace(&mut self.state, ServePublishState::Done)
					else {
						unreachable!()
					};
					self.state = this.offer(stream, msg, token_grant, flate);
				}
				ServePublishState::Rejecting { reject, .. } => {
					let result = ready!(reject.poll(waiter));
					self.state = ServePublishState::Done;
					return Poll::Ready(result);
				}
				ServePublishState::Accepting {
					stream,
					msg,
					token_grant,
					answered,
					..
				} => {
					// The grant ending at any point ends the request there, and is checked first,
					// so a request never acts on authority that already ended.
					let ok = match token_grant.as_ref().map(|grant| grant.poll_ended(waiter)) {
						Some(Poll::Ready(refusal)) => Err(refusal.error),
						_ => {
							let mut answer = || {
								if !std::mem::replace(answered, true) {
									this.bind_alias(msg.request_id, msg.track_alias)?;
									// PUBLISH_OK is a REQUEST_OK on draft-22. FORWARD=0 holds it paused on
									// draft-18; on draft-22 the PUBLISH our FORWARD=0 asked for already is.
									let id = match this.version {
										Version::Draft18 => ietf::PublishOk::ID,
										_ => ietf::RequestOk::ID,
									};
									stream.writer.buffer_message(
										id,
										&ietf::PublishOk {
											forward: false,
											subscriber_priority: None,
											group_order: None,
											filter: None,
											new_group: None,
										},
									)?;
								}
								Ok(())
							};
							match answer() {
								Ok(()) => ready!(stream.writer.poll_flush(waiter)),
								Err(err) => Err(err),
							}
						}
					};
					let ServePublishState::Accepting {
						stream,
						msg,
						token_grant,
						key,
						handoff,
						..
					} = std::mem::replace(&mut self.state, ServePublishState::Done)
					else {
						unreachable!()
					};
					let request_id = msg.request_id;
					if let Err(err) = ok {
						tracing::debug!(broadcast = %key.0, track = %key.1, %err, "failed to hold publish");
						this.remove_subscribe(request_id);
						let mut listings = this.listings.lock();
						if listings.watched_by(&key, &handoff).is_some() {
							listings.remove(&key, false);
							listings.fail(&key.0, err);
						}
						return Poll::Ready(Ok(()));
					}
					tracing::debug!(broadcast = %crate::path::Rooted(this.origin.root(), &key.0), track = %key.1, id = %request_id, "holding publish");
					let upstream = Upstream {
						token_grant,
						stream,
						messages: Messages::new(this.session.clone(), this.control.client()),
						request_id,
						alias: msg.track_alias,
						properties: msg.properties,
						owed: 0,
						copy: None,
					};
					self.state = ServePublishState::Watching(Watch::new(this.clone(), key, upstream, handoff, true));
				}
				ServePublishState::Watching(watch) => {
					ready!(watch.poll(waiter));
					self.state = ServePublishState::Done;
					return Poll::Ready(Ok(()));
				}
				ServePublishState::Done => unreachable!("polled after it resolved"),
			}
		}
	}
}

/// A REQUEST_UPDATE setting a held subscription's Forward State.
#[derive(Default)]
pub(super) struct Forward {
	pub(super) forward: bool,
	/// The Location Filter it resumes with.
	pub(super) filter: Option<Filter>,
	/// FILL_PARAMETERS (draft-22), the backfill it resumes with.
	pub(super) fill: Option<ietf::Fill>,
	/// The aggregate priority, in the model's higher-first terms.
	pub(super) priority: Option<i8>,
	pub(super) max_age: Option<u64>,
	pub(super) new_group: Option<u64>,
}

/// What a subscription resuming a held one keeps of it.
pub(super) struct Resumed {
	pub(super) alias: u64,
	pub(super) properties: ietf::Properties,
	/// REQUEST_UPDATE answers still owed.
	pub(super) owed: u64,
	/// How a PUBLISH_DONE ended it before the resume was answered.
	pub(super) ended: Option<Result<(), Error>>,
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::model::ProduceTest as _;
	use crate::transport::memory;

	const TIMEOUT: Duration = Duration::from_secs(1);

	type PeerStream = Stream<Transport, Version>;

	/// A subscriber listing `broadcast` over a raw peer that plays the publisher.
	struct Harness {
		subscriber: Subscriber,
		peer: memory::Session,
		tracks: crate::broadcast::Tracks,
		listening: Listening,
		version: Version,
		_broadcast: crate::broadcast::Producer,
		_driver: kio::clock::sim::JoinHandle<()>,
	}

	async fn settle() {
		kio::clock::sleep(Duration::from_millis(1)).await;
	}

	fn harness(version: Version) -> Harness {
		harness_with(version, memory::Config::default())
	}

	fn harness_with(version: Version, config: memory::Config) -> Harness {
		let (session, peer) = memory::pair(config);
		let (tasks, mut task_set) = TaskSet::new();
		let driver = kio::clock::sim::spawn(async move { kio::wait(|waiter| task_set.poll(waiter)).await });
		let subscriber = Subscriber::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			crate::origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce(),
			Control::new(false),
			None,
			peer::PeerSetup::declared(peer::Peer::default()),
			crate::Hop::new(1).unwrap(),
			None,
			version,
			tasks,
			Default::default(),
		);
		let broadcast = crate::broadcast::Info::default().produce();
		let lister = Arc::new(broadcast.requests().lister().unwrap());
		let tracks = broadcast.consume().tracks().unwrap();
		let listening = subscriber.listen(crate::Path::from_static("bcast"), lister);
		Harness {
			subscriber,
			peer,
			tracks,
			listening,
			version,
			_broadcast: broadcast,
			_driver: driver,
		}
	}

	async fn read(stream: &mut PeerStream) -> (u64, ietf::Body) {
		let read = async {
			let id = stream.reader.varint().await.unwrap();
			(id, stream.reader.decode::<ietf::Body>().await.unwrap())
		};
		kio::clock::timeout(TIMEOUT, read)
			.await
			.expect("the subscriber sent nothing")
	}

	/// One message in one write, so a peer holding one message per stream still delivers it.
	async fn write<'m, M: ietf::Message<'m> + std::fmt::Debug>(stream: &mut PeerStream, id: u64, msg: &M) {
		stream.writer.buffer_message(id, msg).unwrap();
		kio::wait(|waiter| stream.writer.poll_flush(waiter)).await.unwrap();
		settle().await;
	}

	/// The next listing event as `+name`, `-name`, or `synced`; `None` while pending.
	fn event(tracks: &mut crate::broadcast::Tracks) -> Option<String> {
		match tracks.poll_next(&kio::Waiter::noop()) {
			Poll::Ready(Ok(Some(crate::broadcast::TracksEvent::Added { name, .. }))) => Some(format!("+{name}")),
			Poll::Ready(Ok(Some(crate::broadcast::TracksEvent::Removed { name }))) => Some(format!("-{name}")),
			Poll::Ready(Ok(Some(crate::broadcast::TracksEvent::Finished { name }))) => Some(format!("~{name}")),
			Poll::Ready(Ok(Some(crate::broadcast::TracksEvent::Synced))) => Some("synced".into()),
			Poll::Ready(other) => Some(format!("{other:?}")),
			Poll::Pending => None,
		}
	}

	impl Harness {
		/// Accept the SUBSCRIBE_TRACKS and establish it.
		async fn establish(&mut self) -> PeerStream {
			let mut stream = kio::clock::timeout(
				TIMEOUT,
				Stream::accept(&mut crate::driver::Transport::new(self.peer.clone()), self.version),
			)
			.await
			.expect("no SUBSCRIBE_TRACKS")
			.unwrap();
			let (id, body) = read(&mut stream).await;
			assert_eq!(id, ietf::SubscribeTracks::ID);
			let msg: ietf::SubscribeTracks = body.message(self.version).unwrap();
			assert_eq!(msg.prefix.as_str(), "bcast");
			assert!(!msg.forward, "the listing asks for every track's objects");
			let ok = ietf::RequestOk {
				active: None,
				largest: None,
			};
			write(&mut stream, ietf::RequestOk::ID, &ok).await;
			assert_eq!(event(&mut self.tracks).as_deref(), Some("synced"));
			stream
		}

		/// PUBLISH `name` paused, returning the stream once the subscriber holds it.
		async fn publish(&mut self, name: &str, request_id: u64) -> PeerStream {
			let (stream, id, _) = self.offer(offered("bcast", name, request_id)).await;
			assert!(
				[ietf::RequestOk::ID, ietf::PublishOk::ID].contains(&id),
				"PUBLISH refused: {id:#x}"
			);
			stream
		}

		/// A reader of `name`, served by the subscriber as a broadcast's request would be.
		async fn read_track(
			&mut self,
			name: &str,
		) -> (
			crate::track::Consumer,
			kio::Pending<crate::track::Subscribing>,
			kio::clock::sim::JoinHandle<()>,
		) {
			let mut dynamic = self._broadcast.requests();
			let track = self._broadcast.consume().track(name).unwrap();
			let reader = track.subscribe(None);
			let request = dynamic.requested_track().await.expect("no track requested");
			let mut subscriber = self.subscriber.clone();
			let serving = kio::clock::sim::spawn(async move {
				subscriber
					.run_subscribe(Path::from_static("bcast"), dynamic, None, request)
					.await;
			});
			(track, reader, serving)
		}

		/// Resume the held `video` for a reader, returning it once accepted.
		async fn resume(
			&mut self,
			video: &mut PeerStream,
		) -> (
			crate::track::Consumer,
			crate::track::Subscriber,
			kio::clock::sim::JoinHandle<()>,
		) {
			let (track, reader, serving) = self.read_track("video").await;
			let (id, body) = read(video).await;
			assert_eq!(id, ietf::SubscribeUpdate::ID, "not resumed");
			let resume: ietf::SubscribeUpdate = body.message(self.version).unwrap();
			assert_eq!(resume.forward, Some(true));
			let ok = ietf::RequestOk {
				active: None,
				largest: None,
			};
			write(video, ietf::RequestOk::ID, &ok).await;
			let reader = kio::clock::timeout(TIMEOUT, reader)
				.await
				.expect("never accepted")
				.unwrap();
			(track, reader, serving)
		}

		/// Resume the held `video` for a reader, then let it go so the subscription pauses.
		async fn resume_and_pause(
			&mut self,
			video: &mut PeerStream,
		) -> (crate::track::Consumer, kio::clock::sim::JoinHandle<()>) {
			let (track, reader, serving) = self.resume(video).await;
			drop(reader);
			let (id, body) = read(video).await;
			assert_eq!(id, ietf::SubscribeUpdate::ID, "not paused");
			let pause: ietf::SubscribeUpdate = body.message(self.version).unwrap();
			assert_eq!(pause.forward, Some(false));
			settle().await;
			(track, serving)
		}

		/// Send `publish`, returning the answer.
		async fn offer(&mut self, publish: ietf::Publish<'_>) -> (PeerStream, u64, ietf::Body) {
			let mut stream = Stream::open(&crate::driver::Transport::new(self.peer.clone()), self.version)
				.await
				.unwrap();
			stream.writer.encode_message(&publish).await.unwrap();

			let mut ours = Stream::accept(&mut self.subscriber.session.clone(), self.version)
				.await
				.unwrap();
			let id = ours.reader.varint().await.unwrap();
			assert_eq!(id, ietf::Publish::ID);
			let body: ietf::Body = ours.reader.decode().await.unwrap();
			let subscriber = self.subscriber.clone();
			let version = self.version;
			kio::clock::sim::spawn(async move {
				let msg = body.message::<ietf::Publish>(version).unwrap().into_owned();
				let mut serve = ServePublish::new(subscriber, ours, msg);
				let _ = kio::wait(|waiter| serve.poll(waiter)).await;
			});
			let (id, body) = read(&mut stream).await;
			(stream, id, body)
		}
	}

	/// Readers coming and going while the peer withholds credit on a held track never pile
	/// updates up behind it: the held subscription is handed over only once its pause is out,
	/// and a reader leaving meanwhile stops waiting for it.
	#[kio::test]
	async fn churn_under_withheld_credit_buffers_one_resume_and_pause() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			// One message in flight per stream, so a peer that stops reading withholds credit.
			let config = memory::Config {
				stream_buffer: 0,
				..Default::default()
			};
			let mut h = harness_with(version, config);
			let _listing = h.establish().await;
			let mut video = h.publish("video", 1).await;

			for _ in 0..3 {
				let (track, reader, serving) = h.read_track("video").await;
				settle().await;
				drop(reader);
				drop(track);
				kio::clock::timeout(TIMEOUT, serving)
					.await
					.unwrap_or_else(|_| panic!("{version}: the abandoned resume stayed alive"))
					.unwrap();
			}

			let mut updates = Vec::new();
			while let Ok(Ok(id)) = kio::clock::timeout(TIMEOUT, video.reader.varint()).await {
				let body: ietf::Body = video.reader.decode().await.unwrap();
				assert_eq!(id, ietf::SubscribeUpdate::ID, "{version}");
				updates.push(body.message::<ietf::SubscribeUpdate>(version).unwrap().forward);
			}
			assert_eq!(updates, [Some(true), Some(false)], "{version}");
		}
	}

	/// A paused PUBLISH of `name` in `namespace`, aliased by its request ID.
	fn offered<'a>(namespace: &'a str, name: &'a str, request_id: u64) -> ietf::Publish<'a> {
		ietf::Publish {
			authorization_token: None,
			request_id: RequestId(request_id),
			track_namespace: Path::new(namespace).unwrap(),
			track_name: name.into(),
			track_alias: request_id,
			largest_location: None,
			forward: false,
			properties: ietf::Properties::default(),
			subscriber_priority: None,
			group_order: None,
			filter: None,
		}
	}

	/// One SUBSCRIBE_TRACKS covers each nesting chain, since the peer refuses an overlapping
	/// prefix: a nested listing reuses the broader request, and a broader one cancels the
	/// nested requests and goes out only once they have left the wire.
	#[test]
	fn overlapping_listings_share_one_request() {
		let broadcasts: Vec<_> = (0..5).map(|_| crate::broadcast::Info::default().produce()).collect();
		let lister = |i: usize| Arc::new(broadcasts[i].requests().lister().unwrap());
		let live = |listings: &Listings| listings.requests.values().filter(|r| r.live.is_some()).count();

		let mut listings = Listings::default();
		let (_, _, outer) = listings.listen(Path::from_static("a"), lister(0));
		assert!(
			outer.is_some_and(|outer| outer.blockers.is_empty()),
			"the first listing waits"
		);
		let (_, _, nested) = listings.listen(Path::from_static("a/b"), lister(1));
		assert!(nested.is_none(), "a nested listing asked again");
		assert_eq!(live(&listings), 1);

		let mut listings = Listings::default();
		for (i, path) in ["a/b", "a/c"].into_iter().enumerate() {
			let (_, _, nested) = listings.listen(Path::new(path).unwrap(), lister(2 + i));
			assert!(nested.is_some_and(|nested| nested.blockers.is_empty()), "{path}");
		}
		let (_, _, outer) = listings.listen(Path::from_static("a"), lister(4));
		assert_eq!(outer.expect("the broader listing asks").blockers.len(), 2);
		assert_eq!(live(&listings), 1, "the nested requests stand beside it");
	}

	/// The error code a refused PUBLISH answered with.
	fn refusal(id: u64, body: &ietf::Body, version: Version) -> u64 {
		assert_eq!(id, ietf::RequestError::ID, "accepted");
		body.message::<ietf::RequestError>(version).unwrap().error_code
	}

	/// A PUBLISH no listing asked for stays refused, and one for a track already held is
	/// redundant: DUPLICATE_SUBSCRIPTION on draft-18, which allows one subscription per
	/// track, and UNINTERESTED after.
	#[kio::test]
	async fn an_unasked_or_redundant_publish_is_refused() {
		for (version, duplicate) in [(Version::Draft18, 0x19), (Version::Draft22, 0x20)] {
			let mut h = harness(version);
			let _listing = h.establish().await;
			let (_, id, body) = h.offer(offered("other", "video", 1)).await;
			assert_eq!(refusal(id, &body, version), 0x3, "{version}: unsolicited");
			let _video = h.publish("video", 3).await;
			let (_, id, body) = h.offer(offered("bcast", "video", 5)).await;
			assert_eq!(refusal(id, &body, version), duplicate, "{version}: redundant");
		}
	}

	/// A PUBLISH over the listed-track limit is refused as EXCESSIVE_LOAD, which may pass once
	/// the peer unlists a track, so it invites a retry (draft-18 section 10.6.2).
	#[kio::test]
	async fn an_over_limit_publish_invites_a_retry() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			let mut h = harness(version);
			h.subscriber.announces = crate::session::Slots::new(0);
			let _listing = h.establish().await;
			let (_, id, body) = h.offer(offered("bcast", "video", 1)).await;
			assert_eq!(id, ietf::RequestError::ID, "{version}: accepted");
			let refused: ietf::RequestError = body.message(version).unwrap();
			assert_eq!(refused.error_code, request::EXCESSIVE_LOAD, "{version}");
			assert_eq!(
				refused.retry_interval,
				crate::auth::RETRY_INITIAL.as_millis() as u64,
				"{version}"
			);
		}
	}

	/// A PUBLISH_DONE with TRACK_ENDED unlists the track as finished, and the listing stands.
	#[kio::test]
	async fn a_track_ended_publish_finishes_the_track() {
		let version = Version::Draft22;
		let mut h = harness(version);
		let _listing = h.establish().await;
		let mut video = h.publish("video", 1).await;
		assert_eq!(event(&mut h.tracks).as_deref(), Some("+video"));

		let done = ietf::PublishDone {
			status_code: ietf::PublishDoneStatus::TrackEnded.code(),
			stream_count: 0,
			reason_phrase: "".into(),
		};
		write(&mut video, ietf::PublishDone::ID, &done).await;
		assert_eq!(event(&mut h.tracks).as_deref(), Some("~video"));
		assert!(h.listening.poll_failed(&kio::Waiter::noop()).is_pending());
	}

	/// Any other PUBLISH_DONE, INTERNAL_ERROR included, fails the listing rather than hide a
	/// track that may be live.
	#[kio::test]
	async fn another_publish_done_fails_the_listing() {
		let version = Version::Draft22;
		for (status, expected) in [
			(ietf::PublishDoneStatus::Unauthorized, Error::Unauthorized),
			(ietf::PublishDoneStatus::InternalError, Error::Remote(0)),
		] {
			let mut h = harness(version);
			let _listing = h.establish().await;
			let mut video = h.publish("video", 1).await;
			assert_eq!(event(&mut h.tracks).as_deref(), Some("+video"));

			let done = ietf::PublishDone {
				status_code: status.code(),
				stream_count: 0,
				reason_phrase: "".into(),
			};
			write(&mut video, ietf::PublishDone::ID, &done).await;
			match h.listening.poll_failed(&kio::Waiter::noop()) {
				Poll::Ready(err) => assert_eq!(err.to_string(), expected.to_string(), "{status:?}"),
				Poll::Pending => panic!("{status:?}: the listing stood"),
			}
		}
	}

	/// A resumed listed track ending any way but TRACK_ENDED fails the listing, even a
	/// clean SUBSCRIPTION_ENDED (draft 18), which says nothing of the track itself.
	#[kio::test]
	async fn a_resumed_track_ending_otherwise_fails_the_listing() {
		let version = Version::Draft18;
		let mut h = harness(version);
		let _listing = h.establish().await;
		let mut video = h.publish("video", 1).await;
		assert_eq!(event(&mut h.tracks).as_deref(), Some("+video"));

		let (_track, _reader, _serving) = h.resume(&mut video).await;
		assert!(h.listening.poll_failed(&kio::Waiter::noop()).is_pending());

		// SUBSCRIPTION_ENDED, which draft 18 registers.
		let done = ietf::PublishDone {
			status_code: 0x3,
			stream_count: 0,
			reason_phrase: "".into(),
		};
		write(&mut video, ietf::PublishDone::ID, &done).await;
		// The subscription waits out its data streams before it ends.
		kio::clock::sleep(Duration::from_secs(5)).await;
		assert!(h.listening.poll_failed(&kio::Waiter::noop()).is_ready());
	}

	/// A skipped track is listed through a paused SUBSCRIBE of ours, once it answers.
	#[kio::test]
	async fn a_skipped_track_lists_once_its_paused_subscribe_answers() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			let mut h = harness(version);
			let mut listing = h.establish().await;
			let skipped = ietf::PublishSkipped {
				suffix: Path::from_static(""),
				track_name: "audio".into(),
			};
			write(&mut listing, ietf::PublishSkipped::ID, &skipped).await;
			assert_eq!(event(&mut h.tracks), None, "{version}: listed before it answered");

			let mut stream = kio::clock::timeout(
				TIMEOUT,
				Stream::accept(&mut crate::driver::Transport::new(h.peer.clone()), version),
			)
			.await
			.expect("no paused SUBSCRIBE")
			.unwrap();
			let (id, body) = read(&mut stream).await;
			assert_eq!(id, ietf::Subscribe::ID, "{version}");
			let msg: ietf::Subscribe = body.message(version).unwrap();
			assert_eq!(msg.track_name, "audio", "{version}");
			assert_eq!(
				msg.group_order,
				crate::track::Order::default().into(),
				"{version}: the order a reader gets by default"
			);
			assert!(!msg.forward, "{version}: the SUBSCRIBE forwards");

			let ok = ietf::SubscribeOk {
				track_alias: 3,
				largest: None,
				properties: ietf::Properties {
					timescale: Some(crate::Timescale::MILLI),
					..Default::default()
				},
			};
			write(&mut stream, ietf::SubscribeOk::ID, &ok).await;
			assert_eq!(event(&mut h.tracks).as_deref(), Some("+audio"), "{version}");
		}
	}

	/// A refused paused SUBSCRIBE fails the listing.
	#[kio::test]
	async fn a_refused_paused_subscribe_fails_the_listing() {
		let version = Version::Draft22;
		let mut h = harness(version);
		let mut listing = h.establish().await;
		let skipped = ietf::PublishSkipped {
			suffix: Path::from_static(""),
			track_name: "audio".into(),
		};
		write(&mut listing, ietf::PublishSkipped::ID, &skipped).await;
		let mut stream = kio::clock::timeout(
			TIMEOUT,
			Stream::accept(&mut crate::driver::Transport::new(h.peer.clone()), version),
		)
		.await
		.expect("no paused SUBSCRIBE")
		.unwrap();
		let _ = read(&mut stream).await;
		let refused = ietf::RequestError {
			error_code: 0x3,
			reason_phrase: "".into(),
			retry_interval: 0,
		};
		write(&mut stream, ietf::RequestError::ID, &refused).await;
		assert!(matches!(
			h.listening.poll_failed(&kio::Waiter::noop()),
			Poll::Ready(Error::Unsupported)
		));
	}

	/// A listed PUBLISH whose properties we cannot take fails the listing: the peer sends
	/// nothing else for that track, so a listing left standing would silently miss it.
	#[kio::test]
	async fn an_unsupported_publish_fails_the_listing() {
		let version = Version::Draft22;
		let mut h = harness(version);
		let _listing = h.establish().await;
		let compressed = ietf::Publish {
			properties: ietf::Properties {
				compress: true,
				..Default::default()
			},
			..offered("bcast", "video", 1)
		};
		let (_, id, _) = h.offer(compressed).await;
		assert_eq!(id, ietf::RequestError::ID, "accepted");
		assert!(matches!(
			h.listening.poll_failed(&kio::Waiter::noop()),
			Poll::Ready(Error::UnsupportedExtension)
		));
	}

	/// On draft-22 a PUBLISH that forwards contradicts the paused listing it answers, and
	/// nothing we send can pause it: it is refused and the listing fails, rather than have its
	/// objects refused unseen. On draft-18 our PUBLISH_OK pauses it.
	#[kio::test]
	async fn a_forwarding_publish_fails_the_listing_on_draft_22() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			let mut h = harness(version);
			let _listing = h.establish().await;
			let forwarding = ietf::Publish {
				forward: true,
				..offered("bcast", "video", 1)
			};
			let (_, id, _) = h.offer(forwarding).await;
			let failed = h.listening.poll_failed(&kio::Waiter::noop());
			match version {
				Version::Draft18 => {
					assert_ne!(id, ietf::RequestError::ID, "{version}: refused");
					assert!(failed.is_pending(), "{version}: {failed:?}");
				}
				_ => {
					assert_eq!(id, ietf::RequestError::ID, "{version}: accepted");
					assert!(
						matches!(failed, Poll::Ready(Error::Unsupported)),
						"{version}: {failed:?}"
					);
				}
			}
		}
	}

	/// A copy that learned DYNAMIC_GROUPS keeps it through a draft-22 resume that opts out of
	/// the properties, so once that subscription pauses as the listing's entry, the
	/// REQUEST_UPDATE resuming it still carries the group a join asked for.
	#[kio::test]
	async fn a_paused_resume_that_opted_out_keeps_dynamic_groups() {
		let version = Version::Draft22;
		let mut h = harness(version);
		let mut listing = h.establish().await;
		let accept = |peer: memory::Session| async move {
			let mut stream = kio::clock::timeout(TIMEOUT, Stream::accept(&mut Transport::new(peer), version))
				.await
				.expect("no SUBSCRIBE")
				.unwrap();
			let (id, body) = read(&mut stream).await;
			assert_eq!(id, ietf::Subscribe::ID);
			let msg: ietf::Subscribe = body.message(version).unwrap();
			(stream, msg.properties_wanted, msg.new_group)
		};
		let subscribe_ok = |dynamic_groups| ietf::SubscribeOk {
			track_alias: 3,
			largest: Some(ietf::Location { group: 5, object: 0 }),
			properties: ietf::Properties {
				dynamic_groups,
				..Default::default()
			},
		};
		let answer = ietf::RequestOk {
			active: None,
			largest: None,
		};

		// The first answer declares DYNAMIC_GROUPS, and group 5 lands with a delta.
		let (track, first, _serving) = h.read_track("video").await;
		let (mut stream, wanted, _) = accept(h.peer.clone()).await;
		assert!(wanted);
		write(&mut stream, ietf::SubscribeOk::ID, &subscribe_ok(true)).await;
		let first = kio::clock::timeout(TIMEOUT, first)
			.await
			.expect("never accepted")
			.unwrap();
		let copy = h
			.subscriber
			.state
			.lock()
			.subscribes
			.values()
			.next()
			.unwrap()
			.producer
			.clone()
			.unwrap();
		let mut group = copy.create_group(crate::model::group::Info { start: 5 }).unwrap();
		group.write_frame(None, b"key".as_slice()).unwrap();
		group.write_frame(None, b"delta".as_slice()).unwrap();

		// A join past the ratio asks for group 6, which stays outstanding.
		let join = track.subscribe(None).await.unwrap();
		settle().await;
		let (id, body) = read(&mut stream).await;
		assert_eq!(id, ietf::SubscribeUpdate::ID);
		assert_eq!(
			body.message::<ietf::SubscribeUpdate>(version).unwrap().new_group,
			Some(6)
		);
		write(&mut stream, ietf::RequestOk::ID, &answer).await;

		// Everyone leaves, and the copy lingers.
		drop((first, join));
		settle().await;

		// A returning reader resumes it, opting out of the properties: the answer has none.
		let reader = track.subscribe(None);
		let (mut stream, wanted, new_group) = accept(h.peer.clone()).await;
		assert!(!wanted, "the resume opts out");
		assert_eq!(new_group, Some(6));
		write(&mut stream, ietf::SubscribeOk::ID, &subscribe_ok(false)).await;
		let reader = kio::clock::timeout(TIMEOUT, reader)
			.await
			.expect("never accepted")
			.unwrap();

		// Skipped in the listing, so this subscription becomes the entry and pauses.
		let skipped = ietf::PublishSkipped {
			suffix: Path::from_static(""),
			track_name: "video".into(),
		};
		write(&mut listing, ietf::PublishSkipped::ID, &skipped).await;
		assert_eq!(event(&mut h.tracks).as_deref(), Some("+video"));
		drop(reader);
		let (id, body) = read(&mut stream).await;
		assert_eq!(id, ietf::SubscribeUpdate::ID, "not paused");
		assert_eq!(
			body.message::<ietf::SubscribeUpdate>(version).unwrap().forward,
			Some(false)
		);
		write(&mut stream, ietf::RequestOk::ID, &answer).await;

		// The resume still asks for group 6, since the track declared DYNAMIC_GROUPS.
		let _reader = track.subscribe(None);
		let (id, body) = read(&mut stream).await;
		assert_eq!(id, ietf::SubscribeUpdate::ID, "not resumed");
		let resume: ietf::SubscribeUpdate = body.message(version).unwrap();
		assert_eq!(resume.forward, Some(true));
		assert_eq!(resume.new_group, Some(6), "the pause forgot DYNAMIC_GROUPS");
	}

	/// A listed track that finishes before its resume is answered goes back to lingering, and
	/// its waiting reader SUBSCRIBEs: the idle copy cannot place the end, which the SUBSCRIBE
	/// learns, rather than abort a clean finish.
	#[kio::test]
	async fn a_track_ending_before_its_resume_resubscribes() {
		let version = Version::Draft22;
		let mut h = harness(version);
		let _listing = h.establish().await;
		let mut video = h.publish("video", 1).await;
		let (track, _serving) = h.resume_and_pause(&mut video).await;

		let _reader = track.subscribe(None);
		let (id, body) = read(&mut video).await;
		assert_eq!(id, ietf::SubscribeUpdate::ID, "not resumed");
		assert_eq!(
			body.message::<ietf::SubscribeUpdate>(version).unwrap().forward,
			Some(true)
		);
		let done = ietf::PublishDone {
			status_code: ietf::PublishDoneStatus::TrackEnded.code(),
			stream_count: 0,
			reason_phrase: "".into(),
		};
		write(&mut video, ietf::PublishDone::ID, &done).await;
		assert!(
			h.listening.poll_failed(&kio::Waiter::noop()).is_pending(),
			"TRACK_ENDED failed the listing"
		);

		let mut stream = kio::clock::timeout(
			TIMEOUT,
			Stream::accept(&mut crate::driver::Transport::new(h.peer.clone()), version),
		)
		.await
		.expect("the copy did not SUBSCRIBE")
		.unwrap();
		let (id, _) = read(&mut stream).await;
		assert_eq!(id, ietf::Subscribe::ID);
	}

	/// A paused track's upstream failing aborts the copy a reader left lingering, rather
	/// than leave it open with nothing upstream.
	#[kio::test]
	async fn a_paused_track_failing_aborts_its_lingering_copy() {
		let version = Version::Draft22;
		let mut h = harness(version);
		let _listing = h.establish().await;
		let mut video = h.publish("video", 1).await;
		let (track, serving) = h.resume_and_pause(&mut video).await;
		assert!(!serving.is_finished(), "the copy did not linger");

		let done = ietf::PublishDone {
			status_code: ietf::PublishDoneStatus::InternalError.code(),
			stream_count: 0,
			reason_phrase: "".into(),
		};
		write(&mut video, ietf::PublishDone::ID, &done).await;
		assert!(serving.is_finished(), "the copy outlived its upstream");
		drop(track);
	}

	/// The answer a pause owes is decoded like any other, so a malformed one closes the
	/// session rather than count as an answer.
	#[kio::test]
	async fn a_malformed_pause_answer_closes_the_session() {
		let version = Version::Draft22;
		let mut h = harness(version);
		let _listing = h.establish().await;
		let mut video = h.publish("video", 1).await;
		let (_track, _serving) = h.resume_and_pause(&mut video).await;

		// A REQUEST_OK with no parameters, then a trailing byte.
		video.writer.varint(ietf::RequestOk::ID).await.unwrap();
		video
			.writer
			.write_all(&mut &[0x00, 0x02, 0x00, 0xff][..])
			.await
			.unwrap();
		let peer = h.peer.clone();
		let closed = kio::wait(move |waiter| crate::transport::poll::Session::poll_closed(&peer, waiter));
		kio::clock::timeout(TIMEOUT, closed)
			.await
			.expect("the session stayed up");
	}

	/// A peer that refuses SUBSCRIBE_TRACKS, resets it, or never answers it fails at most
	/// that listing: a plain SUBSCRIBE still goes out and is accepted on the session.
	#[kio::test]
	async fn a_peer_not_answering_the_listing_still_serves_subscribe() {
		let version = Version::Draft22;
		for case in ["refused", "reset", "silent"] {
			let mut h = harness(version);
			let mut listing = kio::clock::timeout(
				TIMEOUT,
				Stream::accept(&mut crate::driver::Transport::new(h.peer.clone()), version),
			)
			.await
			.expect("no SUBSCRIBE_TRACKS")
			.unwrap();
			let (id, _) = read(&mut listing).await;
			assert_eq!(id, ietf::SubscribeTracks::ID);
			let _held = match case {
				"refused" => {
					let refused = ietf::RequestError {
						error_code: 0x3,
						reason_phrase: "".into(),
						retry_interval: 0,
					};
					write(&mut listing, ietf::RequestError::ID, &refused).await;
					None
				}
				"reset" => {
					let Stream { writer, reader } = listing;
					writer.abort(&Error::Cancel);
					settle().await;
					Some(reader)
				}
				_ => {
					settle().await;
					Some(listing.reader)
				}
			};
			let failed = h.listening.poll_failed(&kio::Waiter::noop());
			match case {
				"refused" => assert!(matches!(failed, Poll::Ready(Error::Unsupported)), "{case}: {failed:?}"),
				"reset" => assert!(failed.is_ready(), "{case}: the listing stood"),
				_ => assert!(failed.is_pending(), "{case}: the listing failed"),
			}

			let (_track, reader, _serving) = h.read_track("video").await;
			let mut stream = kio::clock::timeout(
				TIMEOUT,
				Stream::accept(&mut crate::driver::Transport::new(h.peer.clone()), version),
			)
			.await
			.expect("no SUBSCRIBE")
			.unwrap();
			let (id, body) = read(&mut stream).await;
			assert_eq!(id, ietf::Subscribe::ID, "{case}");
			let msg: ietf::Subscribe = body.message(version).unwrap();
			assert!(msg.forward, "{case}: a paused SUBSCRIBE");
			let ok = ietf::SubscribeOk {
				track_alias: 7,
				largest: None,
				properties: ietf::Properties::default(),
			};
			write(&mut stream, ietf::SubscribeOk::ID, &ok).await;
			kio::clock::timeout(TIMEOUT, reader)
				.await
				.expect("never accepted")
				.unwrap_or_else(|err| panic!("{case}: {err}"));
		}
	}

	/// Dropping the listing cancels its SUBSCRIBE_TRACKS: FIN on draft-18, RESET_STREAM on
	/// draft-22, where a FIN is no cancel. The held PUBLISHes are let go with it.
	#[kio::test]
	async fn dropping_the_listing_cancels_it() {
		let version = Version::Draft18;
		let mut h = harness(version);
		let mut listing = h.establish().await;
		let mut video = h.publish("video", 1).await;
		drop(h.listening);
		settle().await;
		let ended = kio::clock::timeout(TIMEOUT, listing.reader.varint_maybe())
			.await
			.expect("the listing stayed open");
		match version {
			Version::Draft18 => assert!(matches!(ended, Ok(None)), "{version}: {ended:?}"),
			_ => assert!(ended.is_err(), "{version}: {ended:?}"),
		}
		let ended = kio::clock::timeout(TIMEOUT, video.reader.varint_maybe())
			.await
			.expect("the PUBLISH stayed held");
		assert!(!matches!(ended, Ok(Some(_))), "{version}: {ended:?}");
	}
}
