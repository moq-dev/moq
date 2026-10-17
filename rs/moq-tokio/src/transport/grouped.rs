//! Send-group and wire-counter hooks for the async backends.
//!
//! The traits are `pub` in this private module, so other crates can neither name nor
//! implement them: `pub(crate)` is refused (E0446) because the public adapter's
//! `type Group` projects through [`GroupedSend`].

use moq_net::transport::poll::SendOptions;

/// A backend session whose send streams may join send groups, as native QUIC's do.
///
/// The async interface has no groups or wire counters, so moq-net's
/// [`send_group`](moq_net::transport::poll::Session::send_group) and
/// [`wire`](moq_net::transport::poll::Session::wire) reach a backend through this. A
/// backend without either implements it empty.
pub trait Grouped: web_transport_trait::Session<SendStream: GroupedSend> {
	/// The wire counters the backend updates as it sends and receives, or `None` when it has none.
	fn wire(&self) -> Option<moq_net::transport::Wire> {
		None
	}

	/// Open a send group at `priority`, or `None` when the backend has none.
	fn send_group(&self, priority: i64) -> Option<<Self::SendStream as GroupedSend>::Group> {
		let _ = priority;
		None
	}

	/// Send a datagram scheduled by `options`; a backend without groups ignores them.
	fn send_datagram(
		&self,
		payload: bytes::Bytes,
		options: SendOptions<'_, <Self::SendStream as GroupedSend>::Group>,
	) -> Result<(), Self::Error> {
		let _ = options;
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
