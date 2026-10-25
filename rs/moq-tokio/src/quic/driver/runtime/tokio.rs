use std::{
	io,
	pin::Pin,
	sync::Arc,
	task::{Context, Poll, ready},
};

use tokio::io::Interest;

use super::{AsyncUdpSocket, UdpSenderHelper, UdpSenderHelperSocket};

/// Register `sock` with the current tokio runtime.
pub(crate) fn wrap_udp_socket(sock: std::net::UdpSocket) -> io::Result<Box<dyn AsyncUdpSocket>> {
	Ok(Box::new(UdpSocket {
		inner: Arc::new(moq_sock::udp::UdpSocketState::new((&sock).into())?),
		io: Arc::new(tokio::net::UdpSocket::from_std(sock)?),
	}))
}

#[derive(Debug, Clone)]
struct UdpSocket {
	io: Arc<tokio::net::UdpSocket>,
	inner: Arc<moq_sock::udp::UdpSocketState>,
}

impl UdpSenderHelperSocket for UdpSocket {
	fn max_transmit_segments(&self) -> usize {
		self.inner.max_gso_segments()
	}

	fn try_send(&self, transmit: &moq_sock::udp::Transmit<'_>) -> io::Result<()> {
		self.io
			.try_io(Interest::WRITABLE, || self.inner.try_send((&self.io).into(), transmit))?;

		Ok(())
	}
}

impl AsyncUdpSocket for UdpSocket {
	fn create_sender(&self) -> Pin<Box<dyn super::UdpSender>> {
		Box::pin(UdpSenderHelper::new(self.clone(), |socket: &Self| {
			let socket = socket.clone();
			async move { socket.io.writable().await }
		}))
	}

	fn poll_recv(
		&mut self,
		cx: &mut Context<'_>,
		bufs: &mut [io::IoSliceMut<'_>],
		meta: &mut [moq_sock::udp::RecvMeta],
	) -> Poll<io::Result<usize>> {
		loop {
			ready!(self.io.poll_recv_ready(cx))?;
			match self
				.io
				.try_io(Interest::READABLE, || self.inner.recv((&self.io).into(), bufs, meta))
			{
				Ok(res) => return Poll::Ready(Ok(res)),
				// `try_io` clears readiness only for `WouldBlock`. Looping on any other
				// error would spin: readiness stays asserted, so `poll_recv_ready`
				// completes at once and the recv fails again, without ever yielding.
				Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
				Err(e) => return Poll::Ready(Err(e)),
			}
		}
	}

	fn local_addr(&self) -> io::Result<std::net::SocketAddr> {
		self.io.local_addr()
	}

	fn may_fragment(&self) -> bool {
		self.inner.may_fragment()
	}

	fn max_receive_segments(&self) -> usize {
		self.inner.gro_segments()
	}
}
