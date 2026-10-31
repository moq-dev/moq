//! Buffering unreliable datagrams

/// How much of each direction's unreliable datagrams to buffer
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Config {
	/// Maximum number of incoming application datagram bytes to buffer, or `None` to disable
	/// incoming datagrams
	///
	/// The peer is forbidden to send single datagrams larger than this size. If the aggregate size
	/// of all datagrams that have been received from the peer but not consumed by the application
	/// exceeds this value, old datagrams are dropped until it is no longer exceeded.
	///
	/// The amount of payload data buffered may be smaller than this due to overhead. Defaults to
	/// 1.25 MB.
	pub receive_buffer: Option<usize>,

	/// Maximum number of outgoing application datagram bytes to buffer
	///
	/// While datagrams are sent ASAP, it is possible for an application to generate data faster
	/// than the link, or even the underlying hardware, can transmit them. This limits the amount of
	/// memory that may be consumed in that case. When the send buffer is full and a new datagram is
	/// sent, the oldest datagram of the lowest-priority send group is dropped until the new
	/// one fits; see [`Datagrams::send`](crate::Datagrams::send).
	///
	/// Counts each datagram's frame bytes, so the payload buffered is a little smaller than this.
	/// Defaults to 1 MiB.
	pub send_buffer: usize,
}
