use std::sync::Arc;

use tokio::sync::oneshot;
use url::Url;

use crate::{Error, Id, NonZeroSlab, State, ffi};

/// A spawned task entry: `close` signals shutdown, `callback` delivers status.
///
/// `close` is an `Option` so `close()` can drop just the sender without
/// removing the entry. The task delivers one final terminal callback and then
/// removes itself, so `user_data` stays valid until that callback fires.
struct TaskEntry {
	close: Option<oneshot::Sender<()>>,
	callback: ffi::OnStatus,
}

#[derive(Default)]
pub struct Session {
	/// Session tasks. Close signals shutdown; the task delivers a final callback, then removes itself.
	task: NonZeroSlab<Option<TaskEntry>>,
}

impl Session {
	pub fn connect(
		&mut self,
		url: Url,
		publish: Option<moq_net::OriginConsumer>,
		consume: Option<moq_net::OriginProducer>,
		callback: ffi::OnStatus,
	) -> Result<Id, Error> {
		let closed = oneshot::channel();

		let entry = TaskEntry {
			close: Some(closed.0),
			callback,
		};
		let id = self.task.insert(Some(entry))?;

		tokio::spawn(async move {
			let res = tokio::select! {
				// close() requested: a clean shutdown delivers a terminal 0.
				_ = closed.1 => Ok(()),
				res = Self::connect_run(callback, url, publish, consume) => res,
			};

			// Deliver one final terminal callback (0 = closed, < 0 = error), then
			// drop the entry. Pull it out from under the lock so the callback never
			// runs while held.
			let entry = State::lock().session.task.remove(id).flatten();
			if let Some(entry) = entry {
				entry.callback.call(res);
			}
		});

		Ok(id)
	}

	async fn connect_run(
		callback: ffi::OnStatus,
		url: Url,
		publish: Option<moq_net::OriginConsumer>,
		consume: Option<moq_net::OriginProducer>,
	) -> Result<(), Error> {
		let client = moq_native::ClientConfig::default()
			.init()
			.map_err(|err| Error::Connect(Arc::new(err)))?;

		let session = client
			.with_publish(publish)
			.with_consume(consume)
			.connect(url)
			.await
			.map_err(|err| Error::Connect(Arc::new(err)))?;

		// Connected: positive sentinel 1. (0 is reserved for a clean close.)
		callback.call(1i32);

		session.closed().await?;
		Ok(())
	}

	pub fn close(&mut self, id: Id) -> Result<(), Error> {
		// Signal shutdown; the task delivers a final callback and removes itself.
		self.task
			.get_mut(id)
			.and_then(|entry| entry.as_mut())
			.ok_or(Error::SessionNotFound)?
			.close
			.take()
			.ok_or(Error::SessionNotFound)?;
		Ok(())
	}
}
