use std::ffi::c_char;
use tokio::sync::oneshot;

use crate::ffi::OnStatus;
use crate::task::Task;
use crate::{Error, Id, NonZeroSlab, State, moq_announce_event, moq_announce_kind, moq_route_info, moq_string};

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

	/// Routes created on an origin, retracted when freed.
	routes: NonZeroSlab<RouteEntry>,

	/// Request deliveries of prefix routes. Freeing the route signals shutdown; the task
	/// delivers a final callback, then removes itself.
	route_requests: NonZeroSlab<Task>,

	/// Broadcast requests delivered to a prefix route's callback, freed after serve/route/reject.
	broadcast_request: NonZeroSlab<(moq_net::origin::Request, crate::ffi::Background)>,
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
	/// Owns the strings `capture_views` borrows; never read directly.
	_captures: Vec<String>,
	capture_views: Vec<moq_string>,
	kind: moq_announce_kind,
	hops: Vec<u64>,
	cost: u64,
	/// The route's instance.
	instance: String,
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
		let captures: Vec<_> = update
			.captures
			.expect("bindings only list full announcement matches")
			.into_iter()
			.map(|capture| capture.to_string())
			.collect();
		let capture_views = captures
			.iter()
			.map(|capture| moq_string {
				data: capture.as_ptr().cast(),
				len: capture.len(),
			})
			.collect();

		Self {
			prefix: update.prefix.to_string(),
			_captures: captures,
			capture_views,
			kind,
			hops: update.route.hops().iter().map(|hop| hop.id()).collect(),
			cost: update.route.cost().value(),
			instance: update.route.instance().to_string(),
			predecessor,
		}
	}
}

impl State {
	/// Store an exact route and the fill handle of the broadcast it owns, returning the route.
	pub fn exact_route(
		&mut self,
		(route, driver): (moq_net::origin::Route, crate::ffi::Background),
	) -> Result<Id, Error> {
		let broadcast = self.publish.create((route.broadcast().clone(), driver.clone()))?;
		self.origin.route_insert(route, driver, broadcast).inspect_err(|_| {
			self.publish.discard(broadcast);
		})
	}
}

struct RouteEntry {
	inner: moq_net::origin::Route,
	driver: crate::ffi::Background,
	/// Minted at creation and kept for the route's lifetime, borrowed by [Origin::route_instance].
	instance: String,
	/// An exact route's broadcast fill handle, or a prefix route's request delivery task.
	kind: RouteKind,
}

