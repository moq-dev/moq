use std::collections::VecDeque;

use crate::Duration;
use crate::connection::spaces::PendingAcks;
use crate::frame::AckFrequency;
use crate::transport_parameters::TransportParameters;
use crate::{TIMER_GRANULARITY, TransportError, VarInt, transport::ack_frequency};

/// State associated to ACK frequency
pub(super) struct AckFrequencyState {
	//
	// Sending ACK_FREQUENCY frames
	//
	/// Packet numbers and requested `max_ack_delay`s of the frames sent after the newest one
	/// acknowledged and not declared lost, oldest first
	///
	/// Only packets in flight hold one, so the congestion window bounds it.
	in_flight: VecDeque<(u64, Duration)>,
	/// The longest delay requested by a frame declared lost after the newest one acknowledged,
	/// with the newest such frame's packet number
	///
	/// The peer may have received it, so it binds until a later frame is acknowledged.
	lost: Option<(u64, Duration)>,
	next_outgoing_sequence_number: VarInt,
	pub(super) peer_max_ack_delay: Duration,

	//
	// Receiving ACK_FREQUENCY frames
	//
	last_ack_frequency_frame: Option<u64>,
	pub(super) max_ack_delay: Duration,
}

impl AckFrequencyState {
	pub(super) fn new(default_max_ack_delay: Duration) -> Self {
		Self {
			in_flight: VecDeque::new(),
			lost: None,
			next_outgoing_sequence_number: VarInt(0),
			peer_max_ack_delay: default_max_ack_delay,

			last_ack_frequency_frame: None,
			max_ack_delay: default_max_ack_delay,
		}
	}

	/// Returns the `max_ack_delay` that should be requested of the peer when sending an
	/// ACK_FREQUENCY frame
	pub(super) fn candidate_max_ack_delay(
		&self,
		rtt: Duration,
		config: &ack_frequency::Config,
		peer_params: &TransportParameters,
	) -> Duration {
		let min_ack_delay = Duration::from_micros(peer_params.min_ack_delay.map_or(0, |x| x.into()));

		// Use the peer's max_ack_delay transport parameter if no custom max_ack_delay was provided
		// in the config, rather than the delay last requested, which would only ever shrink. A
		// configured one is capped below the protocol limit, which the peer's own `max_ack_delay`
		// and `min_ack_delay` transport parameters were already validated against.
		let candidate = config
			.max_ack_delay
			.map_or(Duration::from_millis(peer_params.max_ack_delay.into_inner()), |delay| {
				Ord::min(delay, MAX_ACK_DELAY_LIMIT - Duration::from_micros(1))
			});

		// Must be no smaller than the `min_ack_delay` advertised by the peer
		Ord::max(
			// Should be less than the RTT (itself at least `MIN_AUTOMATIC_ACK_DELAY`)
			Ord::min(candidate, Ord::max(rtt, MIN_AUTOMATIC_ACK_DELAY)),
			min_ack_delay,
		)
	}

	/// Returns the `max_ack_delay` for the purposes of calculating the PTO
	///
	/// This `max_ack_delay` is defined as the maximum of the peer's current `max_ack_delay` and all
	/// unacknowledged `max_ack_delay`s (i.e. proposed values that the peer might already use).
	pub(super) fn max_ack_delay_for_pto(&self) -> Duration {
		self.in_flight
			.iter()
			.map(|&(_, delay)| delay)
			.chain(self.lost.map(|(_, delay)| delay))
			.fold(self.peer_max_ack_delay, Ord::max)
	}

	/// Returns the next sequence number for an ACK_FREQUENCY frame
	pub(super) fn next_sequence_number(&mut self) -> VarInt {
		assert!(self.next_outgoing_sequence_number <= VarInt::MAX);

		let seq = self.next_outgoing_sequence_number;
		self.next_outgoing_sequence_number.0 += 1;
		seq
	}

	/// Returns true if we should send an ACK_FREQUENCY frame
	pub(super) fn should_send_ack_frequency(
		&self,
		rtt: Duration,
		config: &ack_frequency::Config,
		peer_params: &TransportParameters,
	) -> bool {
		if self.next_outgoing_sequence_number.0 == 0 {
			// Always send at startup
			return true;
		}
		let current = self
			.in_flight
			.back()
			.map_or(self.peer_max_ack_delay, |&(_, pending)| pending);
		let desired = self.candidate_max_ack_delay(rtt, config, peer_params);
		let error = (desired.as_secs_f32() / current.as_secs_f32()) - 1.0;
		error.abs() > MAX_RTT_ERROR
	}

