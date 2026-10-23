use std::sync::Arc;

use anyhow::Context;
use url::Url;

use crate::task::Task;
use crate::{Error, Id, NonZeroSlab, State, ffi};

/// A session task and the connection backing its handle.
struct TaskEntry {
	task: Task,
	link: Link,
}

/// What backs a session handle.
enum Link {
	/// A dialed connection: a loop that redials, so it is offline between connections.
	Dialed {
		connection: moq_tokio::Connection,
		/// Reads live connection stats, reporting `None` while reconnecting.
		monitor: moq_tokio::connection::Monitor,
	},
	/// A server-accepted session: a single transport, `None` until SETUP completes.
	Accepted(Option<Accepted>),
}

struct Accepted {
	session: moq_net::Session,
	driver: ffi::Background,
}

/// Everything needed to prepare a session without holding the global state lock.
pub(crate) struct Connect {
	pub config: crate::client::Config,
	pub url: Url,
	pub publish: Option<crate::origin::Producer>,
	pub consume: Option<crate::origin::Producer>,
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
			client = client.with_publisher(&publish.producer);
		}
		if let Some(consume) = &self.consume {
			client = client.with_subscriber(consume.producer.clone());
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
	publish: Option<crate::origin::Producer>,
	consume: Option<crate::origin::Producer>,
	callback: ffi::OnStatus,
}

