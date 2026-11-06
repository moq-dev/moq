//! Scheduling proofs through the real send path: streams and datagrams queued into send
//! groups and drained into packets by `write_frames`, with loss and resets applied the way
//! the connection applies them.

use std::collections::HashMap;

use bytes::{BufMut, Bytes, BytesMut};

use super::state::StreamsState;
use super::{Streams, send};
use crate::{
	Dir, Side, StreamId, VarInt,
	connection::{State as ConnState, spaces::Retransmits},
	frame::{self, Frame, StreamMeta},
	transport_parameters::TransportParameters,
};

/// One frame of a packet, in the order it was written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sent {
	Stream(StreamId),
	/// A datagram, by the tag it was queued with.
	Datagram(u64),
}

const PACKET: usize = 1200;

/// How far two equally entitled groups may drift apart: a drain can stop between their
/// turns, one quantum (a packet) apart, and a turn may overdraw by up to a frame.
const SLACK: u64 = 2 * PACKET as u64;

/// A PING frame's one byte, standing in for the control frames ahead of the streams.
const PING: u8 = 0x01;

/// A sender with more queued than the path takes, drained a packet at a time.
struct Sender {
	state: StreamsState,
	pending: Retransmits,
	conn: ConnState,
	/// Payload bytes each stream got out.
	sent: HashMap<StreamId, u64>,
	/// Bytes each stream's frames took on the wire, headers included, which is what
	/// the round robins share.
	wire: HashMap<StreamId, u64>,
	/// Bytes each datagram tag took on the wire, headers included.
	datagram_wire: HashMap<u64, u64>,
	/// Every frame sent, in order.
	order: Vec<Sent>,
	/// The next datagram tag.
	tag: u64,
}

impl Sender {
	fn new() -> Self {
		Self::limited(VarInt::MAX)
	}

	/// With each stream's flow control window at `window` bytes.
	fn limited(window: VarInt) -> Self {
		let mut state = StreamsState::new(
			Side::Server,
			0u32.into(),
			0u32.into(),
			u64::MAX,
			VarInt::MAX,
			VarInt::MAX,
		);
		state.set_params(&TransportParameters {
			initial_max_streams_uni: VarInt::MAX,
			initial_max_data: VarInt::MAX,
			initial_max_stream_data_uni: window,
			..TransportParameters::default()
		});
		Self {
			state,
			pending: Retransmits::default(),
			conn: ConnState::Established,
			sent: HashMap::new(),
			wire: HashMap::new(),
			datagram_wire: HashMap::new(),
			order: Vec::new(),
			tag: 0,
		}
	}

