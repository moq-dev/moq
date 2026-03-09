use tokio::sync::oneshot;

use crate::ffi::OnStatus;
use crate::{Error, Id, NonZeroSlab, State};

/// A spawned task entry: close sender to signal shutdown, callback to deliver status.
/// Close revokes the callback by taking the entire entry.
struct TaskEntry {
	#[allow(dead_code)] // Dropping the sender signals the receiver.
	close: oneshot::Sender<()>,
	callback: OnStatus,
}

/// Global state managing all active resources.
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
	pub fn create(&mut self) -> Id {
		self.active.insert(moq_lite::OriginProducer::default())
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
		let id = self.announced_task.insert(Some(entry));

		tokio::spawn(async move {
			let res = tokio::select! {
				res = Self::run_announced(id, consumer) => res,
				_ = channel.1 => Ok(()),
			};

			// The lock is dropped before the callback is invoked.
			if let Some(mut entry) = State::lock().origin.announced_task.remove(id).flatten() {
				entry.callback.call(res);
			}
		});

		Ok(id)
	}

	async fn run_announced(task_id: Id, mut consumer: moq_lite::OriginConsumer) -> Result<(), Error> {
		while let Some((path, broadcast)) = consumer.announced().await {
			let mut state = State::lock();
			let origin = &mut state.origin;

			// Stop if the callback was revoked by close.
			let Some(Some(entry)) = origin.announced_task.get_mut(task_id) else {
				return Ok(());
			};

			let announced_id = origin.announced.insert((path.to_string(), broadcast.is_some()));
			entry.callback.call(announced_id);
		}

		Ok(())
	}

	/// Returns announcement info as owned Rust values.
	pub fn announced_info_owned(&self, announced: Id) -> Result<(String, bool), Error> {
		let announced = self.announced.get(announced).ok_or(Error::AnnouncementNotFound)?;
		Ok((announced.0.clone(), announced.1))
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
