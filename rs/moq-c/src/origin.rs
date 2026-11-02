use std::ffi::c_char;
use tokio::sync::oneshot;

use crate::ffi::OnStatus;
use crate::task::Task;
use crate::{Error, Id, NonZeroSlab, State, moq_announce_event, moq_announce_kind, moq_route, moq_string};

/// Global state managing all active resources.
///
/// Stores all sessions, origins, broadcasts, tracks, and frames in slab allocators,
/// returning opaque IDs to C callers. Also manages async tasks via oneshot channels
/// for cancellation.
// TODO split this up into separate structs/mutexes
#[derive(Default)]
pub struct Origin {
	/// Active origin producers for publishing and consuming broadcasts.
	active: NonZeroSlab<Producer>,

	/// Delivered announce events, freed by the caller.
	announced: NonZeroSlab<AnnouncedRecord>,

	/// Announcement listener tasks. Close signals shutdown; the task delivers a final callback, then removes itself.
	announced_task: NonZeroSlab<Task>,

	/// Pending consume-until-announced tasks. Close signals shutdown; the task delivers a final callback, then removes itself.
	consume_task: NonZeroSlab<Task>,

	/// Served routes from [Self::dynamic], retracted when the handle is closed.
	dynamic: NonZeroSlab<DynamicEntry>,

	/// Broadcast requests delivered to a dynamic handler, freed after accept/reject.
	broadcast_request: NonZeroSlab<(moq_net::origin::Request, crate::ffi::Background)>,

	/// Claim answers being filled before their exact route is announced.
	prepared: NonZeroSlab<(moq_net::origin::Prepared, crate::ffi::Background, Id)>,
}

#[derive(Clone)]
pub(crate) struct Producer {
	pub producer: moq_net::origin::Producer,
	pub driver: crate::ffi::Background,
}

impl std::ops::Deref for Producer {
	type Target = moq_net::origin::Producer;
	fn deref(&self) -> &Self::Target {
		&self.producer
	}
}

#[derive(Clone)]
pub(crate) struct Consumer {
	pub inner: moq_net::origin::Consumer,
	pub driver: crate::ffi::Background,
}

impl std::ops::Deref for Consumer {
	type Target = moq_net::origin::Consumer;
	fn deref(&self) -> &Self::Target {
		&self.inner
	}
}

impl Producer {
	fn consume_owned(&self) -> Consumer {
		Consumer {
			inner: self.producer.consume(),
			driver: self.driver.clone(),
		}
	}
}

/// One announce event and the C string views borrowed from it.
struct AnnouncedRecord {
	prefix: String,
	captures: Option<Vec<String>>,
	capture_views: Vec<moq_string>,
	kind: moq_announce_kind,
	hops: Vec<u64>,
	cost: u64,
	/// Empty when the route carries no epoch.
	epoch: String,
	predecessor: String,
}

// The raw pointers only borrow immutable String allocations owned by this record.
// Moving the record does not move those allocations, and the record is never mutated.
unsafe impl Send for AnnouncedRecord {}

impl AnnouncedRecord {
	fn new(event: moq_net::announce::Event) -> Self {
		use moq_net::announce::Event;
		let (update, kind, predecessor) = match event {
			Event::Start(update) => (update, moq_announce_kind::MOQ_ANNOUNCE_KIND_START, String::new()),
			Event::Update(update) => (update, moq_announce_kind::MOQ_ANNOUNCE_KIND_UPDATE, String::new()),
			Event::End(update) => (update, moq_announce_kind::MOQ_ANNOUNCE_KIND_END, String::new()),
			Event::Restart(update) => (update, moq_announce_kind::MOQ_ANNOUNCE_KIND_RESTART, String::new()),
			Event::Takeover { announce, predecessor } => (
				announce,
				moq_announce_kind::MOQ_ANNOUNCE_KIND_TAKEOVER,
				predecessor.to_string(),
			),
		};
		let captures = update.captures.map(|captures| {
			captures
				.into_iter()
				.map(|capture| capture.to_string())
				.collect::<Vec<_>>()
		});
		let capture_views = captures
			.as_deref()
			.unwrap_or_default()
			.iter()
			.map(|capture| moq_string {
				data: capture.as_ptr().cast(),
				len: capture.len(),
			})
			.collect();

		Self {
			prefix: update.prefix.to_string(),
			captures,
			capture_views,
			kind,
			hops: update.route.hops.iter().map(|hop| hop.id()).collect(),
			cost: update.route.cost.value(),
			epoch: update.route.epoch.map(|epoch| epoch.to_string()).unwrap_or_default(),
			predecessor,
		}
	}
}

