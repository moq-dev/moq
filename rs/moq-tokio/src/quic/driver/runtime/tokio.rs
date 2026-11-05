use std::{
	io,
	task::{Context, Poll, ready},
};

use tokio::io::Interest;

use super::AsyncUdpSocket;

/// Register `sock` with the current tokio runtime, else the fallback.
pub(crate) fn wrap_udp_socket(sock: std::net::UdpSocket) -> io::Result<Box<dyn AsyncUdpSocket>> {
	let _runtime = crate::runtime::enter();
	Ok(Box::new(UdpSocket {
		inner: moq_sock::udp::UdpSocketState::new((&sock).into())?,
		io: tokio::net::UdpSocket::from_std(sock)?,
	}))
}

#[derive(Debug)]
struct UdpSocket {
	io: tokio::net::UdpSocket,
	inner: moq_sock::udp::UdpSocketState,
}

impl AsyncUdpSocket for UdpSocket {
	fn poll_recv(
		&self,
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

	fn try_send(&self, transmit: &moq_sock::udp::Transmit<'_>) -> io::Result<()> {
		self.io
			.try_io(Interest::WRITABLE, || self.inner.try_send((&self.io).into(), transmit))
	}

	fn poll_writable(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
		self.io.poll_send_ready(cx)
	}

	fn local_addr(&self) -> io::Result<std::net::SocketAddr> {
		self.io.local_addr()
	}

	fn max_receive_segments(&self) -> usize {
		self.inner.gro_segments()
	}

	fn max_transmit_segments(&self) -> usize {
		self.inner.max_gso_segments()
	}

	fn may_fragment(&self) -> bool {
		self.inner.may_fragment()
	}
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
	use std::net::SocketAddr;

	use super::*;

	/// A real socket hands a send that finds no route back to the connection rather than logging
	/// it as loss. Linux refuses a v4-mapped destination on an IPv6-only socket with
	/// `ENETUNREACH`, an unroutable family without touching the host's routes.
	#[tokio::test]
	async fn socket_returns_unreachable_sends() {
		let socket = socket2::Socket::new(
			socket2::Domain::IPV6,
			socket2::Type::DGRAM,
			Some(socket2::Protocol::UDP),
		)
		.unwrap();
		socket.set_only_v6(true).unwrap();
		socket.set_nonblocking(true).unwrap();
		socket
			.bind(&SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, 0)).into())
			.unwrap();
		let slot = super::super::Slot::new(wrap_udp_socket(socket.into()).unwrap());

		let transmit = moq_sock::udp::Transmit {
			destination: SocketAddr::new(std::net::Ipv4Addr::LOCALHOST.to_ipv6_mapped().into(), 1),
			ecn: None,
			contents: b"hello",
			segment_size: None,
			src_ip: None,
		};
		let result = std::future::poll_fn(|cx| slot.poll_send(&transmit, cx)).await;

		let err = result.expect_err("an unroutable send must reach the connection");
		assert_eq!(err.kind(), io::ErrorKind::NetworkUnreachable);
	}
}
