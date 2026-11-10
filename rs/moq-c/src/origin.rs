use std::ffi::c_char;
use tokio::sync::oneshot;

use crate::ffi::{OnFollow, OnStatus};
use crate::publish::Children;
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

	/// Pending requests. Close signals shutdown; the task delivers a final callback, then removes itself.
	consume_task: NonZeroSlab<Task>,

	/// Exact routes created on an origin, retracted when freed.
	routes: NonZeroSlab<RouteEntry>,

	/// Claims created on an origin, retracted when freed.
	claims: NonZeroSlab<ClaimEntry>,

	/// Follows. Close signals shutdown; the task delivers its end event, then removes itself.
	follow_task: NonZeroSlab<Task<OnFollow>>,

	/// Request deliveries of claims. Freeing the claim signals shutdown; the task delivers a
	/// final callback, then removes itself.
	claim_requests: NonZeroSlab<Task>,

	/// Broadcast requests delivered to a claim's callback, freed after serve/route/reject.
	broadcast_request: NonZeroSlab<(moq_net::origin::Request, crate::ffi::Background)>,
	/// Instance handles: parsed by the caller, reported by a route or claim, or owned by an
	/// announcement record.
	instances: NonZeroSlab<moq_net::Instance>,
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
	cost: u64,
	available: bool,
	/// The route's instance handle, freed with the record.
	instance: Id,
}

// The raw pointers only borrow immutable String allocations owned by this record.
// Moving the record does not move those allocations, and the record is never mutated.
unsafe impl Send for AnnouncedRecord {}

