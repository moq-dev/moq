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
	/// Reads live connection stats, reporting `None` while reconnecting.
	stats: moq_native::ConnectionStatsReader,
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
		publish: Option<moq_net::OriginProducer>,
		consume: Option<moq_net::OriginProducer>,
		callback: ffi::OnStatus,
	) -> Result<Id, Error> {
		let mut client = moq_native::ClientConfig::default().init()?;
		if let Some(publish) = &publish {
			client = client.with_publisher(publish);
		}
		if let Some(consume) = &consume {
			client = client.with_subscriber(consume.clone());
		}

		// Build the reconnect loop up front so we can grab a stats reader for it
		// before moving it into the spawned task.
		let reconnect = client.reconnect(url);
		let stats = reconnect.stats();

		let closed = oneshot::channel();
		let entry = TaskEntry {
			close: Some(closed.0),
			callback,
			stats,
		};
		let id = self.task.insert(Some(entry))?;

		tokio::spawn(async move {
			// Keep the origin producers alive for the lifetime of the reconnect loop:
			// the session reads from the publish consumer and writes into the subscribe producer.
			let _publish = publish;
			let _consume = consume;

			let res = tokio::select! {
				// close() requested: a clean shutdown delivers a terminal 0.
				_ = closed.1 => Ok(()),
				res = Self::report(callback, reconnect) => res,
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

	/// Snapshot the current connection's stats.
	///
	/// Errors with [`Error::SessionNotFound`] if the handle is unknown, or [`Error::Offline`]
	/// if the session is currently between connections (reconnecting).
	pub fn stats(&self, id: Id) -> Result<moq_net::ConnectionStats, Error> {
		self.task
			.get(id)
			.and_then(|entry| entry.as_ref())
			.ok_or(Error::SessionNotFound)?
			.stats
			.stats()
			.ok_or(Error::Offline)
	}

	/// Forward connection epochs to the status callback until the reconnect loop stops.
	///
	/// Returns the terminal error via `?`. Disconnects aren't reported: status 0 is reserved for a
	/// clean close (delivered as the terminal callback once the task ends).
	async fn report(callback: ffi::OnStatus, mut reconnect: moq_native::Reconnect) -> Result<(), Error> {
		let mut connects: u64 = 0;
		loop {
			if let moq_native::Status::Connected = reconnect.status().await.map_err(map_connect_error)? {
				connects += 1;
				// Positive status carries the connection epoch, so callers can tell a
				// reconnect (>1) from the first connect (1). No lock is held, so the C
				// callback is free to re-enter libmoq.
				let code = i32::try_from(connects)
					.context("connection epoch exceeded i32::MAX")
					.map_err(|err| Error::Connect(Arc::new(err)))?;
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

fn map_connect_error(err: moq_native::Error) -> Error {
	match err.connect_error() {
		Some(moq_native::ConnectError::Unauthorized) => Error::Unauthorized,
		Some(moq_native::ConnectError::Forbidden) => Error::Forbidden,
		_ => Error::Connect(Arc::new(err.into())),
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::ffi::ReturnCode;

	#[test]
	fn maps_native_auth_connect_errors() {
		assert!(matches!(
			map_connect_error(moq_native::ConnectError::Unauthorized.into()),
			Error::Unauthorized
		));
		assert!(matches!(
			map_connect_error(moq_native::ConnectError::Forbidden.into()),
			Error::Forbidden
		));
		assert!(matches!(
			map_connect_error(moq_net::Error::Unauthorized.into()),
			Error::Unauthorized
		));
		assert!(matches!(
			map_connect_error(moq_native::Error::ConnectFailed),
			Error::Connect(_)
		));
		assert_eq!(Error::Unauthorized.code(), -34);
		assert_eq!(Error::Forbidden.code(), -35);
		assert_eq!(map_connect_error(moq_native::Error::ConnectFailed).code(), -5);
	}
}
