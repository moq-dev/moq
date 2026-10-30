//! Fuzz target bodies, plus the seeds they start from.
//!
//! Each `pub fn` here is one target: it takes raw fuzzer bytes and drives a parser or
//! a state machine with them. The bodies live in the crate rather than in
//! `fuzz/fuzz_targets/` because the frame decoder and the stream state are private,
//! and because the tests below replay them on stable, which is what turns a crash the
//! fuzzer found into a regression `just check` runs.
//!
//! `packet`, `params`, `streamid`, and `streams` are ported from upstream quinn's
//! `fuzz/fuzz_targets/`. Compiled only under `cfg(test)` or the `fuzz` feature, so none
//! of this is part of the published API. See `fuzz/README.md` for the workflow.
//!
//! [`Plaintext`] is the crypto the `connection` target and the benchmarks run on, and
//! [`SentPackets`] is the sent-packet storage its benchmark drives.

mod connection;
mod plaintext;

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddrV4, SocketAddrV6};

use arbitrary::{Arbitrary, Unstructured};
use bytes::{Bytes, BytesMut};
use rand::{Rng, SeedableRng, rngs::StdRng};

use crate::{
	ConnectionId, Dir, MAX_CID_SIZE, RESET_TOKEN_SIZE, SendStream, Side, StreamId, Streams, VarInt,
	coding::BufMutExt,
	connection::{Retransmits, State, StreamsState},
	frame::{self, Frame},
	packet::{FixedLengthConnectionIdParser, PartialDecode},
	transport_parameters::{PreferredAddress, TransportParameters},
};

#[cfg(feature = "fuzz")]
pub use crate::connection::BenchSentPackets as SentPackets;
pub use connection::connection;
pub use plaintext::Plaintext;

/// One fuzz target body: it returns whether the input got past the first parse, which
/// is what [`seeds`] is checked against.
pub type Target = fn(&[u8]) -> bool;

/// Every target, by the name its `fuzz/fuzz_targets/` shim and corpus directory use.
pub const TARGETS: &[(&str, Target)] = &[
	("packet", packet),
	("params", params),
	("streamid", streamid),
	("streams", streams),
	("frame", frame),
	("connection", connection),
];

/// A cid length byte, a grease byte, and a datagram: the unprotected packet header.
///
/// A decoded packet and the coalesced remainder together account for every input byte.
pub fn packet(data: &[u8]) -> bool {
	let [cid_len, grease, packet @ ..] = data else {
		return false;
	};
	let cid_len = *cid_len as usize % (MAX_CID_SIZE + 1);
	let len = packet.len();

	let Ok((decoded, rest)) = PartialDecode::new(
		BytesMut::from(packet),
		&FixedLengthConnectionIdParser::new(cid_len),
		grease & 1 == 1,
	) else {
		return false;
	};

	let rest = rest.map_or(0, |rest| rest.len());
	assert_eq!(len, decoded.len() + rest, "the decode lost bytes");
	true
}

/// A side byte and a transport parameter block.
///
/// Whatever decodes re-encodes to a block that decodes to the same parameters.
pub fn params(data: &[u8]) -> bool {
	let Some((side, mut data)) = data.split_first() else {
		return false;
	};
	let side = if side & 1 == 0 { Side::Client } else { Side::Server };

	let Ok(params) = TransportParameters::read(side, &mut data) else {
		return false;
	};

	let mut encoded = Vec::new();
	params.write(&mut encoded);
	let decoded =
		TransportParameters::read(side, &mut encoded.as_slice()).expect("re-encoded parameters did not decode");
	assert_eq!(params, decoded, "parameters changed across a re-encode");
	true
}

/// A stream id built from an initiator, a direction, and an index keeps all three.
pub fn streamid(data: &[u8]) -> bool {
	let mut u = Unstructured::new(data);
	let (Ok(side), Ok(dir), Ok(index)) = (
		u.arbitrary::<bool>(),
		u.arbitrary::<bool>(),
		// A stream id is a 62-bit varint whose low two bits carry side and direction.
		u.int_in_range(0..=(1u64 << 60) - 1),
	) else {
		return false;
	};
	let side = if side { Side::Server } else { Side::Client };
	let dir = if dir { Dir::Uni } else { Dir::Bi };

	let id = StreamId::new(side, dir, index);
	assert_eq!(id.initiator(), side);
	assert_eq!(id.dir(), dir);
	assert_eq!(id.index(), index);
	true
}

/// The limits a [`streams`] run starts from.
#[derive(Arbitrary, Debug)]
struct StreamLimits {
	server: bool,
	max_remote_uni: u16,
	max_remote_bi: u16,
	send_window: u16,
	receive_window: u16,
	stream_receive_window: u16,
	uni: bool,
}