	fn groups(&mut self) -> send::Groups<'_> {
		send::Groups { state: &mut self.state }
	}

	fn group(&mut self, priority: i64) -> send::Group {
		self.groups().open(priority)
	}

	fn send(&mut self, id: StreamId) -> send::Stream<'_> {
		send::Stream {
			id,
			state: &mut self.state,
			pending: &mut self.pending,
			conn_state: &self.conn,
		}
	}

	/// Open a stream in `group` at `priority` and queue `bytes` on it.
	fn open(&mut self, group: Option<&send::Group>, priority: i64, bytes: usize) -> StreamId {
		let id = Streams {
			state: &mut self.state,
			conn_state: &self.conn,
		}
		.open(Dir::Uni, send::Options { group, priority })
		.unwrap();
		self.write(id, bytes);
		id
	}

	fn write(&mut self, id: StreamId, bytes: usize) {
		assert_eq!(self.send(id).write(&mut &vec![0; bytes][..]), Ok(bytes));
	}

	/// Queue a `bytes` payload datagram in `group` at `priority`, returning its tag.
	fn datagram(&mut self, group: Option<&send::Group>, priority: i64, bytes: usize) -> u64 {
		let tag = self.tag;
		self.tag += 1;
		let mut data = BytesMut::with_capacity(bytes);
		data.put_u64(tag);
		data.resize(bytes, 0);
		let datagram = frame::OutgoingDatagram::new(data.freeze());
		assert!(
			self.state
				.queue_datagram(datagram, send::Options { group, priority }, usize::MAX)
		);
		tag
	}

	/// Send one packet, returning its stream frames.
	fn packet(&mut self) -> Vec<StreamMeta> {
		self.packet_of(PACKET)
	}

	/// Send one packet of `size` bytes, returning its stream frames.
	fn packet_of(&mut self, size: usize) -> Vec<StreamMeta> {
		self.packet_after(&[], size)
	}

	/// Send one packet of `size` bytes that already holds the control frames `prefix`,
	/// returning its stream frames.
	fn packet_after(&mut self, prefix: &[u8], size: usize) -> Vec<StreamMeta> {
		let mut buf = Vec::with_capacity(size);
		buf.extend_from_slice(prefix);
		let written = self.state.write_frames(&mut buf, 0..size, true);
		let frames = written.streams;

		// The packet's frames in order, and the wire bytes its datagrams took.
		let mut datagram_bytes = 0;
		let frames_in = match buf.is_empty() {
			true => None,
			false => Some(frame::Iter::new(Bytes::from(buf.clone())).unwrap()),
		};
		for frame in frames_in.into_iter().flatten() {
			match frame.unwrap() {
				Frame::Stream(stream) => self.order.push(Sent::Stream(stream.id)),
				Frame::Datagram(datagram) => {
					let wire = frame::OutgoingDatagram::new(datagram.data.clone()).size() as u64;
					let tag = u64::from_be_bytes(datagram.data[..8].try_into().unwrap());
					*self.datagram_wire.entry(tag).or_default() += wire;
					datagram_bytes += wire;
					self.order.push(Sent::Datagram(tag));
				}
				Frame::Ping => {}
				frame => panic!("unexpected {frame:?}"),
			}
		}
		let stream_bytes = (buf.len() - prefix.len()) as u64 - datagram_bytes;

		// Every frame carries its length but perhaps the last, which can run to the
		// end of the packet instead.
		let mut sizes: Vec<_> = frames
			.iter()
			.map(|frame| {
				let mut header = Vec::new();
				frame.encode(true, &mut header);
				(header.len() as u64, frame.offsets.end - frame.offsets.start)
			})
			.collect();
		if sizes.iter().map(|(h, p)| h + p).sum::<u64>() != stream_bytes {
			let (header, payload) = sizes.last_mut().unwrap();
			*header -= VarInt::from_u64(*payload).unwrap().size() as u64;
		}
		assert_eq!(sizes.iter().map(|(h, p)| h + p).sum::<u64>(), stream_bytes);

		for (frame, (header, payload)) in frames.iter().zip(sizes) {
			*self.sent.entry(frame.id).or_default() += payload;
			*self.wire.entry(frame.id).or_default() += header + payload;
		}
		frames.into_iter().collect()
	}

	/// Send `packets` packets, returning the stream of every frame in order.
	fn drain(&mut self, packets: usize) -> Vec<StreamId> {
		(0..packets).flat_map(|_| self.packet()).map(|frame| frame.id).collect()
	}

	fn bytes(&self, streams: &[StreamId]) -> u64 {
		streams.iter().map(|id| self.sent.get(id).copied().unwrap_or(0)).sum()
	}

	fn wire(&self, streams: &[StreamId]) -> u64 {
		streams.iter().map(|id| self.wire.get(id).copied().unwrap_or(0)).sum()
	}

	/// Send `packets` packets, returning every frame in order.
	fn drain_frames(&mut self, packets: usize) -> Vec<Sent> {
		let start = self.order.len();
		for _ in 0..packets {
			self.packet();
		}
		self.order[start..].to_vec()
	}

	fn datagram_wire(&self, tags: &[u64]) -> u64 {
		tags.iter()
			.map(|tag| self.datagram_wire.get(tag).copied().unwrap_or(0))
			.sum()
	}

	fn clear(&mut self) {
		self.sent.clear();
		self.wire.clear();
		self.datagram_wire.clear();
	}
}

fn assert_fair(a: u64, b: u64) {
	assert!(a.abs_diff(b) <= SLACK, "unfair split: {a} vs {b}");
	assert!(a + b > 0, "nothing sent");
}

