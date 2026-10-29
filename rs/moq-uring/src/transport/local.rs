use bytes::{Buf, Bytes};
use moq_net::transport::{self as moq, poll};
use send_wrapper::SendWrapper;
use std::task::Poll;

/// A worker's transport, usable where moq-net wants a `Send` one.
///
/// moq-net boxes every transport with a native `Send` bound, which a worker's
/// `!Send` [`Session`](super::Session) cannot meet, so wrap the session in this
/// before `Server::accept_request` or `Client::connect`. The session, its
/// streams, and its send groups never leave the worker that polls them, so
/// this checks the thread on every use and panics anywhere else instead of
/// proving it at compile time.
#[derive(Clone)]
pub struct Local<T>(SendWrapper<T>);

impl<T> Local<T> {
	/// Bind `inner` to the current thread.
	pub fn new(inner: T) -> Self {
		Self(SendWrapper::new(inner))
	}
}

/// Unwrap the send group of `options`, checking the thread.
fn unwrap<'a, G>(options: poll::SendOptions<'a, Local<G>>) -> poll::SendOptions<'a, G> {
	poll::SendOptions {
		group: options.group.map(|group| &*group.0),
		priority: options.priority,
	}
}

impl<S: poll::Session> poll::Session for Local<S> {
	type SendStream = Local<S::SendStream>;
	type RecvStream = Local<S::RecvStream>;
	type Error = S::Error;

	fn poll_accept_uni(&self, waiter: &kio::Waiter) -> Poll<Result<Self::RecvStream, Self::Error>> {
		self.0.poll_accept_uni(waiter).map_ok(Local::new)
	}

	fn poll_accept_bi(&self, waiter: &kio::Waiter) -> Poll<Result<poll::BiStreams<Self>, Self::Error>> {
		self.0
			.poll_accept_bi(waiter)
			.map_ok(|(send, recv)| (Local::new(send), Local::new(recv)))
	}

	fn poll_open_uni(
		&self,
		waiter: &kio::Waiter,
		options: poll::SendOptions<'_, Local<<S::SendStream as poll::SendStream>::Group>>,
	) -> Poll<Result<Self::SendStream, Self::Error>> {
		self.0.poll_open_uni(waiter, unwrap(options)).map_ok(Local::new)
	}

	fn poll_open_bi(
		&self,
		waiter: &kio::Waiter,
		options: poll::SendOptions<'_, Local<<S::SendStream as poll::SendStream>::Group>>,
	) -> Poll<Result<poll::BiStreams<Self>, Self::Error>> {
		self.0
			.poll_open_bi(waiter, unwrap(options))
			.map_ok(|(send, recv)| (Local::new(send), Local::new(recv)))
	}

	fn poll_open_request(
		&self,
		waiter: &kio::Waiter,
		options: poll::SendOptions<'_, Local<<S::SendStream as poll::SendStream>::Group>>,
	) -> Poll<Result<Option<poll::BiStreams<Self>>, Self::Error>> {
		self.0
			.poll_open_request(waiter, unwrap(options))
			.map_ok(|streams| streams.map(|(send, recv)| (Local::new(send), Local::new(recv))))
	}

	fn send_group(&self, priority: i64) -> Option<Local<<S::SendStream as poll::SendStream>::Group>> {
		self.0.send_group(priority).map(Local::new)
	}

	fn send_datagram(
		&self,
		payload: Bytes,
		options: poll::SendOptions<'_, Local<<S::SendStream as poll::SendStream>::Group>>,
	) -> Result<(), Self::Error> {
		self.0.send_datagram(payload, unwrap(options))
	}

	fn poll_recv_datagram(&self, waiter: &kio::Waiter) -> Poll<Result<Bytes, Self::Error>> {
		self.0.poll_recv_datagram(waiter)
	}

	fn max_datagram_size(&self) -> usize {
		self.0.max_datagram_size()
	}

	fn protocol(&self) -> Option<&str> {
		self.0.protocol()
	}

	fn address(&self) -> moq::Address {
		self.0.address()
	}

	fn close(&self, code: u32, reason: &str) {
		self.0.close(code, reason)
	}

	fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Self::Error> {
		self.0.poll_closed(waiter)
	}

	fn metrics(&self) -> impl moq::Metrics {
		self.0.metrics()
	}

	fn wire(&self) -> Option<moq::Wire> {
		self.0.wire()
	}

	fn probe(&self) -> Option<moq::Probe> {
		self.0.probe()
	}
}

impl<T: poll::SendStream> poll::SendStream for Local<T> {
	type Error = T::Error;
	type Group = Local<T::Group>;

	fn poll_write(&mut self, waiter: &kio::Waiter, buf: &[u8]) -> Poll<Result<usize, Self::Error>> {
		self.0.poll_write(waiter, buf)
	}

	fn poll_write_buf<B: Buf>(&mut self, waiter: &kio::Waiter, buf: &mut B) -> Poll<Result<usize, Self::Error>> {
		self.0.poll_write_buf(waiter, buf)
	}

	fn set_priority(&mut self, priority: i64) {
		self.0.set_priority(priority)
	}

	fn set_group(&mut self, group: &Self::Group) {
		self.0.set_group(&group.0)
	}

	fn finish(&mut self) -> Result<(), Self::Error> {
		self.0.finish()
	}

	fn reset(&mut self, code: u32) {
		self.0.reset(code)
	}

	fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Result<(), Self::Error>> {
		self.0.poll_closed(waiter)
	}
}

impl<T: poll::RecvStream> poll::RecvStream for Local<T> {
	type Error = T::Error;
	type Lease<'a>
		= T::Lease<'a>
	where
		Self: 'a;

	fn poll_lease(&mut self, waiter: &kio::Waiter, want: usize) -> Poll<Result<Self::Lease<'_>, Self::Error>> {
		self.0.poll_lease(waiter, want)
	}

	fn set_priority(&mut self, priority: i64) {
		self.0.set_priority(priority)
	}

	fn stop(&mut self, code: u32) {
		self.0.stop(code)
	}

	fn poll_read_chunk(&mut self, waiter: &kio::Waiter, max: usize) -> Poll<Result<Option<Bytes>, Self::Error>> {
		self.0.poll_read_chunk(waiter, max)
	}
}

impl<T: poll::SendGroup> poll::SendGroup for Local<T> {
	fn set_priority(&mut self, priority: i64) {
		self.0.set_priority(priority)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use moq_net::transport::memory;

	/// The point of the wrapper: a worker's `!Send` session meets moq-net's bounds.
	#[cfg(feature = "quic")]
	#[test]
	fn meets_the_boxed_driver_bounds() {
		fn boxable<S: poll::Boxable>() {}
		boxable::<Local<super::super::Session<crate::quic::web::Session>>>();
	}

	/// Touching it off its thread fails loud rather than racing the worker.
	#[test]
	fn panics_off_its_thread() {
		let (session, _peer) = memory::pair(Default::default());
		let session = Local::new(session);
		let other = session.clone();
		let (panicked, other) = std::thread::spawn(move || {
			let used = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
				poll::Session::max_datagram_size(&other);
			}));
			// Dropping it here would panic too; hand it back instead.
			(used.is_err(), other)
		})
		.join()
		.expect("the thread returns its outcome");
		assert!(panicked, "an off-thread use must panic");
		drop(other);
	}
}
