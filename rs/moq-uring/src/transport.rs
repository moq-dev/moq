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