/// The issue's Alice and Bob: two subscriptions at one priority, an audio track of
/// many small groups and a video track with a deep backlog of large ones. Each gets
/// half the bytes, not half the streams.
#[test]
fn equal_groups_share_bytes_not_streams() {
	let mut sender = Sender::new();
	let audio = sender.group(4);
	let video = sender.group(4);
	let audio: Vec<_> = (0..1_000).map(|seq| sender.open(Some(&audio), seq, 300)).collect();
	let video: Vec<_> = (0..4).map(|seq| sender.open(Some(&video), seq, 1_000_000)).collect();

	sender.drain(400);
	assert_fair(sender.wire(&audio), sender.wire(&video));
}

/// A group that keeps opening newer streams gains nothing over one draining a single
/// stream: new streams spend the group's credit rather than earning more.
#[test]
fn newer_streams_do_not_reset_a_groups_credit() {
	let mut sender = Sender::new();
	let busy = sender.group(0);
	let steady = sender.group(0);
	let mut busy_streams = vec![sender.open(Some(&busy), 0, 1_000_000)];
	let steady = [sender.open(Some(&steady), 0, 1_000_000)];

	for seq in 1..400 {
		busy_streams.push(sender.open(Some(&busy), seq, 2_000));
		sender.packet();
	}
	assert_fair(sender.wire(&busy_streams), sender.wire(&steady));
}

#[test]
fn higher_priority_preempts() {
	let mut sender = Sender::new();
	let high = sender.group(1);
	let low = sender.group(0);
	let low = sender.open(Some(&low), 0, 50_000);
	let high = sender.open(Some(&high), 0, 50_000);

	let order = sender.drain(100);
	let first_low = order.iter().position(|&id| id == low).unwrap();
	assert!(order[..first_low].iter().all(|&id| id == high));
	assert!(order[first_low..].iter().all(|&id| id == low));
	assert_eq!(sender.bytes(&[high, low]), 100_000);
}

#[test]
fn priority_updates_apply_to_queued_data() {
	let mut sender = Sender::new();
	let high = sender.group(1);
	let low = sender.group(0);
	let low_stream = sender.open(Some(&low), 0, 50_000);
	let high_stream = sender.open(Some(&high), 0, 50_000);
	assert!(sender.drain(5).iter().all(|&id| id == high_stream));

	sender.groups().set_priority(&low, 2);
	assert_eq!(sender.groups().priority(&low), 2);
	assert!(sender.drain(5).iter().all(|&id| id == low_stream));

	sender.groups().set_priority(&low, 1);
	sender.clear();
	sender.drain(20);
	assert_fair(sender.wire(&[low_stream]), sender.wire(&[high_stream]));
}

/// Each group sends its own newest stream first, whatever the other groups' numbering.
#[test]
fn each_group_sends_in_its_own_order() {
	let mut sender = Sender::new();
	let descending = sender.group(0);
	let ascending = sender.group(0);
	let newest: Vec<_> = [3, 1, 4, 2]
		.into_iter()
		.map(|seq| (seq, sender.open(Some(&descending), seq, 3_000)))
		.collect();
	// An oldest-first subscription negates its sequences.
	let oldest: Vec<_> = [7, 5, 6]
		.into_iter()
		.map(|seq| (seq, sender.open(Some(&ascending), -seq, 3_000)))
		.collect();

	let order = sender.drain(100);
	let finished = |streams: &[(i64, StreamId)]| {
		let mut last: Vec<_> = streams
			.iter()
			.map(|&(seq, id)| (order.iter().rposition(|&x| x == id).unwrap(), seq))
			.collect();
		last.sort();
		last.into_iter().map(|(_, seq)| seq).collect::<Vec<_>>()
	};
	assert_eq!(finished(&newest), [4, 3, 2, 1]);
	assert_eq!(finished(&oldest), [5, 6, 7]);
}

/// Group order takes full 62-bit group sequences and sparse jumps between them,
/// which a 32-bit scalar shared with the track priority would truncate or wrap.
#[test]
fn group_order_holds_wide_and_sparse_sequences() {
	let mut sender = Sender::new();
	let group = sender.group(0);
	let seqs = [0, 1 << 40, (1 << 62) - 1, 7, 1 << 33, (1 << 32) + 1, 1 << 32];
	let mut streams: Vec<_> = seqs
		.into_iter()
		.map(|seq| (seq, sender.open(Some(&group), seq, 100)))
		.collect();

	let order = sender.drain(10);
	streams.sort_by_key(|&(seq, _)| std::cmp::Reverse(seq));
	let expected: Vec<_> = streams.into_iter().map(|(_, id)| id).collect();
	assert_eq!(order, expected);
}

