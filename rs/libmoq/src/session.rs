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
	link: Link,
}

/// What backs a session handle.
enum Link {
	/// A dialed connection: a loop that redials, so it is offline between connections.
	Dialed {
		/// Reads live connection stats, reporting `None` while reconnecting.
		monitor: moq_tokio::connection::Monitor,
		/// One allocator for the session. Every `moq_session_bandwidth` handle clones
		/// it, so they share one reservation registry.
		bandwidth: moq_net::bandwidth::Allocator,
	},
	/// A server-accepted session: a single transport, `None` until SETUP completes.
	Accepted(Option<Accepted>),
}

struct Accepted {
	session: moq_net::Session,
	/// Shared by every `moq_session_bandwidth` handle, like [`Link::Dialed`]'s.
	bandwidth: moq_net::bandwidth::Allocator,
}

/// Everything needed to prepare a session without holding the global state lock.
pub(crate) struct Connect {
	pub config: crate::client::Config,
	pub url: Url,
	pub publish: Option<moq_net::origin::Producer>,
	pub consume: Option<moq_net::origin::Producer>,
	pub callback: ffi::OnStatus,
}

impl Connect {
	/// Resolve files and backend configuration before the session is inserted.
	pub fn prepare(self) -> Result<PreparedConnect, Error> {
		let mut client = self
			.config
			.connect
			.clone()
			.init(self.config.quic.clone())
			.map_err(|err| Error::InvalidConfig(err.to_string()))?;
		if let Some(publish) = &self.publish {
			client = client.with_publisher(publish);
		}
		if let Some(consume) = &self.consume {
			client = client.with_subscriber(consume.clone());
		}

		Ok(PreparedConnect {
			client,
			url: self.url,
			publish: self.publish,
			consume: self.consume,
			callback: self.callback,
		})
	}
}

/// A validated session request ready for insertion into global state.
pub(crate) struct PreparedConnect {
	client: moq_tokio::Client,
	url: Url,
	publish: Option<moq_net::origin::Producer>,
	consume: Option<moq_net::origin::Producer>,
	callback: ffi::OnStatus,
}

#[derive(Default)]
pub struct Session {
	/// Session tasks. Close signals shutdown; the task delivers a final callback, then removes itself.
	task: NonZeroSlab<Option<TaskEntry>>,
}

