# [M] Report bytes delayed by loss at relay ingest

## Goal

The relay's `moq-stats` ingress rows carry, per broadcast, a cumulative
`loss_delay_bytes` counter: stream bytes that arrived behind a hole and stayed
unreadable for at least one smoothed RTT. Compared with bytes received, it
shows how much of a publisher's media a lossy uplink held back, and it is the
production number that sizes the cut-through opportunity.

## Plan

Decided 2026-09-30: one monotonic byte counter, no hold-time histogram, on the
ingress (`Role::Subscriber`) rows next to
[publisher timeliness](/quest/m1/qos/publisher-timeliness.md)'s fields.
Independent of the bench; it can start once `moq-quic` lands.

- `moq-quic` counts per receive stream. The assembler stamps each chunk buffered past
  the contiguous frontier with its arrival time; when the frontier passes it,
  bytes held at least one smoothed RTT are counted. Plain reordering fills
  within an RTT and is not counted. The count is by arrival and gap fill, not by
  when the application reads, so it stays meaningful once reads go unordered.
  When a stream ends with a hole still open (reset, stop, or session close),
  buffered bytes that have already waited one smoothed RTT are counted then.
- `moq-quic` exposes the per-stream count, and moq-net's
  `transport::poll::RecvStream` gains an accessor returning `Option` (`None`
  when the backend cannot see it), following the module's `Stats`
  convention. The moq-tokio and moq-uring adapters implement it, as
  [poll_acked](/quest/m2/quic-ack-hook.md) does.
- The lite and IETF subscribers read the count at each frame boundary and when
  the group stream ends, adding the delta to the broadcast's ingress row, so a
  long group does not leave the row stale. Nothing on the per-byte path.
- Update the stats section of `doc/bin/relay/config.md`.

Test: a seeded loss on a `moq-quic` pair counts the held bytes; a reorder shorter than
an RTT counts nothing.

Public API: an additive trait accessor. Wire: a new `moq-stats` field,
additive on `main` since `Traffic` is `#[non_exhaustive]` and readers default
missing fields.

## Required

- [Hard fork](/quest/m1/quic/fork/README.md) - the `moq-quic` assembler this counts in

## Related

- [QoS](/quest/m1/qos/README.md) - the per-broadcast ingress conventions this follows