/// A stream blocked by flow control holds no turn and saves up no credit while it
/// waits, so its group neither stalls the other nor bursts once unblocked.
#[test]
fn blocked_streams_hold_no_turn() {
	let mut sender = Sender::limited(5_000u32.into());
	let blocked = sender.group(0);
	let open = sender.group(0);
	let blocked = sender.open(Some(&blocked), 0, 5_000);
	let open = sender.open(Some(&open), 0, 5_000);
	let unlimited = 1 << 60;
	sender.state.received_max_stream_data(open, unlimited).unwrap();
	sender.write(open, 1_000_000);

	sender.drain(50);
	assert_eq!(sender.bytes(&[blocked]), 5_000);
	assert!(sender.bytes(&[open]) > 50_000);

	sender.state.received_max_stream_data(blocked, unlimited).unwrap();
	sender.write(blocked, 1_000_000);
	sender.clear();
	sender.drain(100);
	assert_fair(sender.wire(&[blocked]), sender.wire(&[open]));
}

/// A lost frame retransmits through its group, so new data from an equal group
/// delays it by a turn at most.
#[test]
fn retransmits_are_not_starved_by_equal_groups() {
	let mut sender = Sender::new();
	let lossy = sender.group(0);
	let busy = sender.group(0);
	let lossy = sender.open(Some(&lossy), 0, 1_000);
	let busy = sender.open(Some(&busy), 0, 10_000_000);

	let lost = sender.packet().into_iter().find(|frame| frame.id == lossy).unwrap();
	sender.drain(10);
	sender.state.retransmit(lost);
	sender.clear();
	sender.drain(2);
	assert_eq!(sender.bytes(&[lossy]), 1_000);
	assert!(sender.bytes(&[busy]) > 0);
}

/// Streams outside every group, such as control streams, go before all of them.
#[test]
fn ungrouped_streams_preempt_groups() {
	let mut sender = Sender::new();
	let group = sender.group(i64::MAX);
	let grouped = sender.open(Some(&group), i64::MAX, 10_000);
	let control = sender.open(None, i64::MIN, 10_000);

	let order = sender.drain(20);
	let first = order.iter().position(|&id| id == grouped).unwrap();
	assert!(order[..first].iter().all(|&id| id == control));
	assert_eq!(sender.bytes(&[control]), 10_000);
}

/// A released group lives until its last stream is gone, here by a reset, and then
/// leaves nothing behind in the schedule.
#[test]
fn reset_frees_a_released_group() {
	let mut sender = Sender::new();
	let group = sender.group(0);
	let reset = sender.open(Some(&group), 0, 10_000);
	let finished = sender.open(Some(&group), 1, 100);
	sender.send(finished).finish().unwrap();
	sender.groups().close(group);

	assert_eq!(sender.drain(1)[0], finished);
	sender.send(reset).reset(0u32.into()).unwrap();
	assert!(!sender.state.can_send_stream_data());
	assert!(sender.drain(1).is_empty());

	sender.state.received_ack_of(StreamMeta {
		id: finished,
		offsets: 0..100,
		fin: true,
	});
	assert!(!sender.state.pending.is_clean());
	sender.state.reset_acked(reset, 0);
	assert!(sender.state.pending.is_clean());
}

