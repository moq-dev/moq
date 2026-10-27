# [L] Ring storage for sent and lost packets

## Goal

`moq-quic` tracks sent and lost packets in ring storage instead of a
`BTreeMap`, with O(1) insert, front removal, and typical lookup. Memory stays
bounded by design against a peer that never ACKs, with no timer or cap whose
only job is refusal. A `SentPackets` micro-bench and `just bench` show the
win. A measured no-win against `BTreeMap` abandons the quest.

Loss detection, RTT sampling, and congestion semantics stay as RFC 9002 says,
except where ACK-only retention changes them. The Plan accepts two deviations
from evicting ACK-only records. Persistent congestion can be under-declared
after 256 unACKed ACK-only packets. An RTT sample is skipped when the peer's
largest ACK names an evicted record.

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
  - **Split rings.** One ring holds the ack-eliciting packets. It is indexed
    by entry, not packet number: guess `pn - front_pn`, then binary-search.
    While the peer is silent, the congestion window bounds it, plus PTO
    probes. Probes bypass cwnd but back off up to `2^MAX_BACKOFF_EXPONENT`,
    and each costs one entry, as in today's `BTreeMap`. Once the peer ACKs,
    anything more than `packet_threshold` below its largest ACK is lost, so
    tombstones stay near the front. A second ring holds every ACK-only
    packet, capped at 256 and forgetting its oldest, which RFC 9000
    allows since those packets carry nothing to retransmit. It keeps full
    records, because `pad_to_mtu` pads ACK-only packets without a cwnd gate
    and `finish_and_track` gives them `congestion` state. Evicting one
    debits in-flight bytes and congestion state by `path_generation`, as
    today's forgotten tail does. This drops the
    `unacked_non_ack_eliciting_tail` bookkeeping.
  - **Chunked slots.** Port #2942 with a `VecDeque` of chunks, so front
    removal is O(1). It is the reference point that keeps today's tail-only
    eviction.
- Every packet outside the congestion window is capped or backoff-limited.
  ACK-only packets, padded or not, are capped. PTO probes stay tracked:
  they can carry new STREAM data that must be requeued if lost, and an ACK
  that newly acknowledges only an untracked probe would return early from
  `on_ack_received` without resetting the PTO backoff.
- The ACK-only cap drops from 1000 to 256 in both designs. A dropped record
  costs only the RTT sample when the peer's largest ACK names it, since our
  ack-eliciting packets carry ACK frames that already prune `pending_acks`.
  The peer names a packet sent one RTT plus its ack delay before the ACK
  arrives. The cap therefore needs our ACK rate times (RTT +
  `max_ack_delay`). At one ACK per two 1200-byte packets and 25 ms delay,
  256 covers about 40 Mbps of receive at 100 ms RTT, typical moq viewing.
  1000 covered about 150 Mbps. The worst case drops from about 128 KB to
  32 KB per connection.
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
  - **Split by position.** One packet-number delimiter would need no merged
    walks, with a tail ring for packets sent after cwnd fills. But an
    app-limited sender, the usual moq subscriber, never fills cwnd, so
    ACK-only packets would pile up uncapped behind one unACKed packet.
  - **Superseding or untracking PTO probes.** Probes add a few entries, so
    neither buys speed, and both change loss recovery.
- Ranked ahead of QUIC lock contention: it is self-contained, and upstream
  already measured it.

Guidance:

- Split rings must keep merged packet-number order wherever adjacency
  matters. `detect_lost_packets` treats a gap as "an intervening packet was
  acknowledged" for persistent congestion, so it walks both rings merged.
  ACK processing and `get` (the largest-acked send time) also span both
  rings. Bandwidth-probe sources are ack-eliciting only.
- Split rings evict the oldest ACK-only packet anywhere, not only in the
  tail. The gap that leaves between two lost ack-eliciting packets reads as
  "an intervening packet was acknowledged" and resets persistent congestion.
  Decided 2026-10-27: accept and document it. Skipped packet numbers and
  today's forgotten tail make the same gaps already, and the error only
  under-declares persistent congestion after 256 unACKed ACK-only packets.
- Correctness tests cover padded ACK-only packets, both ACKed and lost,
  including after a path migration, so congestion feedback and in-flight
  bytes stay exact. The bound test includes a padded, never-ACKed receiver
  and repeated PTOs.
- Add `rs/moq-quic/benches/sent_packets.rs` with these scenarios:
  - #2942's three: typical connection, unacked non-ack-eliciting tail, and
    PTO probe holes.
  - An ACK-only-heavy receiver, the usual moq subscriber.
  - A high-BDP bulk sender.

## Related

- [QUIC lock contention](/quest/m2/perf/quic-lock-contention.md) - the next moq-quic hot-path cost in this line
