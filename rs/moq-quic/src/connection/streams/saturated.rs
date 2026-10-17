//! Scheduling proofs through the real send path: streams written into send groups and
//! drained into packets by `write_stream_frames`, with loss and resets applied the way
//! the connection applies them.

use std::collections::HashMap;

use super::state::StreamsState;
use super::{SendDomain, SendGroup, SendGroups, SendStream, Streams};
use crate::{
    Dir, Side, StreamId, VarInt,
    connection::{State as ConnState, spaces::Retransmits},
    frame::StreamMeta,
    transport_parameters::TransportParameters,
};

const PACKET: usize = 1200;

/// How far two equally entitled groups may drift apart: each round robin may
/// overdraw by a frame, at both the group and the domain tier.
const SLACK: u64 = 2 * PACKET as u64;

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
        }
    }

    fn groups(&mut self) -> SendGroups<'_> {
        SendGroups {
            state: &mut self.state,
        }
    }

    fn group(&mut self, priority: i64) -> SendGroup {
        self.groups().open(priority, None)
    }

    fn send(&mut self, id: StreamId) -> SendStream<'_> {
        SendStream {
            id,
            state: &mut self.state,
            pending: &mut self.pending,
            conn_state: &self.conn,
        }
    }

    /// Open a stream in `group` at `priority` and queue `bytes` on it.
    fn open(&mut self, group: Option<&SendGroup>, priority: i64, bytes: usize) -> StreamId {
        let id = Streams {
            state: &mut self.state,
            conn_state: &self.conn,
        }
        .open(Dir::Uni)
        .unwrap();
        let mut stream = self.send(id);
        stream.set_group(group).unwrap();
        stream.set_priority(priority).unwrap();
        self.write(id, bytes);
        id
    }

    fn write(&mut self, id: StreamId, bytes: usize) {
        assert_eq!(self.send(id).write(&vec![0; bytes]), Ok(bytes));
    }

    /// Send one packet, returning its frames.
    fn packet(&mut self) -> Vec<StreamMeta> {
        let mut buf = Vec::with_capacity(PACKET);
        let frames = self.state.write_stream_frames(&mut buf, PACKET, true);

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
        if sizes.iter().map(|(h, p)| h + p).sum::<u64>() != buf.len() as u64 {
            let (header, payload) = sizes.last_mut().unwrap();
            *header -= VarInt::from_u64(*payload).unwrap().size() as u64;
        }
        assert_eq!(
            sizes.iter().map(|(h, p)| h + p).sum::<u64>(),
            buf.len() as u64
        );

        for (frame, (header, payload)) in frames.iter().zip(sizes) {
            *self.sent.entry(frame.id).or_default() += payload;
            *self.wire.entry(frame.id).or_default() += header + payload;
        }
        frames.into_iter().collect()
    }

    /// Send `packets` packets, returning the stream of every frame in order.
    fn drain(&mut self, packets: usize) -> Vec<StreamId> {
        (0..packets)
            .flat_map(|_| self.packet())
            .map(|frame| frame.id)
            .collect()
    }

    fn bytes(&self, streams: &[StreamId]) -> u64 {
        streams
            .iter()
            .map(|id| self.sent.get(id).copied().unwrap_or(0))
            .sum()
    }

    fn wire(&self, streams: &[StreamId]) -> u64 {
        streams
            .iter()
            .map(|id| self.wire.get(id).copied().unwrap_or(0))
            .sum()
    }

    fn clear(&mut self) {
        self.sent.clear();
        self.wire.clear();
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
    let audio: Vec<_> = (0..1_000)
        .map(|seq| sender.open(Some(&audio), seq, 300))
        .collect();
    let video: Vec<_> = (0..4)
        .map(|seq| sender.open(Some(&video), seq, 1_000_000))
        .collect();

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
    let seqs = [
        0,
        1 << 40,
        (1 << 62) - 1,
        7,
        1 << 33,
        (1 << 32) + 1,
        1 << 32,
    ];
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
    sender
        .state
        .received_max_stream_data(open, unlimited)
        .unwrap();
    sender.write(open, 1_000_000);

    sender.drain(50);
    assert_eq!(sender.bytes(&[blocked]), 5_000);
    assert!(sender.bytes(&[open]) > 50_000);

    sender
        .state
        .received_max_stream_data(blocked, unlimited)
        .unwrap();
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

    let lost = sender
        .packet()
        .into_iter()
        .find(|frame| frame.id == lossy)
        .unwrap();
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

/// Domains split bytes evenly however many groups each holds and whatever their
/// priorities, while priority still orders the groups inside one domain.
#[test]
fn domains_share_bytes_and_confine_priority() {
    let mut sender = Sender::new();
    let crowded = sender.groups().open_domain();
    let quiet = sender.groups().open_domain();

    let crowded_groups: Vec<_> = (0..10)
        .map(|priority| sender.groups().open(priority, Some(&crowded)))
        .collect();
    let crowded_streams: Vec<_> = crowded_groups
        .iter()
        .map(|group| sender.open(Some(group), 0, 1_000_000))
        .collect();
    let quiet_group = sender.groups().open(-100, Some(&quiet));
    let quiet_stream = sender.open(Some(&quiet_group), 0, 1_000_000);

    sender.drain(200);
    assert_fair(sender.wire(&crowded_streams), sender.wire(&[quiet_stream]));
    // Only the crowded domain's top group sent anything.
    assert_eq!(
        sender.bytes(&crowded_streams[9..]),
        sender.bytes(&crowded_streams)
    );
}

/// Groups outside any explicit domain share the default domain, which takes one share
/// of the domain round robin.
#[test]
fn the_default_domain_is_one_share() {
    let mut sender = Sender::new();
    let domain = sender.groups().open_domain();
    let inside = sender.groups().open(0, Some(&domain));
    let outside: Vec<SendGroup> = (0..3).map(|_| sender.group(0)).collect();
    let inside = sender.open(Some(&inside), 0, 1_000_000);
    let outside: Vec<_> = outside
        .iter()
        .map(|group| sender.open(Some(group), 0, 1_000_000))
        .collect();

    sender.drain(200);
    assert_fair(sender.wire(&[inside]), sender.wire(&outside));
}

/// A released group lives until its last stream is gone, here by a reset, and then
/// leaves nothing behind in the schedule.
#[test]
fn reset_frees_a_released_group() {
    let mut sender = Sender::new();
    let domain: SendDomain = sender.groups().open_domain();
    let group = sender.groups().open(0, Some(&domain));
    let reset = sender.open(Some(&group), 0, 10_000);
    let finished = sender.open(Some(&group), 1, 100);
    sender.send(finished).finish().unwrap();
    sender.groups().close(group);
    sender.groups().close_domain(domain);

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

    sender.send(reset).reset_to(0u32.into(), 3_000).unwrap();
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
    assert!(
        !sender.state.pending.is_clean(),
        "the reset is unacknowledged"
    );
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
    sender.send(reset).reset_to(0u32.into(), 3_000).unwrap();
    assert!(sender.state.can_send_stream_data());

    assert_eq!(
        sender.state.received_stop_sending(reset, 0u32.into()),
        Some(0)
    );
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