/// A reset that still delivers its first bytes keeps the stream in its group for just
/// that prefix, and the released group is freed once the reset is acknowledged.
#[test]
fn reliable_reset_sends_its_prefix_then_frees_its_group() {
	let mut sender = Sender::new();
	let group = sender.group(0);
	let reset = sender.open(Some(&group), 0, 10_000);
	let finished = sender.open(Some(&group), 0, 10_000);
	sender.send(finished).finish().unwrap();
	sender.groups().close(group);

	let mut stream = sender.send(reset);
	let at = stream.offset().rewind(3_000);
	stream.reset_at(at, 0u32.into()).unwrap();
	assert!(sender.state.can_send_stream_data());
	sender.drain(100);
	assert_eq!(sender.bytes(&[reset]), 3_000, "only the reliable prefix");
	assert_eq!(sender.bytes(&[finished]), 10_000);
	assert!(!sender.state.can_send_stream_data());

	sender.state.received_ack_of(StreamMeta {
		id: finished,
		offsets: 0..10_000,
		fin: true,
	});
	sender.state.received_ack_of(StreamMeta {
		id: reset,
		offsets: 0..3_000,
		fin: false,
	});
	assert!(!sender.state.pending.is_clean(), "the reset is unacknowledged");
	sender.state.reset_acked(reset, 3_000);
	assert!(sender.state.pending.is_clean());
}

/// STOP_SENDING abandons the prefix a reset still owed, so the stream leaves the
/// schedule rather than linger in it.
#[test]
fn stop_sending_drops_a_reliable_prefix_from_the_schedule() {
	let mut sender = Sender::new();
	let group = sender.group(0);
	let reset = sender.open(Some(&group), 0, 10_000);
	let mut stream = sender.send(reset);
	let at = stream.offset().rewind(3_000);
	stream.reset_at(at, 0u32.into()).unwrap();
	assert!(sender.state.can_send_stream_data());

	assert_eq!(sender.state.received_stop_sending(reset, 0u32.into()), Ok(Some(0)));
	assert!(!sender.state.can_send_stream_data());
	assert!(sender.drain(1).is_empty());
}

/// A stream can leave its group for the ungrouped queue, keeping its data queued.
#[test]
fn streams_can_leave_their_group() {
	let mut sender = Sender::new();
	let group = sender.group(0);
	let id = sender.open(Some(&group), 0, 100);
	sender.send(id).set_group(None).unwrap();
	sender.groups().close(group);
	assert!(sender.state.pending.is_clean());
	assert_eq!(sender.drain(1), [id]);
}

/// A low-priority datagram track waits behind a higher-priority stream rather than
/// going out ahead of every stream.
#[test]
fn low_priority_datagrams_wait_behind_streams() {
	let mut sender = Sender::new();
	let high = sender.group(1);
	let low = sender.group(0);
	let tags: Vec<_> = (0..20).map(|seq| sender.datagram(Some(&low), seq, 1_000)).collect();
	let stream = sender.open(Some(&high), 0, 20_000);

	let order = sender.drain_frames(100);
	let first = order.iter().position(|sent| matches!(sent, Sent::Datagram(_))).unwrap();
	assert!(order[..first].iter().all(|&sent| sent == Sent::Stream(stream)));
	assert!(order[first..].iter().all(|sent| matches!(sent, Sent::Datagram(_))));
	assert_eq!(sender.bytes(&[stream]), 20_000);
	assert_eq!(order[first..].len(), tags.len());
}

/// A high-priority datagram track still goes ahead of lower-priority streams.
#[test]
fn high_priority_datagrams_preempt_streams() {
	let mut sender = Sender::new();
	let high = sender.group(1);
	let low = sender.group(0);
	let stream = sender.open(Some(&low), 0, 20_000);
	let tags: Vec<_> = (0..20).map(|seq| sender.datagram(Some(&high), seq, 1_000)).collect();

	let order = sender.drain_frames(100);
	// Newest first, as the group orders them.
	let expected: Vec<_> = tags.iter().rev().map(|&tag| Sent::Datagram(tag)).collect();
	assert_eq!(order[..tags.len()], expected[..]);
	assert!(order[tags.len()..].iter().all(|&sent| sent == Sent::Stream(stream)));
}

/// Inside one group a datagram merges into the group order: it waits behind a stream
/// for a newer group and goes ahead of a stream of its own group.
#[test]
fn datagrams_merge_into_their_group_order() {
	let mut sender = Sender::new();
	let group = sender.group(0);
	let older = sender.datagram(Some(&group), 10, 100);
	let newer = sender.open(Some(&group), 11, 3_000);
	let tied = sender.open(Some(&group), 12, 3_000);
	let ahead = sender.datagram(Some(&group), 12, 100);

	let order = sender.drain_frames(20);
	let position = |sent| order.iter().position(|&x| x == sent).unwrap();
	let last = |sent| order.iter().rposition(|&x| x == sent).unwrap();
	assert_eq!(position(Sent::Datagram(ahead)), 0);
	assert!(last(Sent::Stream(tied)) < position(Sent::Stream(newer)));
	assert!(last(Sent::Stream(newer)) < position(Sent::Datagram(older)));
}

