//! Experimental forward error correction: repair packets the peer rebuilds lost packets from

/// Experimental forward error correction for 1-RTT packets
///
/// A sender follows each burst of packets that carry stream data with repair packets, and the
/// receiver rebuilds a lost packet from them without waiting for its retransmission. The
/// receiver never acknowledges a packet it rebuilt, so the sender still counts the loss and
/// retransmits the data, which the receiver then drops as a duplicate.
///
/// An experiment: the wire format is private to this branch, starts from
/// [draft-zheng-quic-fec-extension](https://datatracker.ietf.org/doc/draft-zheng-quic-fec-extension/)
/// and departs from it (`README.md`, "Forward error correction").
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Config {
	/// The most packets one repair may cover for this endpoint to rebuild from it, which it
	/// tells the peer; it holds the payloads of twice as many received packets
	///
	/// Defaults to 64.
	pub window: u8,

	/// What to send a peer that rebuilds packets, or `None` to send no repairs
	pub send: Option<Send>,
}

impl Default for Config {
	fn default() -> Self {
		Self {
			window: 64,
			send: Some(Send::default()),
		}
	}
}

/// The repairs an endpoint sends
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Send {
	/// How repair symbols are computed
	pub code: Code,

	/// The most packets a block holds before it closes; the peer's window caps it
	///
	/// A block also closes when there is no more stream data to send, so a burst shorter than
	/// this is a block of its own. Defaults to 32.
	pub block: u8,

	/// Repair packets per hundred packets of a block, rounded to the nearest and never fewer
	/// than one a block
	///
	/// [`Code::Xor`] sends one repair a block whatever this says. Defaults to 20.
	pub overhead: u8,
}

impl Default for Send {
	fn default() -> Self {
		Self {
			code: Code::ReedSolomon,
			block: 32,
			overhead: 20,
		}
	}
}

/// How repair symbols are computed from a block's packets
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Code {
	/// One repair, the XOR of the block, which rebuilds one lost packet
	Xor,
	/// Reed-Solomon over GF(2^8) with a Cauchy matrix: any `n` repairs rebuild any `n` lost
	/// packets of the block
	ReedSolomon,
}
