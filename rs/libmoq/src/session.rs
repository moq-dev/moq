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

			// The lock is dropped before the callback is invoked.
			if let Some(entry) = State::lock().session.task.remove(id).flatten() {
				entry.callback.call(res);
			}
		});

		Ok(id)
	}

	/// Connect and stay connected, reconnecting with exponential backoff if the session drops.
	///
	/// Reports each transition through the status callback: a positive connection epoch on every
	/// (re)connect, 0 on each transient disconnect, and a negative code only when reconnection
	/// permanently gives up (the backoff timeout is exceeded), which is terminal.
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

		let mut connects = 0;
		let mut disconnects = 0;

		loop {
			tokio::select! {
				res = reconnect.closed() => return res.map_err(|err| Error::Connect(Arc::new(err))),
				epoch = reconnect.connected(connects) => {
					connects = epoch;
					// Positive status carries the connection epoch, so callers can tell a
					// reconnect (>1) from the first connect (1).
					Self::notify(task_id, i32::try_from(epoch).unwrap_or(i32::MAX));
				}
				epoch = reconnect.disconnected(disconnects) => {
					disconnects = epoch;
					// Status 0: transiently disconnected, reconnect in progress.
					Self::notify(task_id, 0);
				}
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