/// A group's datagrams spend its fair share, so sending datagrams buys no extra bytes over
/// an equal group of streams.
#[test]
fn datagrams_and_streams_share_a_groups_credit() {
	let mut sender = Sender::new();
	let datagrams = sender.group(0);
	let streams = sender.group(0);
	let tags: Vec<_> = (0..1_000)
		.map(|seq| sender.datagram(Some(&datagrams), seq, 500))
		.collect();
	let stream = sender.open(Some(&streams), 0, 1_000_000);

	sender.drain(200);
	assert_fair(sender.datagram_wire(&tags), sender.wire(&[stream]));
}

/// A datagram outside every group goes ahead of every group, like a control stream.
#[test]
fn ungrouped_datagrams_preempt_groups() {
	let mut sender = Sender::new();
	let group = sender.group(i64::MAX);
	let stream = sender.open(Some(&group), i64::MAX, 3_000);
	let tag = sender.datagram(None, 0, 100);

	let order = sender.drain_frames(5);
	assert_eq!(order[0], Sent::Datagram(tag));
	assert!(order[1..].iter().all(|&sent| sent == Sent::Stream(stream)));
}

/// A datagram too large for the room left in a packet ends it and leads the next one,
/// rather than letting what queues behind it go first.
#[test]
fn a_datagram_that_does_not_fit_leads_the_next_packet() {
	let mut sender = Sender::new();
	let group = sender.group(0);
	let stream = sender.open(Some(&group), 0, 100);
	let small = sender.datagram(Some(&group), 2, 300);
	let large = sender.datagram(Some(&group), 1, 1_000);

	assert_eq!(sender.drain_frames(1), [Sent::Datagram(small)]);
	assert_eq!(sender.drain_frames(1), [Sent::Datagram(large), Sent::Stream(stream)]);
	assert!(!sender.state.can_send_stream_data());
}

/// A packet clamped below a queued datagram, such as a loss probe, still carries what fits
/// behind it, even behind its own control frames, and the datagram keeps its place.
#[test]
fn a_clamped_packet_sends_what_fits() {
	let mut sender = Sender::new();
	let group = sender.group(0);
	let large = sender.datagram(Some(&group), 1, 1_000);
	assert!(!sender.state.can_send_frames(500));
	let stream = sender.open(Some(&group), 0, 2_000);
	assert!(sender.state.can_send_frames(500));

	sender.packet_after(&[PING], 500);
	assert_eq!(sender.order, [Sent::Stream(stream)]);
	assert_eq!(sender.drain_frames(1)[0], Sent::Datagram(large));
}

/// A packet already holding a control frame is not empty, so a datagram that no longer
/// fits ends it rather than letting lower-ranked data ahead.
#[test]
fn a_datagram_behind_control_frames_ends_the_packet() {
	let mut sender = Sender::new();
	let group = sender.group(0);
	let large = sender.datagram(Some(&group), 1, 1_000);
	sender.open(Some(&group), 0, 2_000);

	// The DATAGRAM frame (type, two-byte length, payload) would fill the packet alone,
	// but a PING is already in it.
	sender.packet_after(&[PING], 1 + 2 + 1_000);
	assert_eq!(sender.order, []);
	assert_eq!(sender.drain_frames(1)[0], Sent::Datagram(large));
}

/// A released group keeps its queued datagrams and frees once they are sent.
#[test]
fn a_released_group_sends_its_datagrams() {
	let mut sender = Sender::new();
	let group = sender.group(0);
	let tag = sender.datagram(Some(&group), 0, 100);
	sender.groups().close(group);
	assert!(!sender.state.pending.is_clean());
	assert_eq!(sender.drain_frames(1), [Sent::Datagram(tag)]);
	assert!(sender.state.pending.is_clean());
}
