use tokio::sync::oneshot;

use crate::ffi::OnStatus;
use crate::{Error, Id, NonZeroSlab, State};

#[cfg(feature = "c-api")]
use {crate::moq_announced, std::ffi::c_char};

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

	/// Announcement listener task cancellation channels.
	announced_task: NonZeroSlab<oneshot::Sender<()>>,
}

impl Origin {
	pub fn create(&mut self) -> Id {
		self.active.insert(moq_lite::OriginProducer::default())
	}

	pub fn get(&self, id: Id) -> Result<&moq_lite::OriginProducer, Error> {
		self.active.get(id).ok_or(Error::OriginNotFound)
	}

	pub fn announced(&mut self, origin: Id, mut on_announce: OnStatus) -> Result<Id, Error> {
		let origin = self.active.get_mut(origin).ok_or(Error::OriginNotFound)?;
		let consumer = origin.consume();
		let channel = oneshot::channel();

		tokio::spawn(async move {
			tokio::select! {
				res = Self::run_announced(consumer, &mut on_announce) => on_announce.call(res),
				_ = channel.1 => (),
			};

			State::lock().origin.announced_task.remove(id);
		});

		let id = self.announced_task.insert(channel.0);
		Ok(id)
	}

	async fn run_announced(mut consumer: moq_lite::OriginConsumer, on_announce: &mut OnStatus) -> Result<(), Error> {
		while let Some((path, broadcast)) = consumer.announced().await {
			let id = State::lock()
				.origin
				.announced
				.insert((path.to_string(), broadcast.is_some()));
			on_announce.call(id);
		}

		Ok(())
	}

	#[cfg(feature = "c-api")]
	pub fn announced_info(&self, announced: Id, dst: &mut moq_announced) -> Result<(), Error> {
		let announced = self.announced.get(announced).ok_or(Error::AnnouncementNotFound)?;
		*dst = moq_announced {
			path: announced.0.as_str().as_ptr() as *const c_char,
			path_len: announced.0.len(),
			active: announced.1,
		};
		Ok(())
	}

	/// Returns announcement info as owned Rust values (no C struct needed).
	#[cfg(feature = "uniffi-api")]
	pub fn announced_info_owned(&self, announced: Id) -> Result<(String, bool), Error> {
		let announced = self.announced.get(announced).ok_or(Error::AnnouncementNotFound)?;
		Ok((announced.0.clone(), announced.1))
	}

	pub fn announced_close(&mut self, announced: Id) -> Result<(), Error> {
		self.announced_task
			.remove(announced)
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
