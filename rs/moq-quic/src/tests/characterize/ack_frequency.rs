//! What the connection sends and how it acknowledges under draft-ietf-quic-ack-frequency-14,
//! including against peers that read the frame differently (see the crate README)

use super::*;

/// The ACK_FREQUENCY frames the server receives while `drive` runs
fn sent_ack_frequency(pair: &mut Pair, drive: impl FnOnce(&mut Pair)) -> Vec<frame::AckFrequency> {
	pair.server.capture_inbound_packets = true;
	drive(pair);
	pair.server.capture_inbound_packets = false;
	let mut sent = Vec::new();
	for payload in mem::take(&mut pair.server.captured_packets) {
		for frame in frame::Iter::new(payload.into()).unwrap() {
			if let Frame::AckFrequency(frame) = frame.unwrap() {
				sent.push(frame);
			}
		}
	}
	sent
}

/// Whether the server acknowledges each of `pings` client PINGs as it arrives, each in its own
/// packet
fn server_acks(pair: &mut Pair, client_ch: ConnectionHandle, server_ch: ConnectionHandle, pings: usize) -> Vec<bool> {
	(0..pings)
		.map(|_| {
			pair.client_conn_mut(client_ch).ping();
			pair.drive_client();
			let before = pair.server_conn_mut(server_ch).stats().frame_tx.acks;
			pair.drive_server();
			pair.drive_client();
			pair.server_conn_mut(server_ch).stats().frame_tx.acks > before
		})
		.collect()
}

/// A client asking the server for `config`
fn client_asking(config: transport::ack_frequency::Config) -> client::Config {
	client_with(|transport| {
		transport.ack_frequency = Some(config);
	})
}

/// Against a peer advertising a min_ack_delay of 25 ms, as quicly does, a configured delay below it
/// is requested as 25 ms, which a peer refusing anything lower accepts. The thresholds keep their
/// defaults: an ACK every other ack-eliciting packet (Ack-Eliciting Threshold 1, not draft-00's
/// "every packet"), and a Reordering Threshold equal to the packet threshold, as draft-14 section 9
/// recommends.
#[test]
fn request_respects_peer_min_ack_delay() {
	for configured in [1, 10] {
		let _guard = subscribe();
		let mut pair = pair_with(|_| {});
		let config = transport::ack_frequency::Config {
			max_ack_delay: Some(Duration::from_millis(configured)),
			..transport::ack_frequency::Config::default()
		};
		let (client_ch, server_ch) = pair.connect_with(client_asking(config));

		pair.client_conn_mut(client_ch)
			.set_peer_min_ack_delay(Duration::from_millis(25));
		pair.client_conn_mut(client_ch).ping();
		let sent = sent_ack_frequency(&mut pair, Pair::drive);

		let expected = frame::AckFrequency {
			sequence: VarInt(1),
			ack_eliciting_threshold: VarInt(1),
			request_max_ack_delay: VarInt(25_000),
			reordering_threshold: VarInt(3),
		};
		assert_eq!(sent, [expected], "configured {configured} ms");
		assert_eq!(lost(pair.server_conn_mut(server_ch)), None);
		assert_eq!(lost(pair.client_conn_mut(client_ch)), None);
	}
}

/// Whether the server acknowledges each of `pings` in-order ack-eliciting packets as it arrives,
/// after the client asks it for an Ack-Eliciting Threshold of `threshold`, or asks nothing
fn acks_with_threshold(threshold: Option<u64>, pings: usize) -> Vec<bool> {
	let _guard = subscribe();
	let (mut pair, client_ch, server_ch) = connected();

	// The Requested Max Ack Delay is the default 25 ms, and a Reordering Threshold of 0 ignores
	// reordering. The IMMEDIATE_ACK gets this packet acknowledged, so the count starts at zero.
	let mut frames = Frames::default();
	if let Some(threshold) = threshold {
		frames = frames.var(FrameType::ACK_FREQUENCY, &[0, threshold, 25_000, 0]);
	}
	client_sends(
		&mut pair,
		client_ch,
		SpaceId::Data,
		frames.var(FrameType::IMMEDIATE_ACK, &[]),
	);
	pair.drive_server();
	pair.drive_client();

	// The clock stands still, so only the threshold can elicit an ACK
	server_acks(&mut pair, client_ch, server_ch, pings)
}

/// draft-14 section 4: a receiver SHOULD send an ACK "after receiving more than this many
/// ack-eliciting packets", so a threshold T acknowledges every (T + 1)th packet. draft-00 and
/// quicly's packet tolerance acknowledge every Tth packet instead.
#[test]
fn threshold_acks_after_more_than_threshold() {
	assert_eq!(acks_with_threshold(Some(0), 3), [true, true, true]);
	assert_eq!(acks_with_threshold(Some(1), 4), [false, true, false, true]);
	assert_eq!(
		acks_with_threshold(Some(2), 6),
		[false, false, true, false, false, true]
	);
	assert_eq!(acks_with_threshold(Some(3), 4), [false, false, false, true]);
}

/// Before any ACK_FREQUENCY, every other ack-eliciting packet is acknowledged (RFC 9000 section
/// 13.2.2), which draft-14 section 4 says "corresponds to a value of 1"
#[test]
fn threshold_default_is_one() {
	assert_eq!(acks_with_threshold(None, 4), acks_with_threshold(Some(1), 4));
}

/// With the default Reordering Threshold, the first ACK a gap elicits lets the sender declare the
/// missing packet lost by packet threshold (RFC 9002 section 6.1.1), instead of arriving one
/// packet too early to, as draft-14 section 9 warns
#[test]
fn reordering_ack_declares_loss() {
	let _guard = subscribe();
	let mut pair = pair_with(|_| {});
	let config = transport::ack_frequency::Config {
		// High enough that only the gap elicits an ACK
		ack_eliciting_threshold: VarInt(10),
		..transport::ack_frequency::Config::default()
	};
	let (client_ch, server_ch) = pair.connect_with(client_asking(config));

	// An ACK of a few pings gives the gap a Largest Acked to measure from
	for _ in 0..4 {
		pair.client_conn_mut(client_ch).ping();
		pair.drive_client();
	}
	pair.drive();

	pair.mtu = 0;
	pair.client_conn_mut(client_ch).ping();
	pair.drive_client();
	pair.mtu = DEFAULT_MTU;

	let lost_before = pair.client_conn_mut(client_ch).stats().path.lost_packets;
	assert_eq!(server_acks(&mut pair, client_ch, server_ch, 3), [false, false, true]);
	assert_eq!(
		pair.client_conn_mut(client_ch).stats().path.lost_packets - lost_before,
		1
	);
}
