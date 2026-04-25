use std::ffi::c_char;
use tokio::sync::oneshot;

use crate::ffi::OnStatus;
use crate::{Error, Id, NonZeroSlab, State, moq_announced};

/// A spawned task entry: close sender to signal shutdown, callback to deliver status.
struct TaskEntry {
	#[allow(dead_code)] // Dropping the sender signals the receiver.
	close: oneshot::Sender<()>,
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
	active: NonZeroSlab<moq_lite::OriginProducer>,

	/// Broadcast announcement information (path, active status).
	announced: NonZeroSlab<(String, bool)>,

	/// Announcement listener tasks. Close takes the entry to revoke the callback.
	announced_task: NonZeroSlab<Option<TaskEntry>>,
}

impl Origin {
	pub fn create(&mut self) -> Result<Id, Error> {
		self.active.insert(moq_lite::Origin::random().produce())
	}

	pub fn get(&self, id: Id) -> Result<&moq_lite::OriginProducer, Error> {
		self.active.get(id).ok_or(Error::OriginNotFound)
	}

	pub fn announced(&mut self, origin: Id, on_announce: OnStatus) -> Result<Id, Error> {
		let origin = self.active.get_mut(origin).ok_or(Error::OriginNotFound)?;
		let consumer = origin.consume();
		let channel = oneshot::channel();

		let entry = TaskEntry {
			close: channel.0,
			callback: on_announce,
		};
		let id = self.announced_task.insert(Some(entry))?;

		tokio::spawn(async move {
			let res = tokio::select! {
				res = Self::run_announced(id, consumer) => res,
				_ = channel.1 => Ok(()),
			};

			// The lock is dropped before the callback is invoked.
			if let Some(entry) = State::lock().origin.announced_task.remove(id).flatten() {
				entry.callback.call(res);
			}
		});

		Ok(id)
	}

	async fn run_announced(task_id: Id, mut consumer: moq_lite::OriginConsumer) -> Result<(), Error> {
		while let Some((path, broadcast)) = consumer.announced().await {
			let mut state = State::lock();

			// Stop if the callback was revoked by close.
			let Some(Some(entry)) = state.origin.announced_task.get(task_id) else {
				return Ok(());
			};
			let callback = entry.callback;

			let announced_id = state.origin.announced.insert((path.to_string(), broadcast.is_some()))?;
			drop(state);

			// The lock is dropped before the callback is invoked.
			callback.call(announced_id);
		}

		Ok(())
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
		// Take the entire entry: drops the sender (signals shutdown) and revokes the callback.
		self.announced_task
			.get_mut(announced)
			.ok_or(Error::AnnouncementNotFound)?
			.take()
			.ok_or(Error::AnnouncementNotFound)?;
		Ok(())
	}

	pub fn consume<P: moq_lite::AsPath>(&mut self, origin: Id, path: P) -> Result<moq_lite::BroadcastConsumer, Error> {
		let origin = self.active.get_mut(origin).ok_or(Error::OriginNotFound)?;
		// TODO: expose an async variant backed by `announced_broadcast` so FFI callers can wait
		// for gossip instead of racing it.
		#[allow(deprecated)]
		origin.consume().consume_broadcast(path).ok_or(Error::BroadcastNotFound)
	}

	pub fn publish<P: moq_lite::AsPath>(
		&mut self,
		origin: Id,
		path: P,
		broadcast: moq_lite::BroadcastConsumer,
	) -> Result<(), Error> {
		let origin = self.active.get_mut(origin).ok_or(Error::OriginNotFound)?;
		origin.publish_broadcast(path, broadcast);
		Ok(())
	}

	pub fn close(&mut self, origin: Id) -> Result<(), Error> {
		self.active.remove(origin).ok_or(Error::OriginNotFound)?;
		Ok(())
	}
}