struct DynamicEntry {
	task: Task,
	inner: Option<moq_net::origin::Requests>,
}

impl Origin {
	pub fn create(&mut self) -> Result<Id, Error> {
		// Every FFI entry point runs inside `RUNTIME.enter()`, so the driver
		// lands on the dedicated moq-c runtime.
		let (producer, driver) = moq_net::origin::Producer::new(Default::default());
		self.active.insert(Producer {
			producer,
			driver: crate::ffi::Background::driver(driver),
		})
	}

	pub fn get(&self, id: Id) -> Result<&Producer, Error> {
		let origin = self.active.get(id).ok_or(Error::OriginNotFound)?;
		origin.driver.check()?;
		Ok(origin)
	}

	pub fn announced(
		&mut self,
		origin: Id,
		scope: moq_net::Pattern,
		hidden: bool,
		on_announce: OnStatus,
	) -> Result<Id, Error> {
		let origin = self.active.get(origin).ok_or(Error::OriginNotFound)?;
		origin.driver.check()?;
		let driver = origin.driver.clone();
		let consumer = origin
			.producer
			.consume()
			.scope("", &moq_net::Patterns::from(scope))?
			.with_hidden(hidden)
			.announced();
		self.announced_task.spawn(
			on_announce,
			|state| &mut state.origin.announced_task,
			|close| async move { driver.watch(Self::run_announced(on_announce, consumer, close)).await },
		)
	}

	async fn run_announced(
		callback: OnStatus,
		mut consumer: moq_net::announce::Consumer,
		mut close: oneshot::Receiver<()>,
	) -> Result<(), Error> {
		loop {
			// `biased` so a pending close always wins over a ready announcement.
			let event = tokio::select! {
				biased;
				_ = &mut close => return Ok(()),
				next = consumer.next() => match next {
					Some(event) => event,
					None => return Ok(()),
				},
			};

			// Hold the lock only to buffer the event; release it before the callback.
			let announced_id = State::lock().origin.announced.insert(AnnouncedRecord::new(event))?;
			callback.call(announced_id);
		}
	}

	pub fn announced_info(&self, announced: Id, dst: &mut moq_announce_event) -> Result<(), Error> {
		let announced = self.announced.get(announced).ok_or(Error::AnnouncementNotFound)?;
		*dst = moq_announce_event {
			prefix: announced.prefix.as_ptr().cast::<c_char>(),
			prefix_len: announced.prefix.len(),
			captures: announced.capture_views.as_ptr(),
			captures_len: announced.capture_views.len(),
			has_captures: announced.captures.is_some(),
			kind: announced.kind,
			route: moq_route {
				hops: announced.hops.as_ptr(),
				hops_len: announced.hops.len(),
				cost: announced.cost,
				epoch: moq_string {
					data: announced.epoch.as_ptr().cast::<c_char>(),
					len: announced.epoch.len(),
				},
			},
			predecessor: announced.predecessor.as_ptr().cast::<c_char>(),
			predecessor_len: announced.predecessor.len(),
		};
		Ok(())
	}

	/// Free a single announcement record delivered to an `on_announce` callback.
	///
	/// Each announce event allocates a record (read via [`Self::announced_info`]);
	/// the caller releases it here once done. This is per-record, distinct from
	/// [`Self::announced_close`], which stops the whole listener. Records are freed explicitly
	/// rather than on unannounce: an unannounce is its own delivered record, and auto-freeing
	/// the prior one would race a caller still reading it.
	pub fn announced_free(&mut self, announced: Id) -> Result<(), Error> {
		self.announced.remove(announced).ok_or(Error::AnnouncementNotFound)?;
		Ok(())
	}

	pub fn announced_close(&mut self, announced: Id) -> Result<(), Error> {
		self.announced_task.close(announced, Error::AnnouncementNotFound)
	}

	/// Follow the broadcast at `path`, delivering each publisher instance's handle via the callback.
	///
	/// The callback fires a broadcast handle (> 0) for each instance serving the path, until a
	/// single terminal code (`0` on close, negative on error). Returns a task handle for
	/// cancellation via [`Self::resolve_close`].
	pub fn follow(
		&mut self,
		origin: Id,
		path: String,
		epoch: Option<moq_net::Epoch>,
		on_broadcast: OnStatus,
	) -> Result<Id, Error> {
		let origin = self.active.get(origin).ok_or(Error::OriginNotFound)?;
		origin.driver.check()?;
		let driver = origin.driver.clone();
		let consumer = origin.consume_owned();
		self.consume_task.spawn(
			on_broadcast,
			|state| &mut state.origin.consume_task,
			|close| async move {
				driver
					.watch(Self::run_follow(on_broadcast, consumer, path, epoch, close))
					.await
			},
		)
	}