/// One step of a [`streams`] run.
#[derive(Arbitrary, Debug)]
enum StreamOp {
	Open,
	Accept {
		uni: bool,
	},
	Finish {
		id: u64,
	},
	ReceivedStopSending {
		id: u64,
		error_code: u64,
	},
	ReceivedReset {
		id: u64,
		error_code: u64,
		final_offset: u64,
		reliable_size: Option<u64>,
	},
	Reset {
		id: u64,
	},
}

/// An op sequence against a bare `StreamsState`, without a connection around it.
pub fn streams(data: &[u8]) -> bool {
	let mut u = Unstructured::new(data);
	let Ok(limits) = StreamLimits::arbitrary(&mut u) else {
		return false;
	};

	let side = if limits.server { Side::Server } else { Side::Client };
	let dir = if limits.uni { Dir::Uni } else { Dir::Bi };
	let conn_state = State::Established;
	let mut pending = Retransmits::default();
	let mut state = StreamsState::new(
		side,
		limits.max_remote_uni.into(),
		limits.max_remote_bi.into(),
		limits.send_window.into(),
		limits.receive_window.into(),
		limits.stream_receive_window.into(),
	);

	while !u.is_empty() {
		let Ok(op) = StreamOp::arbitrary(&mut u) else {
			break;
		};
		match op {
			StreamOp::Open => {
				Streams::new(&mut state, &conn_state).open(dir, Default::default());
			}
			StreamOp::Accept { uni } => {
				let dir = if uni { Dir::Uni } else { Dir::Bi };
				Streams::new(&mut state, &conn_state).accept(dir);
			}
			StreamOp::Finish { id } => {
				let _ = SendStream::new(StreamId(id), &mut state, &mut pending, &conn_state).finish();
			}
			StreamOp::ReceivedStopSending { id, error_code } => {
				let _ = state.received_stop_sending(StreamId(id), varint(error_code));
			}
			StreamOp::ReceivedReset {
				id,
				error_code,
				final_offset,
				reliable_size,
			} => {
				let _ = state.received_reset(frame::ResetStream {
					id: StreamId(id),
					error_code: varint(error_code),
					final_offset: varint(final_offset),
					reliable_size: reliable_size.map(varint),
				});
			}
			StreamOp::Reset { id } => {
				let _ = SendStream::new(StreamId(id), &mut state, &mut pending, &conn_state).reset(0u32.into());
			}
		}
	}
	true
}

/// A decrypted packet payload: every frame `frame::Iter` yields, until one fails.
///
/// An ACK's ranges walk downward from its largest acknowledged packet without
/// overlapping, which is what the decoder's block scan promises the iterator.
pub fn frame(data: &[u8]) -> bool {
	let Ok(iter) = frame::Iter::new(Bytes::copy_from_slice(data)) else {
		return false;
	};

	let mut decoded = false;
	for frame in iter {
		let Ok(frame) = frame else {
			return false;
		};
		decoded = true;

		if let Frame::Ack(ack) = &frame {
			let mut floor = ack.largest + 1;
			for range in ack.iter() {
				assert!(range.end() < &floor, "ACK ranges overlap or ascend");
				floor = *range.start();
			}
		}
	}
	decoded
}

/// Truncate a fuzzer integer into the varint range.
fn varint(x: u64) -> VarInt {
	VarInt(x & VarInt::MAX.0)
}

/// A seed for one target.
pub struct Seed {
	/// The target whose corpus it belongs in.
	pub target: &'static str,
	/// The bytes to feed the target.
	pub data: Vec<u8>,
}

/// The inputs the fuzzer starts from.
///
/// Generated rather than committed so the corpus follows the code instead of rotting
/// next to it. `just rs fuzz` writes these out before each run; the tests below replay
/// them.
pub fn seeds() -> Vec<Seed> {
	let mut seeds = Vec::new();

	// Real datagrams from a handshake and a short exchange, so the packet target starts
	// from every header form the endpoints actually send.
	for datagram in connection::datagrams() {
		let mut data = vec![connection::CID_LEN as u8, 0];
		data.extend_from_slice(&datagram);
		seeds.push(Seed { target: "packet", data });
	}

	for side in [Side::Client, Side::Server] {
		let mut data = vec![side as u8];
		transport_parameters(side).write(&mut data);
		seeds.push(Seed { target: "params", data });
	}

	seeds.push(Seed {
		target: "streamid",
		data: vec![1, 0, 7, 0, 0, 0, 0, 0, 0, 0],
	});

	for ty in frame_types() {
		for fill in FRAME_FILLS {
			seeds.push(Seed {
				target: "frame",
				data: frame_seed(ty, fill),
			});
		}
	}

	// The op-sequence targets read `arbitrary` structures, which have no byte layout
	// worth writing by hand, so they start from noise of a few sizes.
	for target in ["streams", "connection"] {
		for (seed, len) in [64, 256, 1024].into_iter().enumerate() {
			let mut data = vec![0; len];
			StdRng::seed_from_u64(seed as u64).fill_bytes(&mut data);
			seeds.push(Seed { target, data });
		}
	}

	seeds
}

