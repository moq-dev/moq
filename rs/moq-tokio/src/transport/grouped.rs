//! Send-group hooks for the async backends.
//!
//! The traits are `pub` in this private module, so other crates can neither name nor
//! implement them: `pub(crate)` is refused (E0446) because the public adapter's
//! `type Group` projects through [`GroupedSend`].

/// A backend session whose send streams may join send groups, as native QUIC's do.
///
/// The async interface has no groups, so moq-net's
/// [`send_group`](moq_net::transport::poll::Session::send_group) reaches a backend through
/// this. A backend without groups implements it empty.
pub trait Grouped: web_transport_trait::Session<SendStream: GroupedSend> {
	/// Open a send group at `priority`, or `None` when the backend has none.
	fn send_group(&self, priority: i64) -> Option<<Self::SendStream as GroupedSend>::Group> {
		let _ = priority;
		None
	}

	/// Send a datagram in `group` at `order`; a backend without groups ignores both.
	fn send_datagram(
		&self,
		payload: bytes::Bytes,
		group: Option<&<Self::SendStream as GroupedSend>::Group>,
		order: i64,
	) -> Result<(), Self::Error> {
		let _ = (group, order);
		web_transport_trait::Session::send_datagram(self, payload)
	}
}

/// A backend send stream that may join its [`Grouped`] session's send groups.
pub trait GroupedSend: web_transport_trait::SendStream {
	/// The backend's send group, [`Ungrouped`](moq_net::transport::poll::Ungrouped) when it has none.
	type Group: moq_net::transport::poll::SendGroup + Send + Sync;

	/// Move the stream into `group`.
	fn set_group(&mut self, group: &Self::Group);

	/// Set the priority within the stream's send group, or session-wide when ungrouped.
	///
	/// Saturated into the async interface's narrower priority by default.
	fn set_priority(&mut self, priority: i64) {
		web_transport_trait::SendStream::set_priority(self, priority.clamp(i32::MIN.into(), i32::MAX.into()) as i32);
	}
}