	async fn run_follow(
		callback: OnStatus,
		consumer: Consumer,
		path: String,
		epoch: Option<moq_net::Epoch>,
		mut close: oneshot::Receiver<()>,
	) -> Result<(), Error> {
		// An unreachable path and a closed origin both mean no broadcast can ever arrive here.
		let mut announced = match consumer.follow(path.as_str(), epoch) {
			Ok(announced) => announced,
			Err(moq_net::Error::Unauthorized) => return Err(Error::BroadcastNotFound),
			Err(err) => return Err(err.into()),
		};
		let mut delivered = false;

		// Deliver each publisher instance serving the path: the first, then each one that takes
		// over or returns. `biased` so a pending close always wins.
		loop {
			let broadcast = tokio::select! {
				biased;
				_ = &mut close => return Ok(()),
				available = announced.available() => match available {
					Ok(broadcast) => broadcast,
					// The origin closing ends the wait, cleanly once a handle was delivered.
					Err(moq_net::Error::Closed) if delivered => return Ok(()),
					Err(moq_net::Error::Closed) => return Err(Error::BroadcastNotFound),
					Err(err) => return Err(err.into()),
				},
			};

			// Hold the lock only to buffer the broadcast; release it before the callback.
			let broadcast_id = State::lock().consume.start(broadcast, Some(consumer.clone()))?;
			delivered = true;
			callback.call(broadcast_id);
		}
	}

	/// Request the broadcast at `path`, delivering its handle once it can be served.
	///
	/// Unlike [`Self::follow`] (waits indefinitely, delivering each publisher instance), this
	/// resolves one instance: the callback fires the broadcast handle (> 0) once served, then a
	/// terminal `0`; or a single terminal code (`0` on close, negative on error) if it can't be
	/// served. Returns a task handle for cancellation.
	pub fn request(
		&mut self,
		origin: Id,
		path: String,
		epoch: Option<moq_net::Epoch>,
		on_broadcast: OnStatus,
	) -> Result<Id, Error> {
		let origin = self.active.get(origin).ok_or(Error::OriginNotFound)?;
		origin.driver.check()?;
		let driver = origin.driver.clone();
		let consumer = origin.consume_owned();
		self.consume_task.spawn(
			on_broadcast,
			|state| &mut state.origin.consume_task,
			|close| async move {
				driver
					.watch(Self::run_request(on_broadcast, consumer, path, epoch, close))
					.await
			},
		)
	}

