use std::{
	fmt::{self, Debug},
	io::{self, IoSliceMut},
	net::SocketAddr,
	sync::{Arc, Mutex, RwLock},
	task::{Context, Poll, Waker, ready},
	time::Duration,
};

use moq_sock::udp::{RecvMeta, Transmit};

use crate::quic::driver::Instant;

/// A UDP socket an [`Endpoint`](super::endpoint::Endpoint) runs on, for a runtime or transport of your own
///
/// The endpoint owns it: every connection sends through the endpoint, so dropping the endpoint
/// or awaiting its [`close`](super::endpoint::Endpoint::close) releases the socket.
pub trait AsyncUdpSocket: Send + Sync + Debug + 'static {
	/// Receive UDP datagrams, or register to be woken if receiving may succeed in the future
	///
	/// Only the endpoint receives, so one waker at a time is enough.
	fn poll_recv(
		&self,
		cx: &mut Context<'_>,
		bufs: &mut [IoSliceMut<'_>],
		meta: &mut [RecvMeta],
	) -> Poll<io::Result<usize>>;

	/// Send a datagram if the socket has room for it, or fail with
	/// [`WouldBlock`](io::ErrorKind::WouldBlock)
	///
	/// An error matching [`moq_sock::udp::is_unreachable_err`] reaches
	/// [`moq_quic::connection::Connection::handle_unreachable`]; any other is logged and the
	/// datagram counted as lost.
	fn try_send(&self, transmit: &Transmit<'_>) -> io::Result<()>;

	/// Register to be woken once the socket may have room to send again
	///
	/// The endpoint registers one waker of its own and wakes every connection blocked on a send
	/// from it, so one waker at a time is enough.
	fn poll_writable(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>>;

	/// Look up the local IP address and port used by this socket
	fn local_addr(&self) -> io::Result<SocketAddr>;

	/// Maximum number of datagrams that might be described by a single [`RecvMeta`]
	fn max_receive_segments(&self) -> usize {
		1
	}

	/// Maximum number of datagrams that a [`Transmit`] may encode
	fn max_transmit_segments(&self) -> usize {
		1
	}

	/// Whether datagrams might get fragmented into multiple parts
	///
	/// Sockets should prevent this for best performance. See e.g. the `IPV6_DONTFRAG` socket
	/// option.
	fn may_fragment(&self) -> bool {
		true
	}
}

/// The endpoint's socket, which every connection sends through without keeping it open
///
/// [`release`](Self::release) closes the socket whoever still holds the slot, which is how the
/// endpoint frees its port while connection handles outlive it.
pub(crate) struct Slot {
	socket: RwLock<Option<Box<dyn AsyncUdpSocket>>>,
	/// Wakes every connection blocked on a full send buffer from the socket's one registration
	blocked: Arc<Blocked>,
	/// When a send error was last logged, so a busy endpoint logs one per interval
	last_send_error: Mutex<Option<Instant>>,
}

impl Slot {
	pub(crate) fn new(socket: Box<dyn AsyncUdpSocket>) -> Self {
		Self {
			socket: RwLock::new(Some(socket)),
			blocked: Default::default(),
			last_send_error: Default::default(),
		}
	}

	/// Send `transmit`, or register to be woken once the socket may have room
	///
	/// A released socket loses the datagram, which UDP always allows.
	pub(crate) fn poll_send(&self, transmit: &Transmit<'_>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
		let socket = self.socket.read().unwrap();
		let Some(socket) = socket.as_deref() else {
			return Poll::Ready(Ok(()));
		};
		loop {
			match socket.try_send(transmit) {
				Ok(()) => return Poll::Ready(Ok(())),
				Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
				Err(err) => {
					let mut last = self.last_send_error.lock().unwrap();
					return Poll::Ready(send_failed(&mut last, err, transmit));
				}
			}
			// Every connection sends from its own task, but a socket keeps one waker per
			// direction, so register a fan-out to all of them instead of this task's.
			self.blocked.push(cx.waker());
			let waker = Waker::from(self.blocked.clone());
			ready!(socket.poll_writable(&mut Context::from_waker(&waker)))?;
		}
	}

	/// Receive as [`AsyncUdpSocket::poll_recv`]; a released socket never receives again
	pub(crate) fn poll_recv(
		&self,
		cx: &mut Context<'_>,
		bufs: &mut [IoSliceMut<'_>],
		meta: &mut [RecvMeta],
	) -> Poll<io::Result<usize>> {
		match self.socket.read().unwrap().as_deref() {
			Some(socket) => socket.poll_recv(cx, bufs, meta),
			None => Poll::Pending,
		}
	}

	/// Send through `socket` from now on, returning the one it replaces
	pub(crate) fn replace(&self, socket: Box<dyn AsyncUdpSocket>) -> Option<Box<dyn AsyncUdpSocket>> {
		let previous = self.socket.write().unwrap().replace(socket);
		// A sender blocked on the old socket would otherwise wait on it forever.
		self.blocked.wake_all();
		previous
	}

	/// Close the socket, whoever still sends through the slot
	pub(crate) fn release(&self) {
		self.socket.write().unwrap().take();
		self.blocked.wake_all();
	}

	/// See [`AsyncUdpSocket::max_transmit_segments`]
	pub(crate) fn max_transmit_segments(&self) -> usize {
		self.socket
			.read()
			.unwrap()
			.as_deref()
			.map_or(1, AsyncUdpSocket::max_transmit_segments)
	}
}

impl Debug for Slot {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("Slot")
			.field("socket", &self.socket)
			.finish_non_exhaustive()
	}
}

/// The tasks waiting for a [`Slot`] to become writable
#[derive(Debug, Default)]
struct Blocked(Mutex<Vec<Waker>>);

impl Blocked {
	fn push(&self, waker: &Waker) {
		let mut wakers = self.0.lock().unwrap();
		if !wakers.iter().any(|w| w.will_wake(waker)) {
			wakers.push(waker.clone());
		}
	}

	fn wake_all(&self) {
		for waker in std::mem::take(&mut *self.0.lock().unwrap()) {
			waker.wake();
		}
	}
}

impl std::task::Wake for Blocked {
	fn wake(self: Arc<Self>) {
		self.wake_all();
	}

	fn wake_by_ref(self: &Arc<Self>) {
		self.wake_all();
	}
}

/// Limits I/O error logging to one message per minute.
const IO_ERROR_LOG_INTERVAL: Duration = Duration::from_secs(60);

/// What a failed send, other than `WouldBlock`, means for [`Slot::poll_send`]
///
/// No route to the destination comes back, since retransmitting cannot help and the connection
/// decides whether to give up on the peer. Anything else is logged at most once per
/// [`IO_ERROR_LOG_INTERVAL`] and treated as loss, which UDP always allows.
fn send_failed(last_send_error: &mut Option<Instant>, error: io::Error, transmit: &Transmit<'_>) -> io::Result<()> {
	if moq_sock::udp::is_unreachable_err(&error) {
		return Err(error);
	}
	#[cfg(unix)]
	// Unix `EMSGSIZE` is expected for MTU probes.
	if moq_sock::udp::is_msg_size_err(&error) {
		return Ok(());
	}

	let now = Instant::now();
	if last_send_error.is_some_and(|last| now.saturating_duration_since(last) <= IO_ERROR_LOG_INTERVAL) {
		return Ok(());
	}
	*last_send_error = Some(now);

	tracing::warn!(
		"sendmsg error: {:?}, Transmit: {{ destination: {:?}, src_ip: {:?}, ecn: {:?}, len: {:?}, segment_size: {:?} }}",
		error,
		transmit.destination,
		transmit.src_ip,
		transmit.ecn,
		transmit.contents.len(),
		transmit.segment_size
	);
	Ok(())
}

mod tokio;
pub(crate) use tokio::wrap_udp_socket;

#[cfg(test)]
mod tests {
	use std::task::Waker;

	use super::*;

	#[test]
	fn non_would_block_send_errors_are_logged_and_ignored() {
		let slot = Slot::new(Box::new(TestSocket(io::ErrorKind::PermissionDenied)));
		let mut cx = Context::from_waker(Waker::noop());

		let result = slot.poll_send(&transmit(), &mut cx);

		assert!(matches!(result, Poll::Ready(Ok(()))));
		assert!(slot.last_send_error.lock().unwrap().is_some());
	}

	#[test]
	fn unreachable_send_errors_are_returned() {
		let slot = Slot::new(Box::new(TestSocket(io::ErrorKind::NetworkUnreachable)));
		let mut cx = Context::from_waker(Waker::noop());

		let result = slot.poll_send(&transmit(), &mut cx);

		assert!(
			matches!(result, Poll::Ready(Err(ref e)) if e.kind() == io::ErrorKind::NetworkUnreachable),
			"{result:?}"
		);
	}

	#[test]
	fn a_released_slot_loses_sends() {
		let slot = Slot::new(Box::new(TestSocket(io::ErrorKind::NetworkUnreachable)));
		slot.release();
		let mut cx = Context::from_waker(Waker::noop());

		assert!(matches!(slot.poll_send(&transmit(), &mut cx), Poll::Ready(Ok(()))));
	}

	fn transmit() -> Transmit<'static> {
		Transmit {
			destination: SocketAddr::from(([127, 0, 0, 1], 4433)),
			ecn: None,
			contents: &[],
			segment_size: None,
			src_ip: None,
		}
	}

	#[derive(Debug)]
	struct TestSocket(io::ErrorKind);

	impl AsyncUdpSocket for TestSocket {
		fn poll_recv(
			&self,
			_cx: &mut Context<'_>,
			_bufs: &mut [IoSliceMut<'_>],
			_meta: &mut [RecvMeta],
		) -> Poll<io::Result<usize>> {
			Poll::Pending
		}

		fn try_send(&self, _transmit: &Transmit<'_>) -> io::Result<()> {
			Err(self.0.into())
		}

		fn poll_writable(&self, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
			Poll::Ready(Ok(()))
		}

		fn local_addr(&self) -> io::Result<SocketAddr> {
			Ok(SocketAddr::from(([127, 0, 0, 1], 0)))
		}
	}
}
