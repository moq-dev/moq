use std::{collections::VecDeque, task::Poll};

use bytes::{Buf, Bytes};
use tracing::{debug, trace};

use super::{Connection, Error, send};
use crate::{
	TransportError,
	frame::{Datagram, FrameStruct, OutgoingDatagram},
};

/// API to control datagram traffic
pub struct Datagrams<'a> {
	pub(super) conn: &'a mut Connection,
}

impl Datagrams<'_> {
	/// Queue an unreliable datagram, scheduled by `options`
	///
	/// The datagram is scheduled like one of its group's streams at its priority: it waits
	/// behind higher priorities, goes ahead of a stream of equal priority, and spends the
	/// group's share of bytes. Without a group, it is ranked among the ungrouped streams, which
	/// go before every group.
	///
	/// Sending never blocks. While queued datagrams overflow
	/// [`datagram::Config::send_buffer`](crate::transport::datagram::Config::send_buffer),
	/// the oldest datagram of the lowest-priority group is dropped, and this datagram is dropped
	/// instead once it is the one to go.
	///
	/// Each chunk of `data` is taken with [`Buf::copy_to_bytes`], so a header chained in front
	/// of a [`Bytes`] payload costs no copy of the payload.
	///
	/// Fails with [`send::Error::Unsupported`] when datagrams are disabled locally or by the peer,
	/// or [`send::Error::TooLarge`] when `data` exceeds [`max_size`](Self::max_size).
	///
	/// # Panics
	/// - when `options.group` was opened on another connection
	pub fn send(&mut self, data: impl Buf, options: send::Options<'_>) -> Result<(), send::Error> {
		if self.conn.config.datagram.receive_buffer.is_none() {
			return Err(send::Error::Unsupported);
		}
		let max = self.max_size().ok_or(send::Error::Unsupported)?;
		let budget = self.conn.config.datagram.send_buffer;
		// Checked before taking the payload, which copies whatever is not already `Bytes`.
		let len = data.remaining();
		if len > max || OutgoingDatagram::frame_size(len) > budget {
			return Err(send::Error::TooLarge);
		}
		let datagram = OutgoingDatagram::new(data);
		if !self.conn.streams.queue_datagram(datagram, options, budget) {
			trace!("dropping outgoing datagram");
		}
		Ok(())
	}

	/// Discard queued datagrams that no longer fit the active path.
	pub(super) fn drop_oversized(&mut self) {
		let Some(max_datagram_size) = self.max_size() else {
			return;
		};
		self.conn.streams.drop_oversized_datagrams(max_datagram_size);
	}

	/// Compute the maximum size of datagrams that may be passed to [`send`](Self::send)
	///
	/// Returns `None` if datagrams are unsupported by the peer or disabled locally.
	///
	/// This may change over the lifetime of a connection according to variation in the path MTU
	/// estimate. The peer can also enforce an arbitrarily small fixed limit, but if the peer's
	/// limit is large this is guaranteed to be a little over a kilobyte at minimum.
	///
	/// Not necessarily the maximum size of received datagrams.
	pub fn max_size(&self) -> Option<usize> {
		// We use the conservative overhead bound for any packet number, reducing the budget by at
		// most 3 bytes, so that PN size fluctuations don't cause users sending maximum-size
		// datagrams to suffer avoidable packet loss.
		let max_size = Datagram::max_payload(
			(self.conn.path.current_mtu() as usize - self.conn.predict_1rtt_overhead(None)) as u64,
		)?;
		let limit = Datagram::max_payload(self.conn.peer_params.max_datagram_frame_size?.into_inner())?;
		Some(limit.min(max_size) as usize)
	}

	/// Receive an unreliable, unordered datagram
	pub fn recv(&mut self) -> Option<Bytes> {
		self.conn.datagrams.recv()
	}

	/// [`Self::recv`], or wait for a datagram to arrive
	///
	/// Datagrams that arrived stay receivable after the connection is lost, and only then does
	/// this fail.
	pub fn poll_recv(&mut self, waiter: &kio::Waiter) -> Poll<Result<Bytes, Error>> {
		if let Some(datagram) = self.recv() {
			return Poll::Ready(Ok(datagram));
		}
		let waits = &mut self.conn.streams.waits;
		if let Some(err) = waits.lost() {
			return Poll::Ready(Err(err.clone()));
		}
		waits.register_datagram(waiter);
		Poll::Pending
	}

	/// Bytes available in the outgoing datagram buffer
	///
	/// When greater than zero, [`send`](Self::send)ing a datagram of at most this size is
	/// guaranteed not to cause queued datagrams to be dropped.
	pub fn send_buffer_space(&self) -> usize {
		self.conn
			.config
			.datagram
			.send_buffer
			.saturating_sub(self.conn.streams.datagram_bytes())
			.saturating_sub(Datagram::SIZE_BOUND)
	}
}

