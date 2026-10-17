//! Adapt backend poll sessions to moq-net without changing thread affinity.
mod adapter;
mod local;
pub use adapter::{Error, RecvStream, SendStream, Session};
#[cfg(feature = "quic")]
use adapter::{Grouped, GroupedSend};
pub use local::Local;

#[cfg(feature = "quic")]
use crate::quic;

#[cfg(feature = "quic")]
impl Grouped for quic::web::Session {
	// Leased in the QUIC stack, so the receive window bounds what an incomplete message holds.
	type Recv = quic::web::RecvStream;

	fn recv(stream: RecvStream<Self::RecvStream>) -> Self::Recv {
		stream.0
	}

	fn wire(&self) -> Option<moq_net::transport::Wire> {
		Some(quic::web::Session::wire(self))
	}

	fn send_group(&mut self, priority: i64) -> Option<quic::SendGroup> {
		Some(quic::web::Session::send_group(self, priority))
	}

	fn send_datagram(
		&mut self,
		payload: bytes::Bytes,
		group: Option<&quic::SendGroup>,
		order: i64,
	) -> Result<(), Self::Error> {
		quic::web::Session::send_datagram(self, payload, group, order)
	}
}

#[cfg(feature = "quic")]
impl GroupedSend for quic::web::SendStream {
	type Group = quic::SendGroup;

	fn set_group(&mut self, group: &Self::Group) {
		quic::web::SendStream::set_group(self, Some(group));
	}

	fn set_priority(&mut self, priority: i64) {
		quic::web::SendStream::set_priority(self, priority);
	}
}

#[cfg(feature = "quic")]
impl moq_net::transport::poll::SendGroup for quic::SendGroup {
	fn set_priority(&mut self, priority: i64) {
		quic::SendGroup::set_priority(self, priority)
	}
}

#[cfg(feature = "quic")]
impl moq_net::transport::poll::RecvStream for quic::web::RecvStream {
	type Error = adapter::Error<quic::Error>;
	type Lease<'a> = quic::Lease<'a>;

	fn poll_lease(
		&mut self,
		cx: &mut std::task::Context<'_>,
		want: usize,
	) -> std::task::Poll<Result<Self::Lease<'_>, Self::Error>> {
		quic::web::RecvStream::poll_lease(self, cx, want).map_err(adapter::Error)
	}
	fn set_priority(&mut self, priority: i64) {
		quic::web::RecvStream::set_priority(self, priority)
	}
	fn stop(&mut self, code: u32) {
		web_transport_trait::poll::RecvStream::stop(self, code)
	}
}

#[cfg(feature = "quic")]
impl moq_net::transport::poll::Lease for quic::Lease<'_> {
	fn len(&self) -> usize {
		quic::Lease::len(self)
	}
	fn is_fin(&self) -> bool {
		quic::Lease::is_fin(self)
	}
	fn chunks(&self) -> impl Iterator<Item = &[u8]> {
		quic::Lease::chunks(self)
	}
	fn take(&mut self, max: usize) -> bytes::Bytes {
		quic::Lease::take(self, max)
	}
	fn consume(self, n: usize) {
		quic::Lease::consume(self, n)
	}
}