impl AnnouncedRecord {
	fn new(event: moq_net::announce::Event, instances: &mut NonZeroSlab<moq_net::Instance>) -> Result<Self, Error> {
		use moq_net::announce::Event;
		let (update, kind) = match event {
			Event::Present(update) => (update, moq_announce_kind::MOQ_ANNOUNCE_KIND_PRESENT),
			Event::Absent(update) => (update, moq_announce_kind::MOQ_ANNOUNCE_KIND_ABSENT),
			Event::Offline(update) => (update, moq_announce_kind::MOQ_ANNOUNCE_KIND_OFFLINE),
			Event::Unavailable(update) => (update, moq_announce_kind::MOQ_ANNOUNCE_KIND_UNAVAILABLE),
			Event::Available(update) => (update, moq_announce_kind::MOQ_ANNOUNCE_KIND_AVAILABLE),
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

		Ok(Self {
			prefix: update.prefix.to_string(),
			_captures: captures,
			capture_views,
			kind,
			cost: update.route.cost().value(),
			available: update.route.available(),
			instance: instances.insert(update.route.instance().clone())?,
		})
	}
}

impl State {
	/// Store an exact route and the fill handle of the broadcast it owns, returning the route.
	///
	/// The route is created hidden, so its catalog track is attached before `visible` announces
	/// it and a reader that finds it finds the catalog.
	pub fn exact_route(
		&mut self,
		(route, driver): (moq_net::origin::Route, crate::ffi::Background),
		visible: bool,
	) -> Result<Id, Error> {
		let broadcast = self.publish.create((route.broadcast().clone(), driver.clone()))?;
		self.publish
			.target(broadcast)
			.map(|target| target.children.clone())
			.and_then(|family| {
				if visible {
					route.announce()?;
				}
				self.origin.route_insert(route, driver, broadcast, family)
			})
			.inspect_err(|_| {
				self.publish.discard(broadcast);
			})
	}
}

struct RouteEntry {
	inner: moq_net::origin::Route,
	driver: crate::ffi::Background,
	/// The fill handle of the route's broadcast.
	broadcast: Id,
	/// Carried, not looked up, so a closed broadcast is told apart from a newer one reusing its id.
	family: Children,
}

struct ClaimEntry {
	inner: moq_net::origin::Claim,
	driver: crate::ffi::Background,
	/// The claim's request delivery task.
	requests: Id,
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
			let announced_id = State::lock().origin.announced_insert(event)?;
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
				cost: announced.cost,
				instance: announced.instance.into(),
				available: announced.available,
			},
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
		let record = self.announced.remove(announced).ok_or(Error::AnnouncementNotFound)?;
		self.instances.remove(record.instance);
		Ok(())
	}

	/// Buffer a delivered announce event, with a handle for its route's instance.
	fn announced_insert(&mut self, event: moq_net::announce::Event) -> Result<Id, Error> {
		let record = AnnouncedRecord::new(event, &mut self.instances)?;
		let instance = record.instance;
		self.announced.insert(record).inspect_err(|_| {
			self.instances.remove(instance);
		})
	}

	/// A new instance handle the caller owns.
	pub fn instance_insert(&mut self, instance: moq_net::Instance) -> Result<Id, Error> {
		self.instances.insert(instance)
	}

	/// The instance a handle names.
	pub fn instance(&self, instance: Id) -> Result<&moq_net::Instance, Error> {
		self.instances.get(instance).ok_or(Error::NotFound)
	}

	/// Release an instance handle the caller owns. An announcement's own is released with it.
	pub fn instance_free(&mut self, instance: Id) -> Result<(), Error> {
		self.instances.remove(instance).ok_or(Error::NotFound)?;
		Ok(())
	}

	pub fn announced_close(&mut self, announced: Id) -> Result<(), Error> {
		self.announced_task.close(announced, Error::AnnouncementNotFound)
	}

	/// Follow the broadcast at `path`, delivering each publisher instance's handle via the callback.
	///
	/// The callback reports each instance serving the path as online with its broadcast handle,
	/// the path going offline, and one end with `0` on close or a negative error. Returns a task
	/// handle for cancellation via [`Self::follow_close`].
	pub fn follow(
		&mut self,
		origin: Id,
		path: String,
		instance: Option<moq_net::Instance>,
		on_follow: OnFollow,
	) -> Result<Id, Error> {
		let origin = self.active.get(origin).ok_or(Error::OriginNotFound)?;
		origin.driver.check()?;
		let driver = origin.driver.clone();
		let consumer = origin.consume_owned();
		self.follow_task.spawn(
			on_follow,
			|state| &mut state.origin.follow_task,
			|close| async move {
				driver
					.watch(Self::run_follow(on_follow, consumer, path, instance, close))
					.await
			},
		)
	}

	pub fn follow_close(&mut self, task: Id) -> Result<(), Error> {
		self.follow_task.close(task, Error::NotFound)
	}

	async fn run_follow(
		callback: OnFollow,
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
					Ok(Some(broadcast)) => broadcast,
					Ok(None) => {
						callback.offline();
						continue;
					}
					// The origin closing ends the wait, cleanly once a handle was delivered.
					Err(moq_net::Error::Closed) if delivered => return Ok(()),
					Err(moq_net::Error::Closed) => return Err(Error::BroadcastNotFound),
					Err(err) => return Err(err.into()),
				},
			};

			// Hold the lock only to buffer the broadcast; release it before the callback.
			let broadcast_id = State::lock().consume.start(broadcast, Some(consumer.clone()))?;
			delivered = true;
			callback.online(broadcast_id);
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
		family: Children,
	) -> Result<Id, Error> {
		// Read only on request: reading a hidden job's instance binds it.
		self.routes.insert(RouteEntry {
			inner: route,
			driver,
			broadcast,
			family,
		})
	}

	/// Claim a prefix, delivering each request beneath it as a broadcast-request handle via
	/// `on_request`.
	pub fn claim(
		&mut self,
		origin: Id,
		prefix: &str,
		options: moq_net::origin::RouteOptions,
		on_request: OnStatus,
	) -> Result<Id, Error> {
		let origin = self.get(origin)?;
		let driver = origin.driver.clone();
		let result = origin.producer.claim(prefix, options);
		driver.check()?;
		let claim = result?;

		// The claim first, so the delivery task finds it once it runs; it cannot lock the
		// state before this call returns.
		let (task, close) = Task::new(on_request);
		let requests = self.claim_requests.insert(task)?;
		let id = match self.claims.insert(ClaimEntry {
			inner: claim,
			driver: driver.clone(),
			requests,
		}) {
			Ok(id) => id,
			Err(err) => {
				self.claim_requests.remove(requests);
				return Err(err);
			}
		};

		tokio::spawn(async move {
			let res = crate::ffi::run(driver.watch(Self::run_requests(id, on_request, close, driver.clone()))).await;
			let entry = State::lock().origin.claim_requests.remove(requests);
			if let Some(entry) = entry {
				entry.callback.call(res);
			}
		});

		Ok(id)
	}

	async fn run_requests(
		claim: Id,
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
					match state.origin.claims.get(claim) {
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

	/// The route's broadcast fill handle, until [State::publish_close] ends it.
	pub fn route_broadcast(&self, route: Id) -> Result<Id, Error> {
		let entry = self.route_entry(route)?;
		match entry.family.is_closed() {
			true => Err(Error::BroadcastNotFound),
			false => Ok(entry.broadcast),
		}
	}

	/// A new handle, the caller's, for the instance the route serves.
	pub fn route_instance(&mut self, route: Id) -> Result<Id, Error> {
		let entry = self.routes.get(route).ok_or(Error::NotFound)?;
		entry.driver.check()?;
		let instance = entry.inner.instance().clone();
		self.instances.insert(instance)
	}

	/// Release a route handle, returning the route for the caller to retract by dropping it.
	pub fn route_take(&mut self, route: Id) -> Result<moq_net::origin::Route, Error> {
		Ok(self.routes.remove(route).ok_or(Error::NotFound)?.inner)
	}

	/// Run `f` against the claim, then check its origin is still healthy.
	pub fn claim_with<R>(
		&self,
		claim: Id,
		f: impl FnOnce(&moq_net::origin::Claim) -> Result<R, moq_net::Error>,
	) -> Result<R, Error> {
		let entry = self.claims.get(claim).ok_or(Error::NotFound)?;
		entry.driver.check()?;
		let result = f(&entry.inner);
		entry.driver.check()?;
		Ok(result?)
	}

	/// A new handle, the caller's, for the instance the claim serves.
	pub fn claim_instance(&mut self, claim: Id) -> Result<Id, Error> {
		let entry = self.claims.get(claim).ok_or(Error::NotFound)?;
		entry.driver.check()?;
		let instance = entry.inner.instance().clone();
		self.instances.insert(instance)
	}

	/// Release a claim handle and stop its request delivery, returning the claim for the
	/// caller to retract by dropping it.
	pub fn claim_take(&mut self, claim: Id) -> Result<moq_net::origin::Claim, Error> {
		let entry = self.claims.remove(claim).ok_or(Error::NotFound)?;
		// Already gone once the origin closed and the task delivered its terminal callback.
		let _ = self.claim_requests.close(entry.requests, Error::NotFound);
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
		let route = state.exact_route(retained, false).unwrap();
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
		let route = state.exact_route(healthy, false).unwrap();
		state.origin.route_with(route, |route| route.announce()).unwrap();
	}
}
