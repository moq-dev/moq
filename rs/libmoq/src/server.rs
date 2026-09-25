//! Accepting sessions: listeners and the session requests they deliver.

use std::ffi::c_char;

use tokio::sync::oneshot;

use crate::ffi::OnStatus;
use crate::{Error, Id, NonZeroSlab, State, moq_string};

/// A listener task: `close` signals shutdown, `callback` delivers requests and the terminal status.
///
/// The task delivers one final terminal callback and then removes itself, so
/// `user_data` and the borrowed `addr`/`fingerprints` stay valid until then.
struct ServerEntry {
	close: Option<oneshot::Sender<()>>,
	callback: OnStatus,
	addr: String,
	fingerprints: Vec<String>,
}

#[derive(Default)]
pub struct Server {
	/// Listener tasks. Close signals shutdown; the task delivers a final callback, then removes itself.
	task: NonZeroSlab<Option<ServerEntry>>,

	/// Incoming sessions delivered to a listener callback, freed after accept/reject.
	request: NonZeroSlab<moq_tokio::server::Request>,
}

impl Server {
	/// Serve `server`, delivering each incoming session as a request handle via `callback`.
	pub fn listen(&mut self, server: moq_tokio::Server, callback: OnStatus) -> Result<Id, Error> {
		let addr = server.local_addr()?.to_string();
		let fingerprints = server.certificates().fingerprints();
		let (close, closed) = oneshot::channel();
		let id = self.task.insert(Some(ServerEntry {
			close: Some(close),
			callback,
			addr,
			fingerprints,
		}))?;

		tokio::spawn(async move {
			let res = Self::run(server, callback, closed).await;

			// Deliver one final terminal callback (code <= 0), then drop the entry.
			// Pull it out from under the lock so the callback never runs while held.
			let entry = State::lock().server.task.remove(id).flatten();
			if let Some(entry) = entry {
				entry.callback.call(res);
			}
		});

		Ok(id)
	}

	async fn run(server: moq_tokio::Server, callback: OnStatus, mut close: oneshot::Receiver<()>) -> Result<(), Error> {
		let mut listener = server.listen().await?;

		let res = loop {
			// `biased` so a pending close always wins over a ready request.
			let request = tokio::select! {
				biased;
				_ = &mut close => break Ok(()),
				request = listener.accept() => match request {
					Some(request) => request,
					None => break Ok(()),
				},
			};

			// Hold the lock only to buffer the request; release it before the callback.
			let request = match State::lock().server.request.insert(request) {
				Ok(request) => request,
				Err(err) => break Err(err),
			};
			callback.call(request);
		};

		// Release the sockets before the terminal callback, so the address can be bound again.
		listener.close().await;
		res
	}

	pub fn close(&mut self, server: Id) -> Result<(), Error> {
		// Signal shutdown; the task delivers a final callback and removes itself.
		self.task
			.get_mut(server)
			.and_then(|entry| entry.as_mut())
			.ok_or(Error::NotFound)?
			.close
			.take()
			.ok_or(Error::NotFound)?;
		Ok(())
	}

	fn entry(&self, server: Id) -> Result<&ServerEntry, Error> {
		self.task
			.get(server)
			.and_then(|entry| entry.as_ref())
			.ok_or(Error::NotFound)
	}

	pub fn addr(&self, server: Id, dst: &mut moq_string) -> Result<(), Error> {
		*dst = borrow(&self.entry(server)?.addr);
		Ok(())
	}

	/// Write up to `dst.len()` fingerprints and return how many there are in total.
	pub fn fingerprints(&self, server: Id, dst: &mut [moq_string]) -> Result<usize, Error> {
		let fingerprints = &self.entry(server)?.fingerprints;
		for (dst, fingerprint) in dst.iter_mut().zip(fingerprints) {
			*dst = borrow(fingerprint);
		}
		Ok(fingerprints.len())
	}

	fn request(&self, request: Id) -> Result<&moq_tokio::server::Request, Error> {
		self.request.get(request).ok_or(Error::NotFound)
	}

	pub fn request_path(&self, request: Id, dst: &mut moq_string) -> Result<(), Error> {
		*dst = borrow(self.request(request)?.path());
		Ok(())
	}

	pub fn request_query(&self, request: Id, dst: &mut moq_string) -> Result<(), Error> {
		*dst = match self.request(request)?.query() {
			Some(query) => borrow(query),
			None => moq_string {
				data: std::ptr::null(),
				len: 0,
			},
		};
		Ok(())
	}

	/// Remove a request from the table, for the call that answers it.
	pub fn request_take(&mut self, request: Id) -> Result<moq_tokio::server::Request, Error> {
		self.request.remove(request).ok_or(Error::NotFound)
	}
}

fn borrow(value: &str) -> moq_string {
	moq_string {
		data: value.as_ptr().cast::<c_char>(),
		len: value.len(),
	}
}
