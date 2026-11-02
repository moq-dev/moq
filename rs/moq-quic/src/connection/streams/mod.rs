use std::task::Poll;

use tracing::trace;

use super::spaces::{Retransmits, ThinRetransmits};
use crate::{Dir, StreamId, VarInt, connection::Error, frame};

mod lease;
pub mod recv;
use recv::Recv;

#[cfg(test)]
mod saturated;
mod schedule;
use schedule::{Next, Schedule};

pub mod send;
use send::{Send, SendState};

mod state;
pub(crate) use state::StreamsState;

/// Access to streams
pub struct Streams<'a> {
	pub(super) state: &'a mut StreamsState,
	pub(super) conn_state: &'a super::State,
}

impl<'a> Streams<'a> {
	/// Drive a bare `StreamsState` without a connection, for the `streams` fuzz target
	#[cfg(any(test, feature = "fuzz"))]
	pub(crate) fn new(state: &'a mut StreamsState, conn_state: &'a super::State) -> Self {
		Self { state, conn_state }
	}

	/// Open a single stream if possible, its sending half scheduled by `options`
	///
	/// Returns `None` if the streams in the given direction are currently exhausted.
	///
	/// # Panics
	/// - when `options.group` was opened on another connection
	pub fn open(&mut self, dir: Dir, options: send::Options<'_>) -> Option<StreamId> {
		let group = options.group.map(|g| g.index(self.state.tag));
		if self.conn_state.is_closed() {
			return None;
		}

		if self.state.next[dir as usize] >= self.state.max[dir as usize] {
			self.state.streams_blocked[dir as usize] = true;
			self.state.open_refused[dir as usize] = true;
			return None;
		}

		self.state.next[dir as usize] += 1;
		let id = StreamId::new(self.state.side, dir, self.state.next[dir as usize] - 1);
		self.state.insert_local(id);
		self.state.send_streams += 1;

		// Send state is allocated on first use, so the default schedule needs none.
		if group.is_some() || options.priority != 0 {
			let max_send_data = self.state.max_send_data(id);
			let stream = self.state.send.get_mut(&id).unwrap().insert(Send::new(max_send_data));
			stream.priority = options.priority;
			self.state.pending.join(id, &mut stream.slot, group, stream.priority);
		}
		Some(id)
	}

	/// Accept a remotely initiated stream of a certain directionality, if possible
	///
	/// Returns `None` if there are no new incoming streams for this connection.
	/// Has no impact on the data flow-control or stream concurrency limits.
	pub fn accept(&mut self, dir: Dir) -> Option<StreamId> {
		if self.state.next_remote[dir as usize] == self.state.next_reported_remote[dir as usize] {
			return None;
		}

		let x = self.state.next_reported_remote[dir as usize];
		self.state.next_reported_remote[dir as usize] = x + 1;
		if dir == Dir::Bi {
			self.state.send_streams += 1;
		}

		Some(StreamId::new(!self.state.side, dir, x))
	}

	/// [`Self::open`], or wait for the peer to allow another stream
	///
	/// Pending registers `waiter` until the peer raises the stream limit in `dir`. Fails once
	/// the connection is lost.
	///
	/// # Panics
	/// - when `options.group` was opened on another connection
	pub fn poll_open(
		&mut self,
		waiter: &kio::Waiter,
		dir: Dir,
		options: send::Options<'_>,
	) -> Poll<Result<StreamId, Error>> {
		if let Some(err) = self.state.waits.lost() {
			return Poll::Ready(Err(err.clone()));
		}
		match self.open(dir, options) {
			Some(id) => Poll::Ready(Ok(id)),
			None => {
				self.state.waits.register_open(dir, waiter);
				Poll::Pending
			}
		}
	}

	/// [`Self::accept`], or wait for the peer to open a stream
	///
	/// Pending registers `waiter` until the peer opens a stream in `dir`. Streams the peer opened
	/// stay acceptable after the connection is lost, and only then does this fail.
	pub fn poll_accept(&mut self, waiter: &kio::Waiter, dir: Dir) -> Poll<Result<StreamId, Error>> {
		if let Some(id) = self.accept(dir) {
			return Poll::Ready(Ok(id));
		}
		if let Some(err) = self.state.waits.lost() {
			return Poll::Ready(Err(err.clone()));
		}
		self.state.waits.register_accept(dir, waiter);
		Poll::Pending
	}

