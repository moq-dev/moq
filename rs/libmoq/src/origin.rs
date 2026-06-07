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
	active: NonZeroSlab<moq_net::OriginProducer>,

	/// Announcement guards from `publish`. Removing an entry (via `unpublish`) drops the
	/// guard, which unannounces the broadcast.
	published: NonZeroSlab<moq_net::OriginPublish>,

	/// Broadcast announcement information (path, active status).
	announced: NonZeroSlab<(String, bool)>,

	/// Announcement listener tasks. Close signals shutdown; the task delivers a final callback, then removes itself.
	announced_task: NonZeroSlab<Option<TaskEntry>>,

	/// Pending consume-until-announced tasks. Close signals shutdown; the task delivers a final callback, then removes itself.
	consume_task: NonZeroSlab<Option<TaskEntry>>,
}

impl Origin {
	pub fn create(&mut self) -> Result<Id, Error> {
		self.active.insert(moq_net::Origin::random().produce())
	}

	pub fn get(&self, id: Id) -> Result<&moq_net::OriginProducer, Error> {
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
		mut consumer: moq_net::AnnounceConsumer,
		mut close: oneshot::Receiver<()>,
	) -> Result<(), Error> {
		loop {
			// `biased` so a pending close always wins over a ready announcement.
			let (path, event) = tokio::select! {
				biased;
				_ = &mut close => return Ok(()),
				next = consumer.next() => match next {
					Some(announced) => announced,
					None => return Ok(()),
				},
			};

			// Hold the lock only to buffer the announcement; release it before the callback.
			// Active and Restart both carry a broadcast; Ended does not.
			let announced_id = State::lock()
				.origin
				.announced
				.insert((path.to_string(), event.broadcast().is_some()))?;
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

	pub fn consume<P: moq_net::AsPath>(&mut self, origin: Id, path: P) -> Result<moq_net::BroadcastConsumer, Error> {
		let origin = self.active.get_mut(origin).ok_or(Error::OriginNotFound)?;
		// Synchronous lookup races announcement gossip; consume() is a cheap handle over this
		// origin's tree. Use `consume_announced` to wait for the announcement instead.
		origin.consume().get_broadcast(path).ok_or(Error::BroadcastNotFound)
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
		consumer: moq_net::OriginConsumer,
		path: String,
		mut close: oneshot::Receiver<()>,
	) -> Result<(), Error> {
		// `biased` so a pending close always wins over a ready announcement.
		let broadcast = tokio::select! {
			biased;
			_ = &mut close => return Ok(()),
			found = consumer.announced_broadcast(path.as_str()) => found.ok_or(Error::BroadcastNotFound)?,
		};

		// Hold the lock only to buffer the broadcast; release it before the callback.
		let broadcast_id = State::lock().consume.start(broadcast)?;
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

	/// Announce `broadcast` under `path`, returning a publish handle. The announcement stays
	/// live until [`Self::unpublish`] is called with that handle (independent of the broadcast's
	/// own lifetime). Errors with [`Error::Moq`] if the path is outside the origin's scope.
	pub fn publish<P: moq_net::AsPath>(
		&mut self,
		origin: Id,
		path: P,
		broadcast: moq_net::BroadcastConsumer,
	) -> Result<Id, Error> {
		let origin = self.active.get(origin).ok_or(Error::OriginNotFound)?;
		let publish = origin.publish_broadcast(path, broadcast)?;
		self.published.insert(publish)
	}

	/// Drop a publish handle from [`Self::publish`], unannouncing the broadcast.
	pub fn unpublish(&mut self, publish: Id) -> Result<(), Error> {
		// Dropping the removed guard is what unannounces the broadcast.
		let publish = self.published.remove(publish).ok_or(Error::BroadcastNotFound)?;
		drop(publish);
		Ok(())
	}

	pub fn close(&mut self, origin: Id) -> Result<(), Error> {
		self.active.remove(origin).ok_or(Error::OriginNotFound)?;
		Ok(())
	}
}