	/// Notifies the [`AckFrequencyState`] that a packet containing an ACK_FREQUENCY frame was sent
	pub(super) fn ack_frequency_sent(&mut self, pn: u64, requested_max_ack_delay: Duration) {
		self.in_flight.push_back((pn, requested_max_ack_delay));
	}

	/// Notifies the [`AckFrequencyState`] that the 1-RTT packet `pn`, which carried an
	/// ACK_FREQUENCY frame, was acknowledged
	///
	/// The peer ignores a frame older than one it has seen, so every older request is void.
	pub(super) fn on_acked(&mut self, pn: u64) {
		// Absent when a newer frame was acknowledged first
		let Some(i) = self.in_flight.iter().position(|&(number, _)| number == pn) else {
			return;
		};
		self.peer_max_ack_delay = self.in_flight[i].1;
		self.in_flight.drain(..=i);
		if self.lost.is_some_and(|(number, _)| number < pn) {
			self.lost = None;
		}
	}

	/// Notifies the [`AckFrequencyState`] that the 1-RTT packet `pn`, which carried an
	/// ACK_FREQUENCY frame, was declared lost
	pub(super) fn on_lost(&mut self, pn: u64) {
		let Some(i) = self.in_flight.iter().position(|&(number, _)| number == pn) else {
			return;
		};
		let (_, delay) = self.in_flight.remove(i).expect("found above");
		self.lost = Some(
			self.lost
				.map_or((pn, delay), |(number, lost)| (number.max(pn), lost.max(delay))),
		);
	}

	/// Notifies the [`AckFrequencyState`] that an ACK_FREQUENCY frame was received
	///
	/// Updates the endpoint's params according to the payload of the ACK_FREQUENCY frame, or
	/// returns an error in case the requested `max_ack_delay` is invalid.
	///
	/// Returns `true` if the frame was processed and `false` if it was ignored because of being
	/// stale.
	pub(super) fn ack_frequency_received(
		&mut self,
		frame: &AckFrequency,
		pending_acks: &mut PendingAcks,
	) -> Result<bool, TransportError> {
		if self
			.last_ack_frequency_frame
			.is_some_and(|highest_sequence_nr| frame.sequence.into_inner() <= highest_sequence_nr)
		{
			return Ok(false);
		}

		self.last_ack_frequency_frame = Some(frame.sequence.into_inner());

		// Update max_ack_delay
		let max_ack_delay = Duration::from_micros(frame.request_max_ack_delay.into_inner());
		if max_ack_delay < TIMER_GRANULARITY {
			return Err(TransportError::PROTOCOL_VIOLATION(
				"Requested Max Ack Delay in ACK_FREQUENCY frame is less than min_ack_delay",
			));
		}
		// As for the max_ack_delay transport parameter (RFC 9000 section 18.2), 2^14 ms or more
		// is invalid
		// https://www.ietf.org/archive/id/draft-ietf-quic-ack-frequency-14.html#section-4
		if max_ack_delay >= MAX_ACK_DELAY_LIMIT {
			return Err(TransportError::PROTOCOL_VIOLATION(
				"Requested Max Ack Delay in ACK_FREQUENCY frame is 2^14 ms or more",
			));
		}
		self.max_ack_delay = max_ack_delay;

		// Update the rest of the params
		pending_acks.set_ack_frequency_params(frame);

		Ok(true)
	}
}

/// Maximum proportion difference between the most recently requested max ACK delay and the
/// currently desired one before a new request is sent, when the peer supports the ACK frequency
/// extension and an explicit max ACK delay is not configured.
const MAX_RTT_ERROR: f32 = 0.2;

/// Exclusive upper bound on a Requested Max Ack Delay, as for the `max_ack_delay` transport
/// parameter
const MAX_ACK_DELAY_LIMIT: Duration = Duration::from_millis(1 << 14);

/// Minimum value to request the peer set max ACK delay to when the peer supports the ACK frequency
/// extension and an explicit max ACK delay is not configured.
// Keep in sync with `ack_frequency::Config::max_ack_delay` documentation
const MIN_AUTOMATIC_ACK_DELAY: Duration = Duration::from_millis(25);

#[cfg(test)]
mod tests {
	use super::*;
	use crate::Side;

	#[test]
	fn peer_min_ack_delay_can_exceed_rtt() {
		let params = TransportParameters {
			min_ack_delay: Some(100_000u32.into()),
			max_ack_delay: 100u32.into(),
			..TransportParameters::default()
		};
		let mut encoded = Vec::new();
		params.write(&mut encoded);
		let params = TransportParameters::read(Side::Client, &mut encoded.as_slice())
			.expect("peer parameters must pass wire-level validation");
		let state = AckFrequencyState::new(Duration::from_millis(100));
		let delay = state.candidate_max_ack_delay(Duration::from_millis(5), &ack_frequency::Config::default(), &params);
		assert_eq!(delay, Duration::from_millis(100));
	}

