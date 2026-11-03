//! Controlling the peer's acknowledgement frequency

use crate::{Duration, VarInt};

/// Parameters for controlling the peer's acknowledgement frequency
///
/// The parameters provided in this config will be sent to the peer at the beginning of the
/// connection, so it can take them into account when sending acknowledgements (see each parameter's
/// description for details on how it influences acknowledgement frequency).
///
/// The implementation follows
/// [draft-ietf-quic-ack-frequency-14](https://www.ietf.org/archive/id/draft-ietf-quic-ack-frequency-14.html),
/// whose codepoints date from draft-07. The defaults produce behavior slightly different than the
/// behavior without this extension, because they change the way reordered packets are handled
/// (see [`reordering_threshold`](Self::reordering_threshold) for details).
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Config {
	/// The ack-eliciting threshold we will request the peer to use
	///
	/// This threshold represents the number of ack-eliciting packets an endpoint may receive
	/// without immediately sending an ACK.
	///
	/// The remote peer should send at least one ACK frame when more than this number of
	/// ack-eliciting packets have been received. A value of 0 results in a receiver immediately
	/// acknowledging every ack-eliciting packet.
	///
	/// Defaults to 1, which sends ACK frames for every other ack-eliciting packet. A peer
	/// following draft-00, or quicly's packet tolerance, acknowledges once this many have arrived
	/// instead, so it reads 1 as every packet.
	pub ack_eliciting_threshold: VarInt,

	/// The `max_ack_delay` we will request the peer to use
	///
	/// This parameter represents the maximum amount of time that an endpoint waits before sending
	/// an ACK when the ack-eliciting threshold hasn't been reached.
	///
	/// The effective `max_ack_delay` will be clamped to be at least the peer's `min_ack_delay`
	/// transport parameter, at most the greater of the current path RTT or 25ms, and below the
	/// protocol limit of 2^14 ms.
	///
	/// Defaults to `None`, in which case the peer's original `max_ack_delay` will be used, as
	/// obtained from its transport parameters.
	pub max_ack_delay: Option<Duration>,

	/// The reordering threshold we will request the peer to use
	///
	/// This threshold represents the amount of out-of-order packets that will trigger an endpoint
	/// to send an ACK, without waiting for `ack_eliciting_threshold` to be exceeded or for
	/// `max_ack_delay` to be elapsed.
	///
	/// A value of 0 indicates out-of-order packets do not elicit an immediate ACK. A value of 1
	/// immediately acknowledges any packets that are received out of order (this is also the
	/// behavior when the extension is disabled).
	///
	/// It is recommended to set this value to
	/// [`packet_threshold`](super::Config::packet_threshold), so the ACK a gap elicits lets us
	/// declare the missing packet lost; a smaller value elicits it a packet too early. Since the
	/// default packet threshold is 3, this value defaults to 3.
	pub reordering_threshold: VarInt,
}

impl Default for Config {
	fn default() -> Self {
		Self {
			ack_eliciting_threshold: VarInt(1),
			max_ack_delay: None,
			reordering_threshold: VarInt(3),
		}
	}
}
