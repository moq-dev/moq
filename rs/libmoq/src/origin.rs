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

	/// Served routes from [Self::dynamic], retracted when the handle is closed.
	dynamic: NonZeroSlab<Option<DynamicEntry>>,

	/// Broadcast requests delivered to a dynamic handler, freed after accept/abort.
	broadcast_request: NonZeroSlab<Option<moq_net::origin::Request>>,
}

struct DynamicEntry {
	inner: Option<moq_net::origin::Dynamic>,
	close: Option<oneshot::Sender<()>>,
	callback: OnStatus,
}

impl Origin {
	pub fn create(&mut self) -> Result<Id, Error> {
		// Every FFI entry point runs inside `RUNTIME.enter()`, so the driver
		// lands on the dedicated libmoq runtime.
		self.active.insert(moq_tokio::origin::spawn(moq_net::Hop::random()))
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
			let announced_id = State::lock().origin.announced.insert((
				update
					.pattern
					.as_prefix()
					.unwrap_or_else(|| update.pattern.as_str())
					.to_owned(),
				update.active,
			))?;
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
		// `routed_broadcast` rides out the churn between a route covering the path
		// and the path actually resolving (failover, an advertise-only announce
		// racing its handler). `biased` so a pending close always wins.
		let broadcast = tokio::select! {
			biased;
			_ = &mut close => return Ok(()),
			resolved = consumer.routed_broadcast(path.as_str()) => match resolved {
				Ok(broadcast) => broadcast,
				// An unreachable path and a closed origin both mean no broadcast
				// can ever arrive here.
				Err(moq_net::Error::Unauthorized | moq_net::Error::Closed) => {
					return Err(Error::BroadcastNotFound);
				}
				Err(err) => return Err(err.into()),
			},
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

	/// Create an unadvertised broadcast at `path` on an origin.
	///
	/// Errors with [`Error::Moq`] if the path is outside the origin's scope.
	pub fn create_broadcast<P: moq_net::AsPath>(
		&self,
		origin: Id,
		path: P,
	) -> Result<moq_net::broadcast::Producer, Error> {
		let origin = self.active.get(origin).ok_or(Error::OriginNotFound)?;
		Ok(origin.create_broadcast(path)?)
	}

	/// Advertise `pattern` and serve requests beneath it, delivering each as a
	/// broadcast-request handle via `on_request`.
	pub fn dynamic(
		&mut self,
		origin: Id,
		pattern: moq_net::Pattern,
		route: moq_net::origin::Route,
		on_request: OnStatus,
	) -> Result<Id, Error> {
		let origin = self.active.get(origin).ok_or(Error::OriginNotFound)?;
		let inner = origin.dynamic(pattern, route)?;
		let channel = oneshot::channel();
		let id = self.dynamic.insert(Some(DynamicEntry {
			inner: Some(inner),
			close: Some(channel.0),
			callback: on_request,
		}))?;

		tokio::spawn(async move {
			let res = Self::run_dynamic(id, channel.1).await;
			let entry = State::lock().origin.dynamic.remove(id).flatten();
			if let Some(entry) = entry {
				entry.callback.call(res);
			}
		});

		Ok(id)
	}

	async fn run_dynamic(id: Id, mut close: oneshot::Receiver<()>) -> Result<(), Error> {
		loop {
			let request = tokio::select! {
				biased;
				_ = &mut close => return Ok(()),
				res = kio::wait(|waiter| {
					let state = State::lock();
					match state.origin.dynamic.get(id).and_then(|entry| entry.as_ref()).and_then(|entry| entry.inner.as_ref()) {
						Some(dynamic) => dynamic.poll_requested_broadcast(waiter),
						None => std::task::Poll::Ready(Err(moq_net::Error::Closed)),
					}
				}) => match res {
					Ok(request) => request,
					Err(moq_net::Error::Closed) => return Ok(()),
					Err(err) => return Err(err.into()),
				},
			};

			let request_id = State::lock().origin.broadcast_request.insert(Some(request))?;
			let callback = State::lock()
				.origin
				.dynamic
				.get(id)
				.and_then(|entry| entry.as_ref())
				.map(|entry| entry.callback);
			let Some(callback) = callback else {
				return Ok(());
			};
			callback.call(request_id);
		}
	}

	pub fn dynamic_update(&self, dynamic: Id, route: moq_net::origin::Route) -> Result<(), Error> {
		let dynamic = self
			.dynamic
			.get(dynamic)
			.and_then(|entry| entry.as_ref())
			.and_then(|entry| entry.inner.as_ref())
			.ok_or(Error::NotFound)?;
		Ok(dynamic.update(route)?)
	}

	pub fn dynamic_close(&mut self, dynamic: Id) -> Result<(), Error> {
		let entry = self
			.dynamic
			.get_mut(dynamic)
			.and_then(|entry| entry.as_mut())
			.ok_or(Error::NotFound)?;
		let inner = entry.inner.take();
		entry.close.take();
		drop(inner);
		Ok(())
	}

	pub fn broadcast_request_path(&self, request: Id, dst: &mut crate::moq_string) -> Result<(), Error> {
		let request = self
			.broadcast_request
			.get(request)
			.and_then(|slot| slot.as_ref())
			.ok_or(Error::NotFound)?;
		let path = request.path();
		*dst = crate::moq_string {
			data: path.as_str().as_ptr().cast::<std::ffi::c_char>(),
			len: path.as_str().len(),
		};
		Ok(())
	}

	pub fn broadcast_request_take(&mut self, request: Id) -> Result<moq_net::origin::Request, Error> {
		self.broadcast_request.remove(request).flatten().ok_or(Error::NotFound)
	}

	pub fn close(&mut self, origin: Id) -> Result<(), Error> {
		self.active.remove(origin).ok_or(Error::OriginNotFound)?;
		Ok(())
	}
}
