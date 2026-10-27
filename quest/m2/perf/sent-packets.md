# [L] Ring storage for sent and lost packets

## Goal

`moq-quic` tracks sent and lost packets in ring storage instead of a
`BTreeMap`, with O(1) insert, front removal, and typical lookup. Memory stays
bounded by design against a peer that never ACKs, with no timer or cap whose
only job is refusal. A `SentPackets` micro-bench and `just bench` show the
win. A measured no-win against `BTreeMap` abandons the quest.

Loss detection, RTT sampling, and congestion semantics stay as RFC 9002 says.
How ACK-only packets are retained may change.

## Plan

Today `SentPackets` (`rs/moq-quic/src/connection/sent_packets.rs`) and
`PacketSpace::lost_packets` are `BTreeMap`s. That is quinn's fallback from
[quinn-rs/quinn#2858](https://github.com/quinn-rs/quinn/pull/2858), which
reverted the ring buffer from
[quinn-rs/quinn#2730](https://github.com/quinn-rs/quinn/pull/2730). In that
ring, a peer that never ACKs pins the front entry. `PacketSpace::sent` then
forgets the oldest ACK-only packet after 1000, and the forgotten slots stay
inside the span between front and back, so the span grows at the peer's send
rate. [quinn-rs/quinn#2942](https://github.com/quinn-rs/quinn/pull/2942)
(open) answers this with 64-slot chunks and reports about 2x on
micro-benches.

Decided 2026-10-27:

- Spike two designs on one branch, bench both against the `BTreeMap`
  baseline, keep only the winner, and record the table in the PR.
  - **Split rings.** One ring holds full `SentPacket`s for every packet
    counted in flight (`size != 0`). That covers every ack-eliciting packet
    and every padded ACK-only packet, which `finish_and_track` gives
    `congestion` state and `PathData::remove_in_flight` debits by
    `path_generation`. This ring is indexed by entry, not packet number:
    guess `pn - front_pn`, then binary-search. The congestion window plus
    PTO probes bounds it while the peer is silent. Once the peer ACKs,
    anything more than 3 below its largest ACK is lost, so tombstones stay
    near the front. A second ring holds compact records (packet number, send
    time, largest acked) for zero-size ACK-only packets, which carry no
    congestion or path state. It is capped at today's 1000 and forgets its
    oldest, which RFC 9000 allows since those packets carry nothing to
    retransmit. This drops the `unacked_non_ack_eliciting_tail` bookkeeping.
  - **Chunked slots.** Port #2942 with a `VecDeque` of chunks, so front
    removal is O(1). It is the no-behavior-change reference point.
- Pick the winner this way. First, it must keep storage bounded by live
  packets under a never-ACKing peer (a unit test). Among the designs that
  pass, the fastest on the micro-bench wins. Within noise, the one with the
  least code wins.
- `lost_packets` uses the winner's storage in the same quest. Its packet
  numbers and send times rise together. `drain_lost_packets` drops entries
  from the front by age, and spurious-loss recovery removes from the middle.
- Rejected:
  - **Age-based loss.** Declaring packets lost after a period with no ACK
    still lets memory grow with the peer's packet rate times the period. It
    also retransmits and cuts cwnd on packets that may have arrived.
  - **Tail ring migrated into a pn-indexed ring.** Migrating on each
    ack-eliciting send carries the forgotten gap into the main ring, so a
    silent peer still grows it between PTO probes.
  - **Ring plus side map.** Not chosen for the spike.
- Ranked ahead of QUIC lock contention: it is self-contained, and upstream
  already measured it.

Guidance:

- Split rings must keep merged packet-number order wherever adjacency
  matters. `detect_lost_packets` treats a gap as "an intervening packet was
  acknowledged" for persistent congestion, so it walks both rings merged.
  ACK processing and `get` (the largest-acked send time) also span both
  rings. Bandwidth-probe sources are ack-eliciting only.
- Correctness tests cover padded ACK-only packets, both ACKed and lost,
  including after a path migration, so congestion feedback and in-flight
  bytes stay exact.
- Add `rs/moq-quic/benches/sent_packets.rs` with these scenarios:
  - #2942's three: typical connection, unacked non-ack-eliciting tail, and
    PTO probe holes.
  - An ACK-only-heavy receiver, the usual moq subscriber.
  - A high-BDP bulk sender.

## Related

- [QUIC lock contention](/quest/m2/perf/quic-lock-contention.md) - the next moq-quic hot-path cost in this line
