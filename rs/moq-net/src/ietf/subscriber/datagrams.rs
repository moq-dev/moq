//! The two model kinds share one kindless IETF subscription, as on lite-05/06.
use super::*;
use crate::datagram;

/// Keeps the initial stream interest until its answer also accepts the datagram request.
pub(super) struct Requested {
	this: Subscriber,
	key: tracks::TrackKey,
	subscribing: track::Subscribing,
}

impl Requested {
	pub(super) fn new(this: Subscriber, key: tracks::TrackKey, track: track::Consumer, priority: i8) -> Self {
		Self {
			this,
			key,
			subscribing: track
				.subscribe(track::Subscription::default().with_priority(priority))
				.into_inner(),
		}
	}
	fn reject(&self, err: Error) {
		if let Some(request) = self.this.state.lock().datagram_requests.remove(&self.key) {
			request.reject(err);
		}
	}
}
impl kio::Task for Requested {
	type Output = ();
	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		{
			let state = self.this.state.lock();
			if let Some(request) = state.datagram_requests.get(&self.key)
				&& request.poll_unused(waiter).is_ready()
				&& request.close_unused()
			{
				drop(state);
				self.reject(Error::Cancel);
				return Poll::Ready(());
			}
		}
		match ready!(self.subscribing.poll_ok(waiter)) {
			Ok(_) => {}
			Err(err) => self.reject(err),
		}
		Poll::Ready(())
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
			Some(request) => request.accept(info),
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
		if let Poll::Ready(priority) = self.producer.poll_priority(waiter, self.priority) {
			self.priority = priority;
			match priority {
				None => self.held = None,
				Some(priority) => match &mut self.held {
					Some(held) => {
						if let Err(err) = held.update(held.subscription().with_priority(priority)) {
							self.producer.abort(err);
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
			Ok(()) => self.producer.finish(),
			Err(err) => self.producer.abort(err),
		}
	}
}
impl Drop for Shared {
	fn drop(&mut self) {
		self.end(Err(Error::Cancel));
	}
}