	/// The number of streams that may have unacknowledged data.
	pub fn send_streams(&self) -> usize {
		self.state.send_streams
	}

	/// The number of remotely initiated open streams of a certain directionality.
	///
	/// Includes remotely initiated streams, which have not been accepted via [`accept`](Self::accept).
	/// These streams count against the configured concurrency limit.
	pub fn remote_open_streams(&self, dir: Dir) -> u64 {
		// total opened - total closed = total opened - ( total permitted - total permitted unclosed )
		self.state.next_remote[dir as usize]
			- (self.state.max_remote[dir as usize] - self.state.allocated_remote_count[dir as usize])
	}
}

impl StreamsState {
	/// Queue a datagram scheduled by `options`, within `budget` bytes of queued datagrams.
	///
	/// Returns whether it stayed queued rather than being evicted itself.
	///
	/// # Panics
	/// - when `options.group` was opened on another connection
	pub(crate) fn queue_datagram(
		&mut self,
		datagram: frame::OutgoingDatagram,
		options: send::Options<'_>,
		budget: usize,
	) -> bool {
		let group = options.group.map(|g| g.index(self.tag));
		let frame = datagram.size();
		self.pending
			.push_datagram(group, options.priority, datagram, frame, budget)
	}

	/// Drop queued datagrams with a payload over `max_payload` bytes, returning whether any was.
	pub(crate) fn drop_oversized_datagrams(&mut self, max_payload: usize) -> bool {
		self.pending.retain_datagrams(|datagram| {
			let keep = datagram.len() <= max_payload;
			if !keep {
				trace!(
					"dropping {} byte datagram violating {} byte limit",
					datagram.len(),
					max_payload
				);
			}
			keep
		})
	}

	/// Frame bytes queued datagrams hold against the send buffer.
	pub(crate) fn datagram_bytes(&self) -> usize {
		self.pending.datagram_bytes()
	}
}

/// Application events about streams
#[derive(Debug, PartialEq, Eq)]
pub enum StreamEvent {
	/// One or more new streams has been opened and can be accepted
	///
	/// A separate [`StreamEvent::Readable`] is emitted once data arrives.
	Opened {
		/// Directionality for which streams have been opened
		dir: Dir,
	},
	/// A currently open stream likely has data or errors waiting to be read
	Readable {
		/// Which stream is now readable
		id: StreamId,
	},
	/// A formerly write-blocked stream might be ready for a write or have been stopped
	///
	/// Only generated for streams that are currently open.
	Writable {
		/// Which stream is now writable
		id: StreamId,
	},
	/// A finished stream has been fully acknowledged or stopped
	Finished {
		/// Which stream has been finished
		id: StreamId,
	},
	/// The answer changed for the lowest offset a [`send::Stream::acked`] call is waiting for
	///
	/// The stream's contiguous acknowledged prefix reached it, or a reset or STOP_SENDING may have
	/// failed any waiter. Every waiter on the stream polls again: one at or below `offset` is
	/// acknowledged, and one past it learns its error or registers again. Reported before a [`StreamEvent::Finished`] from the same ACK.
	/// Not reported when the connection closes, which fails [`send::Stream::acked`] instead.
	Acked {
		/// Which stream was acknowledged
		id: StreamId,
		/// The end of the stream's contiguous acknowledged prefix
		offset: u64,
	},
	/// The peer asked us to stop sending on an outgoing stream
	Stopped {
		/// Which stream has been stopped
		id: StreamId,
		/// Error code supplied by the peer
		error_code: VarInt,
	},
	/// At least one new stream of a certain directionality may be opened
	Available {
		/// Directionality for which streams are newly available
		dir: Dir,
	},
}

/// Indicates whether a frame needs to be transmitted
///
/// This type wraps around bool and uses the `#[must_use]` attribute in order
/// to prevent accidental loss of the frame transmission requirement.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
#[must_use = "A frame might need to be enqueued"]
pub struct ShouldTransmit(bool);

impl ShouldTransmit {
	/// Returns whether a frame should be transmitted
	pub fn should_transmit(self) -> bool {
		self.0
	}
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
enum StreamHalf {
	Send,
	Recv,
}
