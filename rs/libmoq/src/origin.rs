use std::ffi::c_char;
use tokio::sync::oneshot;

use crate::ffi::OnStatus;
use crate::{Error, Id, NonZeroSlab, State, moq_announced};

/// A spawned task entry: `close` signals shutdown, `callback` delivers status.
///
/// `close` is an `Option` so `*_close` can drop just the sender without
/// removing the entry. The task delivers one final terminal callback and then
/// removes itself, so `user_data` stays valid until that callback fires.
struct TaskEntry {
	close: Option<oneshot::Sender<()>>,
	callback: OnStatus,
}

/// Global state managing all active resources.
///
/// Stores all sessions, origins, broadcasts, tracks, and frames in slab allocators,
/// returning opaque IDs to C callers. Also manages async tasks via oneshot channels
/// for cancellation.
// TODO split this up into separate structs/mutexes
#[derive(Default)]
pub struct Origin {
	/// Active origin producers for publishing and consuming broadcasts.
	active: NonZeroSlab<moq_net::origin::Producer>,

	/// Broadcast announcement information (path, active status).
	announced: NonZeroSlab<(String, bool)>,

	/// Announcement listener tasks. Close signals shutdown; the task delivers a final callback, then removes itself.
	announced_task: NonZeroSlab<Option<TaskEntry>>,

	/// Pending consume-until-announced tasks. Close signals shutdown; the task delivers a final callback, then removes itself.
	consume_task: NonZeroSlab<Option<TaskEntry>>,
}

impl Origin {
	pub fn create(&mut self) -> Result<Id, Error> {
		// Every FFI entry point runs inside `RUNTIME.enter()`, so the driver
		// lands on the dedicated libmoq runtime.
		self.active.insert(moq_tokio::origin::spawn(moq_net::Origin::random()))
	}

	pub fn get(&self, id: Id) -> Result<&moq_net::origin::Producer, Error> {
		self.active.get(id).ok_or(Error::OriginNotFound)
	}

	pub fn announced(&mut self, origin: Id, on_announce: OnStatus) -> Result<Id, Error> {
		let origin = self.active.get_mut(origin).ok_or(Error::OriginNotFound)?;
		let consumer = origin.consume().announced();
		let channel = oneshot::channel();

		let entry = TaskEntry {
			close: Some(channel.0),
			callback: on_announce,
		};
		let id = self.announced_task.insert(Some(entry))?;

		tokio::spawn(async move {
			let res = Self::run_announced(on_announce, consumer, channel.1).await;

			// Deliver one final terminal callback (code <= 0), then drop the entry.
			// Pull it out from under the lock so the callback never runs while held.
			let entry = State::lock().origin.announced_task.remove(id).flatten();
			if let Some(entry) = entry {
				entry.callback.call(res);
			}
		});

		Ok(id)
	}

	async fn run_announced(
		callback: OnStatus,
		mut consumer: moq_net::announce::Consumer,
		mut close: oneshot::Receiver<()>,
	) -> Result<(), Error> {
		loop {
			// `biased` so a pending close always wins over a ready announcement.
			let update = tokio::select! {
				biased;
				_ = &mut close => return Ok(()),
				next = consumer.next() => match next {
					Some(announced) => announced,
					None => return Ok(()),
				},
			};

			// Hold the lock only to buffer the announcement; release it before the callback.
			let announced_id = State::lock()
				.origin
				.announced
				.insert((update.route.prefix.to_string(), update.active))?;
			callback.call(announced_id);
		}
	}

	pub fn announced_info(&self, announced: Id, dst: &mut moq_announced) -> Result<(), Error> {
		let announced = self.announced.get(announced).ok_or(Error::AnnouncementNotFound)?;
		*dst = moq_announced {
			path: announced.0.as_str().as_ptr() as *const c_char,
			path_len: announced.0.len(),
			active: announced.1,
		};
		Ok(())
	}

	/// Free a single announcement record delivered to an `on_announce` callback.
	///
	/// Each announce/unannounce event allocates a record (read via [`Self::announced_info`]);
	/// the caller releases it here once done. This is per-record, distinct from
	/// [`Self::announced_close`], which stops the whole listener. Records are freed explicitly
	/// rather than on unannounce: an unannounce is its own delivered record, and auto-freeing
	/// the prior one would race a caller still reading it.
	pub fn announced_free(&mut self, announced: Id) -> Result<(), Error> {
		self.announced.remove(announced).ok_or(Error::AnnouncementNotFound)?;
		Ok(())
	}

	pub fn announced_close(&mut self, announced: Id) -> Result<(), Error> {
		// Signal shutdown; the task delivers a final callback and removes itself.
		self.announced_task
			.get_mut(announced)
			.and_then(|entry| entry.as_mut())
			.ok_or(Error::AnnouncementNotFound)?
			.close
			.take()
			.ok_or(Error::AnnouncementNotFound)?;
		Ok(())
	}

	/// Wait until the broadcast at `path` is announced, then deliver its handle via the callback.
	///
	/// The callback fires the broadcast handle (> 0) once announced, then a terminal `0`. On error
	/// or cancellation it fires a single terminal code (`0` on close, negative on error). Returns a
	/// task handle for cancellation via [`Self::consume_announced_close`].
	pub fn consume_announced(&mut self, origin: Id, path: String, on_broadcast: OnStatus) -> Result<Id, Error> {
		let origin = self.active.get_mut(origin).ok_or(Error::OriginNotFound)?;
		let consumer = origin.consume();
		let channel = oneshot::channel();

		let entry = TaskEntry {
			close: Some(channel.0),
			callback: on_broadcast,
		};
		let id = self.consume_task.insert(Some(entry))?;

		tokio::spawn(async move {
			let res = Self::run_consume_announced(on_broadcast, consumer, path, channel.1).await;

			// Deliver one final terminal callback (code <= 0), then drop the entry.
			// Pull it out from under the lock so the callback never runs while held.
			let entry = State::lock().origin.consume_task.remove(id).flatten();
			if let Some(entry) = entry {
				entry.callback.call(res);
			}
		});

		Ok(id)
	}

