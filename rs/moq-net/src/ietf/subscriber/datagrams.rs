//! The two model kinds share one kindless IETF subscription, as on lite-05/06.
use super::*;
use crate::datagram;

/// Keeps the initial stream interest until its answer also accepts the datagram request.
pub(super) struct Requested {
	this: Subscriber,
	key: tracks::TrackKey,
	id: std::sync::Arc<()>,
	subscribing: track::Subscribing,
	priority: i8,
}

impl Requested {
	pub(super) fn new(
		this: Subscriber,
		key: tracks::TrackKey,
		track: track::Consumer,
		request: datagram::Request,
	) -> Self {
		let priority = request.priority().unwrap_or(0);
		let id = std::sync::Arc::new(());
		this.state
			.lock()
			.datagram_requests
			.insert(key.clone(), (id.clone(), request));
		Self {
			id,
			priority,
			this,
			key,
			subscribing: track
				.subscribe(track::Subscription::default().with_priority(priority))
				.into_inner(),
		}
	}
	fn reject(&self, err: Error) {
		let mut state = self.this.state.lock();
		if let Entry::Occupied(entry) = state.datagram_requests.entry(self.key.clone())
			&& std::sync::Arc::ptr_eq(&entry.get().0, &self.id)
		{
			entry.remove().1.reject(err);
		}
	}
}
impl kio::Task for Requested {
	type Output = ();
	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		let priority = {
			let state = self.this.state.lock();
			let Some((id, request)) = state.datagram_requests.get(&self.key) else {
				return Poll::Ready(());
			};
			if !std::sync::Arc::ptr_eq(id, &self.id) {
				return Poll::Ready(());
			}
			if request.poll_unused(waiter).is_ready() && request.close_unused() {
				drop(state);
				self.reject(Error::Cancel);
				return Poll::Ready(());
			}
			request.priority()
		};
		if let Some(priority) = priority
			&& priority != self.priority
		{
			self.priority = priority;
			if let Err(err) = self
				.subscribing
				.update(track::Subscription::default().with_priority(priority))
			{
				self.reject(err);
				return Poll::Ready(());
			}
		}
		if let Poll::Ready(Err(err)) = self.subscribing.poll_ok(waiter) {
			self.reject(err);
			return Poll::Ready(());
		}
		// A lingering track already knows its metadata. Only Shared accepting this request
		// confirms that its new upstream subscription also owns the datagram half.
		Poll::Pending
	}
}
impl Drop for Requested {
	fn drop(&mut self) {
		self.reject(Error::Cancel);
	}
}

/// The datagram half of a stream subscription, holding demand only for actual readers.
pub(super) struct Shared {
	pub(super) producer: datagram::Producer,
	track: track::Producer,
	held: Option<track::Subscriber>,
	priority: Option<i8>,
}
impl Shared {
	pub(super) fn new(
		this: &Subscriber,
		source: &broadcast::Producer,
		key: tracks::TrackKey,
		track: &track::Producer,
	) -> Option<Self> {
		let info = datagram::Info::default()
			.with_timescale(track.info().timescale)
			.with_priority(track.info().priority);
		let pending = this.state.lock().datagram_requests.remove(&key);
		let producer = match pending {
			Some((_, request)) => request.accept(info),
			None => source.create_datagrams(key.1, info).ok()?,
		};
		Some(Self {
			producer,
			track: track.clone(),
			held: None,
			priority: None,
		})
	}
	pub(super) fn poll(&mut self, waiter: &kio::Waiter) {
		while let Poll::Ready(priority) = self.producer.poll_priority(waiter, self.priority) {
			self.priority = priority;
			match priority {
				None => self.held = None,
				Some(priority) => match &mut self.held {
					Some(held) => {
						if let Err(err) = held.update(held.subscription().with_priority(priority)) {
							self.producer.clone().abort(err);
						}
					}
					None => {
						self.held = Some(
							self.track
								.subscribe(track::Subscription::default().with_priority(priority))
								.parked(),
						)
					}
				},
			}
		}
	}
	pub(super) fn end(&mut self, end: Result<(), Error>) {
		self.held = None;
		match end {
			Ok(()) => self.producer.clone().finish(),
			Err(err) => self.producer.clone().abort(err),
		}
	}
}
impl Drop for Shared {
	fn drop(&mut self) {
		self.end(Err(Error::Cancel));
	}
}
