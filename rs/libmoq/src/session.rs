use std::sync::Arc;

use tokio::sync::oneshot;
use url::Url;

use crate::{ffi, Error, Id, NonZeroSlab, State};

#[derive(Default)]
pub struct Session {
	/// Session task cancellation channels.
	task: NonZeroSlab<oneshot::Sender<()>>,
}

impl Session {
	pub fn connect(
		&mut self,
		url: Url,
		publish: Option<moq_lite::OriginConsumer>,
		consume: Option<moq_lite::OriginProducer>,
		mut callback: ffi::OnStatus,
	) -> Result<Id, Error> {
		// Used just to notify when the session is removed from the map.
		let closed = oneshot::channel();

		let id = self.task.insert(closed.0);
		tokio::spawn(async move {
			let res = tokio::select! {
				// No more receiver, which means [session_close] was called.
				_ = closed.1 => Err(Error::Closed),
				// The connection failed.
				res = Self::connect_run(url, publish, consume, &mut callback) => res,
			};
			callback.call(res);

			// Make sure we clean up the task on exit.
			State::lock().session.task.remove(id);
		});

		Ok(id)
	}

	async fn connect_run(
		url: Url,
		publish: Option<moq_lite::OriginConsumer>,
		consume: Option<moq_lite::OriginProducer>,
		callback: &mut ffi::OnStatus,
	) -> Result<(), Error> {
		let config = moq_native::ClientConfig::default();
		let client = config.init().map_err(|err| Error::Connect(Arc::new(err)))?;
		let session = client
			.connect(url, publish, consume)
			.await
			.map_err(|err| Error::Connect(Arc::new(err)))?;
		callback.call(());

		session.closed().await?;
		Ok(())
	}

	pub fn close(&mut self, id: Id) -> Result<(), Error> {
		self.task.remove(id).ok_or(Error::NotFound)?;
		Ok(())
	}
}