/// Parameters with every field set that `side` may receive: a client also gets the
/// server-only ones.
fn transport_parameters(side: Side) -> TransportParameters {
	let mut params = TransportParameters {
		initial_src_cid: Some(ConnectionId::new(&[0x01; 8])),
		initial_max_data: 1_000_000u32.into(),
		initial_max_stream_data_bidi_local: 100_000u32.into(),
		initial_max_stream_data_bidi_remote: 100_000u32.into(),
		initial_max_stream_data_uni: 100_000u32.into(),
		initial_max_streams_bidi: 16u32.into(),
		initial_max_streams_uni: 16u32.into(),
		max_idle_timeout: 30_000u32.into(),
		active_connection_id_limit: 4u32.into(),
		max_datagram_frame_size: Some(1200u32.into()),
		disable_active_migration: true,
		grease_quic_bit: true,
		min_ack_delay: Some(2_000u32.into()),
		reset_stream_at: true,
		..TransportParameters::default()
	};
	if side.is_client() {
		params.original_dst_cid = Some(ConnectionId::new(&[0x02; 8]));
		params.retry_src_cid = Some(ConnectionId::new(&[0x03; 8]));
		params.stateless_reset_token = Some([0xab; RESET_TOKEN_SIZE].into());
		params.preferred_address = Some(PreferredAddress {
			address_v4: Some(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 42)),
			address_v6: Some(SocketAddrV6::new(Ipv6Addr::LOCALHOST, 24, 0, 0)),
			connection_id: ConnectionId::new(&[0x04]),
			stateless_reset_token: [0xcd; RESET_TOKEN_SIZE].into(),
		});
	}
	params
}

/// Every frame type the decoder dispatches: RFC 9000's, RESET_STREAM_AT, IMMEDIATE_ACK,
/// DATAGRAM, and ACK_FREQUENCY.
fn frame_types() -> impl Iterator<Item = u64> {
	(0x00..=0x1f).chain([0x24, 0x30, 0x31, 0xaf])
}

/// Repeating one byte gives every field a small valid value: 0x00 an empty ACK block,
/// 0x01 a valid NEW_CONNECTION_ID length. The leftover bytes decode as PADDING or PING.
const FRAME_FILLS: [u8; 2] = [0x00, 0x01];

fn frame_seed(ty: u64, fill: u8) -> Vec<u8> {
	let mut data = Vec::new();
	data.write_var(ty);
	data.extend(std::iter::repeat_n(fill, 40));
	data
}

#[cfg(test)]
mod tests {
	use super::*;

	use std::collections::BTreeSet;

	fn target(name: &str) -> Target {
		TARGETS
			.iter()
			.find(|(target, _)| *target == name)
			.unwrap_or_else(|| panic!("no such target: {name}"))
			.1
	}

	/// Every seed runs, and every target has a seed that gets past its first parse.
	///
	/// The second half is the point: a corpus that parses nothing leaves the fuzzer
	/// mutating noise against an early `return`.
	#[test]
	fn seeds_reach_every_target() {
		let mut parsed = BTreeSet::new();
		for seed in seeds() {
			if target(seed.target)(&seed.data) {
				parsed.insert(seed.target);
			}
		}

		let missing: Vec<_> = TARGETS
			.iter()
			.map(|(name, _)| *name)
			.filter(|name| !parsed.contains(name))
			.collect();
		assert!(missing.is_empty(), "no seed parses for {missing:?}");
	}

	/// Every frame type the decoder knows has a seed that decodes.
	#[test]
	fn seeds_reach_every_frame() {
		let missing: Vec<_> = frame_types()
			.filter(|&ty| !FRAME_FILLS.iter().any(|&fill| frame(&frame_seed(ty, fill))))
			.collect();
		assert!(missing.is_empty(), "no frame seed decodes for {missing:02x?}");
	}

	/// Replay the crash inputs committed under `fuzz/regressions/`, so a bug the fuzzer
	/// found stays fixed on the stable toolchain, in the suite CI already runs.
	#[test]
	fn regressions() {
		let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fuzz/regressions");

		for (name, target) in TARGETS {
			// Nothing has crashed yet for this target; `fuzz/README.md` says what to
			// drop in here when something does.
			let Ok(entries) = std::fs::read_dir(root.join(name)) else {
				continue;
			};

			for entry in entries {
				let path = entry.expect("could not read the regression directory").path();
				if path.extension().is_some_and(|ext| ext == "md") {
					continue;
				}
				let data = std::fs::read(&path).expect("could not read a regression input");
				target(&data);
			}
		}
	}
}