	async fn run_request(
		callback: OnStatus,
		consumer: Consumer,
		path: String,
		epoch: Option<moq_net::Epoch>,
		mut close: oneshot::Receiver<()>,
	) -> Result<(), Error> {
		// Resolves to an error when no announced route can serve the path.
		let pending = consumer.request(path.as_str(), epoch);

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

	pub fn resolve_close(&mut self, task: Id) -> Result<(), Error> {
		self.consume_task.close(task, Error::NotFound)
	}

	/// Create an unannounced broadcast at `path` on an origin.
	///
	/// Errors with [`Error::Moq`] if the path is outside the origin's scope.
	pub fn create_broadcast<P: TryInto<moq_net::Path, Error: Into<moq_net::Error>>>(
		&self,
		origin: Id,
		path: P,
	) -> Result<(moq_net::origin::Publication, crate::ffi::Background), Error> {
		let origin = self.get(origin)?;
		let result = origin.producer.create_broadcast(path);
		origin.driver.check()?;
		Ok((result?, origin.driver.clone()))
	}

	/// Advertise `prefix` and serve requests beneath it, delivering each as a
	/// broadcast-request handle via `on_request`.
	pub fn dynamic(&mut self, origin: Id, prefix: &str, on_request: OnStatus) -> Result<Id, Error> {
		let origin = self.active.get(origin).ok_or(Error::OriginNotFound)?;
		origin.driver.check()?;
		let driver = origin.driver.clone();
		let result = origin.producer.requests(prefix);
		origin.driver.check()?;
		let inner = result?;
		let (task, close) = Task::new(on_request);
		let id = self.dynamic.insert(DynamicEntry {
			task,
			inner: Some(inner),
		})?;

		let request_driver = driver.clone();
		tokio::spawn(async move {
			let res = crate::ffi::run(driver.watch(Self::run_dynamic(id, close, request_driver))).await;
			let entry = State::lock().origin.dynamic.remove(id);
			if let Some(entry) = entry {
				entry.task.callback.call(res);
			}
		});

		Ok(id)
	}

	async fn run_dynamic(
		id: Id,
		mut close: oneshot::Receiver<()>,
		driver: crate::ffi::Background,
	) -> Result<(), Error> {
		loop {
			let request = tokio::select! {
				biased;
				_ = &mut close => return Ok(()),
				res = kio::wait(|waiter| {
					let state = State::lock();
					match state.origin.dynamic.get(id).and_then(|entry| entry.inner.as_ref()) {
						Some(dynamic) => dynamic.poll_requested_broadcast(waiter),
						None => std::task::Poll::Ready(Err(moq_net::Error::Closed)),
					}
				}) => match res {
					Ok(request) => request,
					Err(moq_net::Error::Closed) => return Ok(()),
					Err(err) => return Err(err.into()),
				},
			};

			let request_id = State::lock()
				.origin
				.broadcast_request
				.insert((request, driver.clone()))?;
			let callback = State::lock().origin.dynamic.get(id).map(|entry| entry.task.callback);
			let Some(callback) = callback else {
				return Ok(());
			};
			callback.call(request_id);
		}
	}

	pub fn dynamic_close(&mut self, dynamic: Id) -> Result<(), Error> {
		let entry = self.dynamic.get_mut(dynamic).ok_or(Error::NotFound)?;
		let inner = entry.inner.take().ok_or(Error::NotFound)?;
		entry.task.close(Error::NotFound)?;
		drop(inner);
		Ok(())
	}

	pub fn broadcast_request_path(&self, request: Id, dst: &mut crate::moq_string) -> Result<(), Error> {
		let request = self.broadcast_request.get(request).ok_or(Error::NotFound)?;
		let path = request.0.path();
		*dst = crate::moq_string {
			data: path.as_str().as_ptr().cast::<std::ffi::c_char>(),
			len: path.as_str().len(),
		};
		Ok(())
	}

	pub fn broadcast_request_take(
		&mut self,
		request: Id,
	) -> Result<(moq_net::origin::Request, crate::ffi::Background), Error> {
		self.broadcast_request.remove(request).ok_or(Error::NotFound)
	}

	pub fn prepared_insert(
		&mut self,
		prepared: moq_net::origin::Prepared,
		driver: crate::ffi::Background,
		broadcast: Id,
	) -> Result<Id, Error> {
		self.prepared.insert((prepared, driver, broadcast))
	}

	pub fn prepared_take(
		&mut self,
		prepared: Id,
	) -> Result<(moq_net::origin::Prepared, crate::ffi::Background, Id), Error> {
		self.prepared.remove(prepared).ok_or(Error::NotFound)
	}

	pub fn close(&mut self, origin: Id) -> Result<(), Error> {
		self.active.remove(origin).ok_or(Error::OriginNotFound)?;
		Ok(())
	}
}

#[cfg(test)]
mod supervision_tests {
	use super::*;

	#[tokio::test]
	async fn a_broadcast_keeps_the_origin_failure_after_its_origin_handle_is_released() {
		let (producer, completion, trigger) = crate::ffi::panicking_origin();
		let mut origins = Origin::default();
		let origin = origins
			.active
			.insert(Producer {
				producer,
				driver: completion.clone(),
			})
			.unwrap();
		let mut publish = crate::publish::Publish::default();
		let (publication, _broadcast) = publish
			.create_publication(origins.create_broadcast(origin, "retained").unwrap())
			.unwrap();
		origins.close(origin).unwrap();
		*trigger
			.write()
			.unwrap_or_else(|_| unreachable!("test keeps its panic trigger open")) = true;
		assert!(matches!(completion.finished().await, Err(Error::Panic)));
		drop(completion);
		assert!(matches!(
			publish.announce(publication, Default::default()),
			Err(Error::Panic)
		));
		let healthy = origins.create().unwrap();
		let (publication, _broadcast) = publish
			.create_publication(origins.create_broadcast(healthy, "healthy").unwrap())
			.unwrap();
		publish.announce(publication, Default::default()).unwrap();
	}
}
