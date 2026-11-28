# [L] A bonded connection duplicates chosen streams on every path

## Goal

Redundancy is a per-stream property with a connection default. A stream
marked redundant has its data sent on every available path, so it loses
nothing when one path dies; an unmarked stream is striped as today. A
connection whose default is redundant duplicates everything, and a striped
connection can mark single streams, such as a control stream beside striped
video. The receiver delivers each byte once.

## Plan

Decided 2026-11-26 (`/quest-plan` via kixel-ai/kixel.ai): ✅ one quest, with
redundancy per stream and a connection default (rejected: a connection-wide
switch first and per-stream later, whose switch the second quest rebuilds;
connection-wide only, which cannot duplicate a control stream beside striped
video).

Recommended, open to review:

- In moq-quic, a flag on the send stream and a default in the transport
  config, read by the scheduler seam
  [striping](/quest/m3/quic-multipath/stripe.md) leaves. QUIC stream offsets
  already drop a duplicate at the receiver, so nothing changes on the wire.
- In moq-net, a session option marks streams at or above a priority as
  redundant, so no track API and no moq-lite field is needed. Settle the
  public shape with its consumer before building it.
- Datagrams are not duplicated: a datagram has no offset, so its copy would
  reach the application twice.
- Backup paths stay idle while an available path lives, for redundant
  streams too, since a backup is usually metered.
- A redundant stream spends each path's congestion window, so the stats
  report duplicated bytes per path.

Measure on the seeded sim and the namespace harness: a marked stream's gap
across a silent cut is zero where a striped one waits out loss detection,
unmarked streams keep their striped throughput, and the duplicated bytes.

Public API: additive in moq-quic, moq-tokio, and moq-net. Wire: none.

## Required

- [A bonded connection sends on every available path at once](/quest/m3/quic-multipath/stripe.md) - the scheduler seam and the measurements to compare against

## Related

- [QUIC multipath](/quest/m3/quic-multipath/README.md) - the decisions this builds on