#[derive(Default)]
pub struct Session {
	/// Session tasks. Close signals shutdown; the task delivers a final callback, then removes itself.
	task: NonZeroSlab<TaskEntry>,
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
			connection: reconnect.clone(),
			monitor: reconnect.monitor(),
		};

		let (task, closed) = Task::new(callback);
		let id = self.task.insert(TaskEntry { task, link })?;

		tokio::spawn(async move {
			// Keep the origin producers alive for the lifetime of the reconnect loop:
			// the session reads from the publish consumer and writes into the subscribe producer.
			let publish_driver = publish.as_ref().map(|origin| origin.driver.clone()).unwrap_or_default();
			let consume_driver = consume.as_ref().map(|origin| origin.driver.clone()).unwrap_or_default();
			let _publish = publish;
			let _consume = consume;

			// A panic ends only this session, reported through the terminal callback.
			let res = ffi::run(publish_driver.watch(consume_driver.watch(async move {
				tokio::select! {
					// close() requested: drain what the session queued, then close it.
					_ = closed => reconnect.clone().close().await.map_err(map_connect_error),
					res = Self::report(callback, reconnect.clone()) => res,
				}
			})))
			.await;
			// The terminal callback waits for the close to reach the peer, so a caller that exits
			// on it doesn't leave the peer to time the connection out.
			let res = crate::ffi::run(async {
				client.close().await;
				publish_driver.check()?;
				consume_driver.check()?;
				res
			})
			.await;

			// Deliver one final terminal callback (0 = closed, < 0 = error), then
			// drop the entry. Pull it out from under the lock so the callback never
			// runs while held.
			let entry = State::lock().session.task.remove(id);
			if let Some(entry) = entry {
				entry.task.callback.call(res);
			}
		});

		Ok(id)
	}

	/// Complete SETUP for an incoming session, reporting `1` once established.
	pub fn accept(
		&mut self,
		request: moq_tokio::server::Request,
		publish: Option<crate::origin::Producer>,
		consume: Option<crate::origin::Producer>,
		callback: ffi::OnStatus,
	) -> Result<Id, Error> {
		let mut request = request;
		if let Some(publish) = &publish {
			request = request.with_publisher(&publish.producer);
		}
		if let Some(consume) = &consume {
			request = request.with_subscriber(consume.producer.clone());
		}

		let (task, closed) = Task::new(callback);
		let id = self.task.insert(TaskEntry {
			task,
			link: Link::Accepted(None),
		})?;

		tokio::spawn(async move {
			// Keep the origin producers alive for the lifetime of the session.
			let publish_driver = publish.as_ref().map(|origin| origin.driver.clone()).unwrap_or_default();
			let consume_driver = consume.as_ref().map(|origin| origin.driver.clone()).unwrap_or_default();
			let _publish = publish;
			let _consume = consume;

			// A panic ends only this session, reported through the terminal callback.
			let mut res = ffi::run(publish_driver.watch(consume_driver.watch(async move {
				tokio::select! {
					_ = closed => Ok(()),
					res = Self::serve(id, callback, request) => res,
				}
			})))
			.await;

			let entry = State::lock().session.task.remove(id);
			if let Some(entry) = entry {
				// Close the transport now rather than whenever the last clone drops.
				if let Link::Accepted(Some(accepted)) = &entry.link
					&& let Err(error) = ffi::run(async {
						accepted.session.abort(moq_net::Error::Cancel);
						accepted.driver.finished().await
					})
					.await
				{
					res = Err(error);
				}
				let res = publish_driver.check().and_then(|()| consume_driver.check()).and(res);
				entry.task.callback.call(res);
			}
		});

		Ok(id)
	}

	/// Establish an accepted session, publish it to the entry, and wait for it to close.
	async fn serve(id: Id, callback: ffi::OnStatus, request: moq_tokio::server::Request) -> Result<(), Error> {
		let (session, driver) = request.ok().await.map_err(map_connect_error)?;
		let driver = ffi::Background::new(async move {
			match driver.await {
				Ok(result) => result.map_err(map_connect_error),
				Err(error) if error.is_panic() => Err(Error::Panic),
				Err(_) => Err(moq_net::Error::Cancel.into()),
			}
		});

		if let Some(entry) = State::lock().session.task.get_mut(id) {
			entry.link = Link::Accepted(Some(Accepted {
				session: session.clone(),
				driver: driver.clone(),
			}));
		}

		// A server-accepted session is a single transport, so it only ever connects once.
		callback.call(1);
		driver.finished().await?;
		Err(session.closed().await.into())
	}

	fn link(&self, id: Id) -> Result<&Link, Error> {
		Ok(&self.task.get(id).ok_or(Error::SessionNotFound)?.link)
	}

	/// The session's bandwidth allocator. Every handle shares one reservation registry,
	/// which the session probes for.
	///
	/// Errors with [`Error::Offline`] for an accepted session still in SETUP.
	pub fn bandwidth(&self, id: Id) -> Result<moq_net::bandwidth::Allocator, Error> {
		match self.link(id)? {
			Link::Dialed { connection, .. } => Ok(connection.bandwidth()),
			Link::Accepted(accepted) => Ok(accepted.as_ref().ok_or(Error::Offline)?.session.bandwidth()),
		}
	}

	/// Mint receive-estimate demand only when the caller asks for a consumer.
	pub fn recv_bandwidth(&self, id: Id) -> Result<Option<moq_net::bandwidth::Consumer>, Error> {
		match self.link(id)? {
			Link::Dialed { connection, .. } => Ok(Some(connection.recv_bandwidth())),
			Link::Accepted(accepted) => Ok(accepted.as_ref().ok_or(Error::Offline)?.session.recv_bandwidth()),
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

	/// Why a dialed session is reconnecting, or `None` while nothing has failed since the
	/// last connect. A server-accepted session never reconnects, so it is always `None`.
	///
	/// Errors with [`Error::SessionNotFound`] if the handle is unknown.
	pub fn failure(&self, id: Id) -> Result<Option<Error>, Error> {
		Ok(match self.link(id)? {
			Link::Dialed { monitor, .. } => monitor.failure().map(map_connect_error),
			Link::Accepted(_) => None,
		})
	}

	/// Forward connect counts to the status callback until the reconnect loop stops.
	///
	/// Returns the terminal error via `?`. Disconnects aren't reported: status 0 is reserved for a
	/// clean close (delivered as the terminal callback once the task ends).
	async fn report(callback: ffi::OnStatus, mut reconnect: moq_tokio::Connection) -> Result<(), Error> {
		let mut connects: u64 = 0;
		loop {
			if let moq_tokio::Status::Connected = reconnect.status().await.map_err(map_connect_error)? {
				connects += 1;
				// Positive status carries the connect count, so callers can tell a
				// reconnect (>1) from the first connect (1). No lock is held, so the C
				// callback is free to re-enter moq-c.
				let code = i32::try_from(connects)
					.context("connect count exceeded i32::MAX")
					.map_err(|err| Error::Connect(Arc::new(err)))?;
				callback.call(code);
			}
		}
	}

	pub fn close(&mut self, id: Id) -> Result<(), Error> {
		// Signal shutdown; the task delivers a final callback and removes itself.
		self.task
			.get_mut(id)
			.ok_or(Error::SessionNotFound)?
			.task
			.close(Error::SessionNotFound)
	}
}

fn map_connect_error(err: moq_tokio::Error) -> Error {
	match err {
		// Local auth stays the dedicated C status. A scoped protocol close is `Error::Moq`
		// so `moq_error_protocol` can recover the registry and code.
		moq_tokio::Error::MoqNet(moq_net::Error::Unauthorized) => Error::Unauthorized,
		moq_tokio::Error::Panic => Error::Panic,
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
		// A refused request's expiry is not a session close, so it has its own status.
		let expired = map_connect_error(moq_net::Error::Expired.into());
		assert_eq!(expired.code(), -42);
		assert!(expired.protocol().is_none());
		assert_eq!(
			map_connect_error(moq_net::Error::from(moq_net::SessionError::Unauthorized).into()).code(),
			-2
		);
		assert_eq!(map_connect_error(moq_tokio::Error::ConnectFailed).code(), -5);
	}
}