impl Session {
	pub fn connect(&mut self, request: PreparedConnect) -> Result<Id, Error> {
		let PreparedConnect {
			client,
			url,
			publish,
			consume,
			callback,
		} = request;

		// Build the reconnect loop up front so we can grab a monitor for it
		// before moving it into the spawned task.
		let reconnect = client.connect(url);
		let link = Link::Dialed {
			monitor: reconnect.monitor(),
			bandwidth: moq_net::bandwidth::Allocator::new(reconnect.send_bandwidth()),
		};

		let closed = oneshot::channel();
		let entry = TaskEntry {
			close: Some(closed.0),
			callback,
			link,
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

	/// Complete SETUP for an incoming session, reporting `1` once established.
	pub fn accept(
		&mut self,
		request: moq_tokio::server::Request,
		publish: Option<moq_net::origin::Producer>,
		consume: Option<moq_net::origin::Producer>,
		callback: ffi::OnStatus,
	) -> Result<Id, Error> {
		let mut request = request;
		if let Some(publish) = &publish {
			request = request.with_publisher(publish);
		}
		if let Some(consume) = &consume {
			request = request.with_subscriber(consume.clone());
		}

		let closed = oneshot::channel();
		let entry = TaskEntry {
			close: Some(closed.0),
			callback,
			link: Link::Accepted(None),
		};
		let id = self.task.insert(Some(entry))?;

		tokio::spawn(async move {
			// Keep the origin producers alive for the lifetime of the session.
			let _publish = publish;
			let _consume = consume;

			let res = tokio::select! {
				_ = closed.1 => Ok(()),
				res = Self::serve(id, callback, request) => res,
			};

			let entry = State::lock().session.task.remove(id).flatten();
			if let Some(entry) = entry {
				// Close the transport now rather than whenever the last clone drops.
				if let Link::Accepted(Some(accepted)) = &entry.link {
					accepted.session.abort(moq_net::Error::Cancel);
				}
				entry.callback.call(res);
			}
		});

		Ok(id)
	}

	/// Establish an accepted session, publish it to the entry, and wait for it to close.
	async fn serve(id: Id, callback: ffi::OnStatus, request: moq_tokio::server::Request) -> Result<(), Error> {
		let session = request.ok().await.map_err(map_connect_error)?;
		let bandwidth = session
			.send_bandwidth()
			.map(moq_net::bandwidth::Allocator::new)
			.unwrap_or_else(moq_net::bandwidth::Allocator::unlimited);

		if let Some(entry) = State::lock().session.task.get_mut(id).and_then(|entry| entry.as_mut()) {
			entry.link = Link::Accepted(Some(Accepted {
				session: session.clone(),
				bandwidth,
			}));
		}

		// A server-accepted session is a single transport, so it only ever reaches epoch 1.
		callback.call(1);
		Err(session.closed().await.into())
	}

	fn link(&self, id: Id) -> Result<&Link, Error> {
		Ok(&self
			.task
			.get(id)
			.and_then(|entry| entry.as_ref())
			.ok_or(Error::SessionNotFound)?
			.link)
	}

	/// The session's bandwidth allocator. Clones share one reservation registry.
	///
	/// Errors with [`Error::Offline`] for an accepted session still in SETUP.
	pub fn bandwidth(&self, id: Id) -> Result<moq_net::bandwidth::Allocator, Error> {
		match self.link(id)? {
			Link::Dialed { bandwidth, .. } => Ok(bandwidth.clone()),
			Link::Accepted(accepted) => Ok(accepted.as_ref().ok_or(Error::Offline)?.bandwidth.clone()),
		}
	}

	/// Snapshot the current connection's stats.
	///
	/// Errors with [`Error::SessionNotFound`] if the handle is unknown, or [`Error::Offline`]
	/// if the session has no live connection (reconnecting, or still in SETUP).
	pub fn stats(&self, id: Id) -> Result<moq_net::session::Stats, Error> {
		Ok(self.snapshot(id)?.0)
	}

	/// Statistics and protocol from the same live connection.
	///
	/// Errors with [`Error::SessionNotFound`] if the handle is unknown, or [`Error::Offline`]
	/// if the session has no live connection (reconnecting, or still in SETUP).
	pub fn snapshot(&self, id: Id) -> Result<(moq_net::session::Stats, moq_net::Version), Error> {
		match self.link(id)? {
			Link::Dialed { monitor, .. } => {
				let snapshot = monitor.snapshot().ok_or(Error::Offline)?;
				Ok((snapshot.stats, snapshot.version))
			}
			Link::Accepted(accepted) => {
				let session = &accepted.as_ref().ok_or(Error::Offline)?.session;
				Ok((session.stats(), session.version()))
			}
		}
	}

	/// Forward connection epochs to the status callback until the reconnect loop stops.
	///
	/// Returns the terminal error via `?`. Disconnects aren't reported: status 0 is reserved for a
	/// clean close (delivered as the terminal callback once the task ends).
	async fn report(callback: ffi::OnStatus, mut reconnect: moq_tokio::Connection) -> Result<(), Error> {
		let mut connects: u64 = 0;
		loop {
			if let moq_tokio::Status::Connected = reconnect.status().await.map_err(map_connect_error)? {
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

fn map_connect_error(err: moq_tokio::Error) -> Error {
	match err {
		// Local auth stays the dedicated C status. A scoped protocol close is `Error::Moq`
		// so `moq_error_protocol` can recover the registry and code.
		moq_tokio::Error::MoqNet(moq_net::Error::Unauthorized) => Error::Unauthorized,
		moq_tokio::Error::MoqNet(err) => err.into(),
		err => match err.connect_error() {
			Some(moq_tokio::ConnectError::Unauthorized) => Error::Unauthorized,
			Some(moq_tokio::ConnectError::Forbidden) => Error::Forbidden,
			_ => Error::Connect(Arc::new(err.into())),
		},
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::ffi::ReturnCode;

	#[test]
	fn maps_native_auth_connect_errors() {
		assert!(matches!(
			map_connect_error(moq_tokio::ConnectError::Unauthorized.into()),
			Error::Unauthorized
		));
		assert!(matches!(
			map_connect_error(moq_tokio::ConnectError::Forbidden.into()),
			Error::Forbidden
		));
		assert!(matches!(
			map_connect_error(moq_net::Error::Unauthorized.into()),
			Error::Unauthorized
		));
		assert!(matches!(
			map_connect_error(moq_net::Error::from(moq_net::SessionError::Unauthorized).into()),
			Error::Moq(moq_net::Error::Session(moq_net::SessionError::Unauthorized))
		));
		assert!(matches!(
			map_connect_error(moq_tokio::Error::ConnectFailed),
			Error::Connect(_)
		));
		assert_eq!(Error::Unauthorized.code(), -34);
		assert_eq!(Error::Forbidden.code(), -35);
		assert_eq!(map_connect_error(moq_net::Error::Unauthorized.into()).code(), -34);
		assert_eq!(
			map_connect_error(moq_net::Error::from(moq_net::SessionError::Unauthorized).into()).code(),
			-2
		);
		assert_eq!(map_connect_error(moq_tokio::Error::ConnectFailed).code(), -5);
	}
}
