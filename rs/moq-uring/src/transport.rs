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
	fn poll_open_request(
		&mut self,
		cx: &mut std::task::Context<'_>,
	) -> std::task::Poll<Result<Option<web_transport_trait::poll::BiStreams<Self>>, Self::Error>> {
		Self::poll_open_request(self, cx)
	}

	// Leased in the QUIC stack, so the receive window bounds what an incomplete message holds.
	type Recv = quic::web::RecvStream;

	fn recv(stream: RecvStream<Self::RecvStream>) -> Self::Recv {
		stream.0
	}

	fn wire(&self) -> Option<moq_net::transport::Wire> {
		Some(quic::web::Session::wire(self))
	}

	fn probe(&self) -> Option<moq_net::transport::Probe> {
		quic::web::Session::probe(self)
	}

	fn streams_evicted(&self) -> Option<u64> {
		Some(quic::web::Session::streams_evicted(self))
	}

	fn send_group(&mut self, priority: i64) -> Option<quic::send::Group> {
		Some(quic::web::Session::send_group(self, priority))
	}

	fn send_datagram(
		&mut self,
		payload: bytes::Bytes,
		options: moq_net::transport::poll::SendOptions<'_, quic::send::Group>,
	) -> Result<(), Self::Error> {
		let options = quic::send::Options {
			group: options.group,
			priority: options.priority,
		};
		quic::web::Session::send_datagram(self, payload, options)
	}
}

#[cfg(feature = "quic")]
impl GroupedSend for quic::web::SendStream {
	type Group = quic::send::Group;

	fn set_group(&mut self, group: &Self::Group) {
		quic::web::SendStream::set_group(self, Some(group));
	}

	fn set_priority(&mut self, priority: i64) {
		quic::web::SendStream::set_priority(self, priority);
	}
}

#[cfg(feature = "quic")]
impl moq_net::transport::poll::SendGroup for quic::send::Group {
	fn set_priority(&mut self, priority: i64) {
		quic::send::Group::set_priority(self, priority)
	}
}

#[cfg(feature = "quic")]
impl moq_net::transport::poll::RecvStream for quic::web::RecvStream {
	type Error = adapter::Error<quic::Error>;
	type Lease<'a> = quic::recv::Lease<'a>;

	fn poll_lease(
		&mut self,
		waiter: &kio::Waiter,
		want: usize,
	) -> std::task::Poll<Result<Self::Lease<'_>, Self::Error>> {
		quic::web::RecvStream::poll_lease(self, &mut waiter.context(), want).map_err(adapter::Error)
	}
	fn set_priority(&mut self, priority: i64) {
		quic::web::RecvStream::set_priority(self, priority)
	}
	fn stop(&mut self, code: u32) {
		web_transport_trait::poll::RecvStream::stop(self, code)
	}
}

#[cfg(feature = "quic")]
impl moq_net::transport::poll::Lease for quic::recv::Lease<'_> {
	fn is_fin(&self) -> bool {
		quic::recv::Lease::is_fin(self)
	}
	fn chunks(&self) -> impl Iterator<Item = &[u8]> {
		quic::recv::Lease::chunks(self)
	}
	fn release(self) {
		quic::recv::Lease::release(self)
	}
}