/// Incoming datagrams; outgoing ones are scheduled with the streams.
#[derive(Default)]
pub(super) struct DatagramState {
	pub(super) incoming: DatagramBuffer,
}

impl DatagramState {
	pub(super) fn received(&mut self, datagram: Datagram, window: &Option<usize>) -> Result<bool, TransportError> {
		let window = match window {
			None => {
				return Err(TransportError::PROTOCOL_VIOLATION("unexpected DATAGRAM frame"));
			}
			Some(x) => *x,
		};

		let size_with_overhead = datagram.data.len() + size_of::<Datagram>();

		if size_with_overhead > window {
			// A valid frame can still exceed the local memory budget because of queue metadata.
			debug!("dropping datagram exceeding receive memory budget");
			return Ok(false);
		}

		let was_empty = self.incoming.is_empty();
		while self.incoming.memory_used() + size_with_overhead > window {
			debug!("dropping stale datagram");
			self.recv();
		}

		// The window charges only the payload, but a slice keeps its whole receive buffer alive,
		// so a peer sending tiny datagrams would otherwise hold a buffer for each.
		self.incoming.push_back(Datagram {
			data: Bytes::copy_from_slice(&datagram.data),
		});
		Ok(was_empty)
	}

	pub(super) fn recv(&mut self) -> Option<Bytes> {
		let x = self.incoming.pop_front()?.data;
		Some(x)
	}
}

#[derive(Default)]
pub(super) struct DatagramBuffer {
	queue: VecDeque<Datagram>,
	payload_bytes: usize,
}

impl DatagramBuffer {
	fn push_back(&mut self, datagram: Datagram) {
		self.payload_bytes += datagram.data.len();
		self.queue.push_back(datagram);
	}

	fn pop_front(&mut self) -> Option<Datagram> {
		let datagram = self.queue.pop_front()?;
		self.payload_bytes -= datagram.data.len();
		Some(datagram)
	}

	fn memory_used(&self) -> usize {
		self.payload_bytes
			.saturating_add(self.queue.len() * size_of::<Datagram>())
	}

	pub(super) fn is_empty(&self) -> bool {
		self.queue.is_empty()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn empty_frame_flood_limit() {
		let mut state = DatagramState::default();
		let datagram = Datagram { data: Bytes::new() };
		let window = 100;
		loop {
			let initial_count = state.incoming.queue.len();
			state.received(datagram.clone(), &Some(window)).unwrap();
			assert!(state.incoming.queue.len() * size_of::<Datagram>() <= window);
			if state.incoming.queue.len() == initial_count {
				// Datagrams are getting dropped
				break;
			}
		}
	}

	/// The queue charges a datagram its own bytes, so however many tiny datagrams fill it, none
	/// keeps alive the receive batch it arrived in.
	#[test]
	fn tiny_datagrams_do_not_pin_their_batches() {
		const WINDOW: usize = 1024;
		let mut state = DatagramState::default();
		// Far past the window, so stale datagrams are dropped along the way.
		let batches: Vec<Bytes> = (0..WINDOW).map(|_| Bytes::from(vec![7u8; 4096])).collect();
		for batch in &batches {
			let datagram = Datagram { data: batch.slice(..1) };
			state.received(datagram, &Some(WINDOW)).unwrap();
		}
		assert!(
			batches.into_iter().all(|batch| batch.is_unique()),
			"a queued datagram still points into its batch"
		);
		assert!(state.incoming.memory_used() <= WINDOW);
		assert_eq!(state.recv().unwrap(), &[7][..]);
	}
}
