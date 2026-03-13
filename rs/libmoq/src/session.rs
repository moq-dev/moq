use std::sync::Arc;

use tokio::sync::oneshot;
use url::Url;

use crate::{Error, Id, NonZeroSlab, State, ffi};

/// A spawned task entry: close sender to signal shutdown, callback to deliver status.
struct TaskEntry {
	#[allow(dead_code)] // Dropping the sender signals the receiver.
	close: oneshot::Sender<()>,
	callback: ffi::OnStatus,
}

#[derive(Default)]
pub struct Session {
	/// Session tasks. Close takes the entry to revoke the callback.
	task: NonZeroSlab<Option<TaskEntry>>,
}

impl Session {
	pub fn connect(
		&mut self,
		url: Url,
		publish: Option<moq_lite::OriginConsumer>,
		consume: Option<moq_lite::OriginProducer>,
		callback: ffi::OnStatus,
	) -> Result<Id, Error> {
		let closed = oneshot::channel();

		let entry = TaskEntry {
			close: closed.0,
			callback,
		};
		let id = self.task.insert(Some(entry))?;

		tokio::spawn(async move {
			let res = tokio::select! {
				_ = closed.1 => Err(Error::Closed),
				res = Self::connect_run(id, url, publish, consume) => res,
			};

			// The lock is dropped before the callback is invoked.
			if let Some(entry) = State::lock().session.task.remove(id).flatten() {
				entry.callback.call(res);
			}
		});

		Ok(id)
	}

	async fn connect_run(
		task_id: Id,
		url: Url,
		publish: Option<moq_lite::OriginConsumer>,
		consume: Option<moq_lite::OriginProducer>,
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

		// "Connected" callback — copy from slab if not revoked.
		if let Some(Some(entry)) = State::lock().session.task.get(task_id) {
			entry.callback.call(());
		}

		session.closed().await?;
		Ok(())
	}

	pub fn close(&mut self, id: Id) -> Result<(), Error> {
		// Take the entire entry: drops the sender (signals shutdown) and revokes the callback.
		self.task
			.get_mut(id)
			.ok_or(Error::SessionNotFound)?
			.take()
			.ok_or(Error::SessionNotFound)?;
		Ok(())
	}
}
