use std::sync::Arc;

use anyhow::Context;
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

	/// Connect and stay connected, reconnecting with exponential backoff if the session drops.
	///
	/// Reports a positive connection epoch through the status callback on every (re)connect, and a
	/// negative code only when reconnection permanently gives up (the backoff timeout is exceeded),
	/// which is terminal.
	async fn connect_run(
		callback: ffi::OnStatus,
		url: Url,
		publish: Option<moq_net::OriginConsumer>,
		consume: Option<moq_net::OriginProducer>,
	) -> Result<(), Error> {
		let reconnect = moq_native::ClientConfig::default()
			.init()?
			.with_publish(publish)
			.with_consume(consume)
			.reconnect(url);

		// report() runs until the reconnect loop gives up; map its terminal error to Connect.
		Self::report(callback, reconnect)
			.await
			.map_err(|err| Error::Connect(Arc::new(err)))
	}

	/// Forward connection epochs to the status callback until the reconnect loop stops.
	///
	/// Returns the terminal error via `?`. Disconnects aren't reported: status 0 is reserved for a
	/// clean close (delivered as the terminal callback once the task ends).
	async fn report(callback: ffi::OnStatus, mut reconnect: moq_native::Reconnect) -> anyhow::Result<()> {
		let mut connects: u64 = 0;
		loop {
			if let moq_native::Status::Connected = reconnect.status().await? {
				connects += 1;
				// Positive status carries the connection epoch, so callers can tell a
				// reconnect (>1) from the first connect (1). No lock is held, so the C
				// callback is free to re-enter libmoq.
				let code = i32::try_from(connects).context("connection epoch exceeded i32::MAX")?;
				callback.call(code);
			}
		}
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