	#[test]
	fn peer_min_ack_delay_can_exceed_rtt_with_large_max() {
		let params = TransportParameters {
			min_ack_delay: Some(100_000u32.into()),
			max_ack_delay: 200u32.into(),
			..TransportParameters::default()
		};
		let mut encoded = Vec::new();
		params.write(&mut encoded);
		let params = TransportParameters::read(Side::Client, &mut encoded.as_slice())
			.expect("peer parameters must pass wire-level validation");
		let state = AckFrequencyState::new(Duration::from_millis(100));
		let delay = state.candidate_max_ack_delay(Duration::from_millis(5), &ack_frequency::Config::default(), &params);
		assert_eq!(delay, Duration::from_millis(100));
	}

	/// A configured delay on a long RTT is requested just below the protocol limit
	#[test]
	fn configured_max_ack_delay_stays_below_limit() {
		let state = AckFrequencyState::new(Duration::from_millis(25));
		let config = ack_frequency::Config {
			max_ack_delay: Some(MAX_ACK_DELAY_LIMIT),
			..ack_frequency::Config::default()
		};
		let delay = state.candidate_max_ack_delay(MAX_ACK_DELAY_LIMIT * 2, &config, &TransportParameters::default());
		assert_eq!(delay, MAX_ACK_DELAY_LIMIT - Duration::from_micros(1));
	}

	/// Against quicly's max_ack_delay and min_ack_delay, both 25 ms, the request is never below
	/// 25 ms, and a longer configured delay is still capped at the RTT
	#[test]
	fn peer_min_ack_delay_of_25ms() {
		let params = TransportParameters {
			min_ack_delay: Some(25_000u32.into()),
			max_ack_delay: 25u32.into(),
			..TransportParameters::default()
		};
		let state = AckFrequencyState::new(Duration::from_millis(25));
		let ms = Duration::from_millis;
		for (configured, rtt, requested) in [
			(None, 0, 25),
			(None, 200, 25),
			(Some(1), 0, 25),
			(Some(1), 200, 25),
			(Some(100), 0, 25),
			(Some(100), 50, 50),
			(Some(100), 200, 100),
		] {
			let config = ack_frequency::Config {
				max_ack_delay: configured.map(ms),
				..ack_frequency::Config::default()
			};
			let delay = state.candidate_max_ack_delay(ms(rtt), &config, &params);
			assert_eq!(delay, ms(requested), "configured {configured:?} ms, RTT {rtt} ms");
		}
	}

	/// Without a configured delay, a request starts from the peer's `max_ack_delay` transport
	/// parameter, not the delay last requested, so a request shrunk by a short RTT grows back with
	/// a longer one
	#[test]
	fn request_grows_back_with_the_rtt() {
		let params = TransportParameters {
			max_ack_delay: 100u32.into(),
			..TransportParameters::default()
		};
		let config = ack_frequency::Config::default();
		let mut state = AckFrequencyState::new(Duration::from_millis(100));
		let short = state.candidate_max_ack_delay(Duration::from_millis(30), &config, &params);
		assert_eq!(short, Duration::from_millis(30));
		state.ack_frequency_sent(1, short);
		state.on_acked(1);
		let long = state.candidate_max_ack_delay(Duration::from_millis(300), &config, &params);
		assert_eq!(long, Duration::from_millis(100));
	}

	/// The PTO counts every delay the peer may be using: the one acknowledged last and each
	/// requested since, sent or lost, until a later request is acknowledged
	#[test]
	fn pto_counts_every_unacknowledged_request() {
		let ms = Duration::from_millis;
		let mut state = AckFrequencyState::new(ms(25));
		state.ack_frequency_sent(1, ms(100));
		state.ack_frequency_sent(2, ms(40));
		assert_eq!(state.max_ack_delay_for_pto(), ms(100));
		// The newer request settles the older one
		state.on_acked(2);
		assert_eq!(state.peer_max_ack_delay, ms(40));
		assert_eq!(state.max_ack_delay_for_pto(), ms(40));
		state.on_acked(1);
		assert_eq!(state.peer_max_ack_delay, ms(40));

		// A lost request may still have arrived
		state.ack_frequency_sent(3, ms(200));
		state.on_lost(3);
		state.ack_frequency_sent(4, ms(30));
		assert_eq!(state.max_ack_delay_for_pto(), ms(200));
		state.on_acked(4);
		assert_eq!(state.max_ack_delay_for_pto(), ms(30));
	}
}
