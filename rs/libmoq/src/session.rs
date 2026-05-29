use std::sync::Arc;

use anyhow::Context;
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
		publish: Option<moq_net::OriginConsumer>,
		consume: Option<moq_net::OriginProducer>,
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

			// Snapshot the callback so the lock is released before invoking user code.
			let callback = State::lock()
				.session
				.task
				.remove(id)
				.flatten()
				.map(|entry| entry.callback);
			if let Some(callback) = callback {
				callback.call(res);
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
		task_id: Id,
		url: Url,
		publish: Option<moq_net::OriginConsumer>,
		consume: Option<moq_net::OriginProducer>,
	) -> Result<(), Error> {
		let reconnect = moq_native::ClientConfig::default()
			.init()
			.map_err(|err| Error::Connect(Arc::new(err)))?
			.with_publish(publish)
			.with_consume(consume)
			.reconnect(url);

		// report() runs until the reconnect loop gives up; map its terminal error to Connect.
		Self::report(task_id, reconnect)
			.await
			.map_err(|err| Error::Connect(Arc::new(err)))
	}

	/// Forward connection epochs to the status callback until the reconnect loop stops.
	///
	/// Returns the terminal error via `?`. Disconnects aren't reported: a separate change reserves
	/// status 0 for "closed".
	async fn report(task_id: Id, mut reconnect: moq_native::Reconnect) -> anyhow::Result<()> {
		let mut connects: u64 = 0;
		loop {
			if let moq_native::Status::Connected = reconnect.status().await? {
				connects += 1;
				// Positive status carries the connection epoch, so callers can tell a
				// reconnect (>1) from the first connect (1).
				let code = i32::try_from(connects).context("connection epoch exceeded i32::MAX")?;
				Self::notify(task_id, code);
			}
		}
	}

	/// Invoke a session's status callback unless it has been revoked.
	///
	/// Copies the callback out before releasing the lock, so the C callback never runs while
	/// the global state is held.
	fn notify(task_id: Id, code: i32) {
		let callback = State::lock()
			.session
			.task
			.get(task_id)
			.and_then(|entry| entry.as_ref())
			.map(|entry| entry.callback);

		if let Some(callback) = callback {
			callback.call(code);
		}
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
