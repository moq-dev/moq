//! Adapt backend poll sessions to moq-net without changing thread affinity.
mod adapter;
pub use adapter::{Error, Grouped, GroupedSend, RecvStream, SendStream, Session};

use crate::quic;

impl Grouped for quic::web::Session {
	fn send_group(&mut self, priority: i32) -> Option<quic::SendGroup> {
		Some(quic::web::Session::send_group(self, priority))
	}
}

impl GroupedSend for quic::web::SendStream {
	type Group = quic::SendGroup;

	fn set_group(&mut self, group: &Self::Group) {
		quic::web::SendStream::set_group(self, Some(group));
	}

	fn set_order(&mut self, order: i64) {
		quic::web::SendStream::set_priority(self, order);
	}
}

impl moq_net::transport::poll::SendGroup for quic::SendGroup {
	fn set_priority(&mut self, priority: i32) {
		quic::SendGroup::set_priority(self, priority)
	}
}