enum RouteKind {
	Exact { broadcast: Id },
	Prefix { requests: Id },
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
			kind: announced.kind,
			route: moq_route_info {
				hops: announced.hops.as_ptr(),
				hops_len: announced.hops.len(),
				cost: announced.cost,
				instance: moq_string {
					data: announced.instance.as_ptr().cast::<c_char>(),
					len: announced.instance.len(),
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
		instance: Option<moq_net::Instance>,
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
					.watch(Self::run_follow(on_broadcast, consumer, path, instance, close))
					.await
			},
		)
	}

	async fn run_follow(
		callback: OnStatus,
		consumer: Consumer,
		path: String,
		instance: Option<moq_net::Instance>,
		mut close: oneshot::Receiver<()>,
	) -> Result<(), Error> {
		// An unreachable path and a closed origin both mean no broadcast can ever arrive here.
		let mut announced = match consumer.follow(path.as_str(), instance) {
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
		instance: Option<moq_net::Instance>,
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
					.watch(Self::run_request(on_broadcast, consumer, path, instance, close))
					.await
			},
		)
	}

	async fn run_request(
		callback: OnStatus,
		consumer: Consumer,
		path: String,
		instance: Option<moq_net::Instance>,
		mut close: oneshot::Receiver<()>,
	) -> Result<(), Error> {
		// Resolves to an error when no announced route can serve the path.
		let pending = consumer.request(path.as_str(), instance);

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

	/// Create an exact route at `path`, returning the route and the broadcast it owns.
	///
	/// Errors with [`Error::Moq`] if the path is outside the origin's scope.
	pub fn route(
		&self,
		origin: Id,
		path: &str,
		options: moq_net::origin::RouteOptions,
	) -> Result<(moq_net::origin::Route, crate::ffi::Background), Error> {
		let origin = self.get(origin)?;
		let result = origin.producer.route(path, options);
		origin.driver.check()?;
		Ok((result?, origin.driver.clone()))
	}

	/// Store an exact route whose broadcast fill handle is `broadcast`.
	pub fn route_insert(
		&mut self,
		route: moq_net::origin::Route,
		driver: crate::ffi::Background,
		broadcast: Id,
	) -> Result<Id, Error> {
		self.routes.insert(RouteEntry {
			instance: route.instance().to_string(),
			inner: route,
			driver,
			kind: RouteKind::Exact { broadcast },
		})
	}

	/// Create a prefix route, delivering each request beneath it as a broadcast-request handle
	/// via `on_request`.
	pub fn route_prefix(
		&mut self,
		origin: Id,
		prefix: &str,
		options: moq_net::origin::RouteOptions,
		on_request: OnStatus,
	) -> Result<Id, Error> {
		let origin = self.get(origin)?;
		let driver = origin.driver.clone();
		let result = origin.producer.route_prefix(prefix, options);
		driver.check()?;
		let route = result?;
		let instance = route.instance().to_string();

		// The route first, so the delivery task finds it once it runs; it cannot lock the
		// state before this call returns.
		let (task, close) = Task::new(on_request);
		let requests = self.route_requests.insert(task)?;
		let id = match self.routes.insert(RouteEntry {
			inner: route,
			driver: driver.clone(),
			instance,
			kind: RouteKind::Prefix { requests },
		}) {
			Ok(id) => id,
			Err(err) => {
				self.route_requests.remove(requests);
				return Err(err);
			}
		};

		tokio::spawn(async move {
			let res = crate::ffi::run(driver.watch(Self::run_requests(id, on_request, close, driver.clone()))).await;
			let entry = State::lock().origin.route_requests.remove(requests);
			if let Some(entry) = entry {
				entry.callback.call(res);
			}
		});

		Ok(id)
	}

	async fn run_requests(
		route: Id,
		callback: OnStatus,
		mut close: oneshot::Receiver<()>,
		driver: crate::ffi::Background,
	) -> Result<(), Error> {
		loop {
			let request = tokio::select! {
				biased;
				_ = &mut close => return Ok(()),
				request = kio::wait(|waiter| {
					let state = State::lock();
					match state.origin.routes.get(route) {
						Some(entry) => entry.inner.poll_requested(waiter),
						None => std::task::Poll::Ready(None),
					}
				}) => match request {
					Some(request) => request,
					// Retracted, or the origin closed.
					None => return Ok(()),
				},
			};

			// Hold the lock only to buffer the request; release it before the callback.
			let request_id = State::lock()
				.origin
				.broadcast_request
				.insert((request, driver.clone()))?;
			callback.call(request_id);
		}
	}

	fn route_entry(&self, route: Id) -> Result<&RouteEntry, Error> {
		let entry = self.routes.get(route).ok_or(Error::NotFound)?;
		entry.driver.check()?;
		Ok(entry)
	}

	/// Run `f` against the route, then check its origin is still healthy.
	pub fn route_with<R>(
		&self,
		route: Id,
		f: impl FnOnce(&moq_net::origin::Route) -> Result<R, moq_net::Error>,
	) -> Result<R, Error> {
		let entry = self.route_entry(route)?;
		let result = f(&entry.inner);
		entry.driver.check()?;
		Ok(result?)
	}

	/// An exact route's broadcast fill handle, or [`Error::BroadcastNotFound`] on a prefix route.
	pub fn route_broadcast(&self, route: Id) -> Result<Id, Error> {
		match self.route_entry(route)?.kind {
			RouteKind::Exact { broadcast } => Ok(broadcast),
			RouteKind::Prefix { .. } => Err(Error::BroadcastNotFound),
		}
	}

	pub fn route_instance(&self, route: Id, dst: &mut moq_string) -> Result<(), Error> {
		let entry = self.route_entry(route)?;
		*dst = moq_string {
			data: entry.instance.as_ptr().cast::<c_char>(),
			len: entry.instance.len(),
		};
		Ok(())
	}

	/// Release a route handle and stop a prefix route's request delivery, returning the route
	/// for the caller to retract by dropping it.
	pub fn route_take(&mut self, route: Id) -> Result<moq_net::origin::Route, Error> {
		let entry = self.routes.remove(route).ok_or(Error::NotFound)?;
		if let RouteKind::Prefix { requests } = entry.kind {
			// Already gone once the origin closed and the task delivered its terminal callback.
			let _ = self.route_requests.close(requests, Error::NotFound);
		}
		Ok(entry.inner)
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

	pub fn close(&mut self, origin: Id) -> Result<(), Error> {
		self.active.remove(origin).ok_or(Error::OriginNotFound)?;
		Ok(())
	}
}

#[cfg(test)]
mod supervision_tests {
	use super::*;
	use moq_net::origin::RouteOptions;

	#[tokio::test]
	async fn a_route_keeps_the_origin_failure_after_its_origin_handle_is_released() {
		let (producer, completion, trigger) = crate::ffi::panicking_origin();
		let mut state = State::new();
		let origin = state
			.origin
			.active
			.insert(Producer {
				producer,
				driver: completion.clone(),
			})
			.unwrap();
		let retained = state.origin.route(origin, "retained", RouteOptions::hidden()).unwrap();
		let route = state.exact_route(retained).unwrap();
		state.origin.close(origin).unwrap();
		*trigger
			.write()
			.unwrap_or_else(|_| unreachable!("test keeps its panic trigger open")) = true;
		assert!(matches!(completion.finished().await, Err(Error::Panic)));
		drop(completion);
		assert!(matches!(
			state.origin.route_with(route, |route| route.announce()),
			Err(Error::Panic)
		));
		let healthy = state.origin.create().unwrap();
		let healthy = state.origin.route(healthy, "healthy", RouteOptions::hidden()).unwrap();
		let route = state.exact_route(healthy).unwrap();
		state.origin.route_with(route, |route| route.announce()).unwrap();
	}
}
