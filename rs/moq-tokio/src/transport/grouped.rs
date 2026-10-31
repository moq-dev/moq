//! Send-group, wire-counter, and probe hooks for the async backends.
//!
//! The traits are `pub` in this private module, so other crates can neither name nor
//! implement them: `pub(crate)` is refused (E0446) because the public adapter's
//! `type Group` projects through [`GroupedSend`].

use moq_net::transport::poll::SendOptions;

/// A backend session whose send streams may join send groups, as native QUIC's do.
///
/// The async interface has no groups, wire counters, or probe target, so moq-net's
/// [`send_group`](moq_net::transport::poll::Session::send_group),
/// [`wire`](moq_net::transport::poll::Session::wire), and
/// [`probe`](moq_net::transport::poll::Session::probe) reach a backend through this. A
/// backend without them implements it empty.
pub trait Grouped: web_transport_trait::Session<SendStream: GroupedSend> {
	/// The receive stream moq-net reads: the backend's own lease where it has one, or
	/// [`Buffered`](moq_net::transport::poll::Buffered) reads otherwise.
	type Recv: moq_net::transport::poll::RecvStream;

	/// Adapt one of the backend's receive streams, freshly accepted or opened.
	fn recv(stream: super::RecvStream<Self::RecvStream>) -> Self::Recv;

	/// Open a request, waiting for stream credit like any open unless the backend can inspect it.
	fn open_request(&self) -> impl std::future::Future<Output = super::RequestResult<Self>> + Send {
		async { self.open_bi().await.map(Some) }
	}

	/// The wire counters the backend updates as it sends and receives, or `None` when it has none.
	fn wire(&self) -> Option<moq_net::transport::Wire> {
		None
	}

	/// The target the backend's sender probes up to, or `None` when it cannot probe.
	fn probe(&self) -> Option<moq_net::transport::Probe> {
		None
	}

	/// Receive streams evicted to free a stuck receive window, or `None` when the backend never
	/// evicts.
	fn streams_evicted(&self) -> Option<u64> {
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