	async fn run_consume_announced(
		callback: OnStatus,
		consumer: moq_net::origin::Consumer,
		path: String,
		mut close: oneshot::Receiver<()>,
	) -> Result<(), Error> {
		// Watch the path's coverage so an Unroutable resolution waits for the route
		// table to change instead of surfacing as a spurious terminal failure: the
		// covering route can retract between the wait and the resolution (failover
		// churn), or cover the path while nothing serves it yet.
		let scoped = consumer.with_root(path.as_str()).ok_or(Error::BroadcastNotFound)?;
		let mut announced = scoped.announced();
		let broadcast = loop {
			// `biased` so a pending close always wins over a ready announcement.
			tokio::select! {
				biased;
				_ = &mut close => return Ok(()),
				found = consumer.routed(path.as_str()) => {
					found.ok_or(Error::BroadcastNotFound)?;
				}
			};
			match consumer.request_broadcast(path.as_str()).await {
				Ok(broadcast) => break broadcast,
				// Ride out the churn: retry once the coverage changes (the cursor
				// replays the current coverage first, so at most one extra attempt
				// runs before this genuinely blocks).
				Err(moq_net::Error::Unroutable) => {
					tokio::select! {
						biased;
						_ = &mut close => return Ok(()),
						update = announced.next() => {
							if update.is_none() {
								return Err(Error::BroadcastNotFound);
							}
						}
					}
				}
				Err(err) => return Err(err.into()),
			}
		};

		// Hold the lock only to buffer the broadcast; release it before the callback.
		let broadcast_id = State::lock().consume.start(broadcast, Some(consumer))?;
		callback.call(broadcast_id);
		Ok(())
	}

	/// Request the broadcast at `path`, delivering its handle once it can be served.
	///
	/// Unlike [`Self::consume`] (announced-only, fails fast) and [`Self::consume_announced`]
	/// (waits indefinitely for a future announcement), this resolves against any broadcast
	/// reachable by exact path now, whether announced or not: the callback fires the broadcast
	/// handle (> 0) once served, then a terminal `0`; or a single terminal code (`0` on close,
	/// negative on error) if it can't be served. Returns a task handle for cancellation.
	pub fn request(&mut self, origin: Id, path: String, on_broadcast: OnStatus) -> Result<Id, Error> {
		let origin = self.active.get_mut(origin).ok_or(Error::OriginNotFound)?;
		let consumer = origin.consume();
		let channel = oneshot::channel();

		let entry = TaskEntry {
			close: Some(channel.0),
			callback: on_broadcast,
		};
		let id = self.consume_task.insert(Some(entry))?;

		tokio::spawn(async move {
			let res = Self::run_request(on_broadcast, consumer, path, channel.1).await;

			// Deliver one final terminal callback (code <= 0), then drop the entry.
			// Pull it out from under the lock so the callback never runs while held.
			let entry = State::lock().origin.consume_task.remove(id).flatten();
			if let Some(entry) = entry {
				entry.callback.call(res);
			}
		});

		Ok(id)
	}

	async fn run_request(
		callback: OnStatus,
		consumer: moq_net::origin::Consumer,
		path: String,
		mut close: oneshot::Receiver<()>,
	) -> Result<(), Error> {
		// Resolves to an error when no broadcast is reachable by exact path.
		let pending = consumer.request_broadcast(path.as_str());

		// `biased` so a pending close always wins over a ready broadcast.
		let broadcast = tokio::select! {
			biased;
			_ = &mut close => return Ok(()),
			res = pending => res?,
		};

		// Hold the lock only to buffer the broadcast; release it before the callback.
		let broadcast_id = State::lock().consume.start(broadcast, Some(consumer))?;
		callback.call(broadcast_id);
		Ok(())
	}

	pub fn consume_announced_close(&mut self, task: Id) -> Result<(), Error> {
		// Signal shutdown; the task delivers a final callback and removes itself.
		self.consume_task
			.get_mut(task)
			.and_then(|entry| entry.as_mut())
			.ok_or(Error::NotFound)?
			.close
			.take()
			.ok_or(Error::NotFound)?;
		Ok(())
	}

	/// Create a live broadcast at `path` on an origin, announcing the exact path.
	///
	/// Errors with [`Error::Moq`] if the path is outside the origin's scope.
	#[allow(clippy::type_complexity)]
	pub fn publish<P: moq_net::AsPath>(
		&self,
		origin: Id,
		path: P,
	) -> Result<
		(
			moq_net::broadcast::Producer,
			moq_net::origin::Producer,
			moq_net::PathOwned,
			moq_net::origin::Announcement,
		),
		Error,
	> {
		let origin = self.active.get(origin).ok_or(Error::OriginNotFound)?;
		let path = path.as_path().to_owned();
		let (broadcast, announcement) = origin.publish_broadcast(&path)?;
		Ok((broadcast, origin.clone(), path, announcement))
	}

	pub fn close(&mut self, origin: Id) -> Result<(), Error> {
		self.active.remove(origin).ok_or(Error::OriginNotFound)?;
		Ok(())
	}
}
