# [S] Announce counters

## Goal

A relay's `/metrics` exposes, per (tier, role), announce starts, ends,
in-place updates, and the encoded bytes of announce-stream messages. An
operator can then chart a cluster's announce traffic without reading the
`.stats` broadcasts. `announced_bytes` keeps its meaning (broadcast-name
length), since downstream billing reads it.

## Plan

`rs/moq-net/src/stats.rs` already counts `announces_started`,
`announces_ended`, and `announced_bytes` per tier; `render_metrics` in
`rs/moq-relay/src/internal.rs` skips them. Render them, and add two counters:

- `announces_updated`: routes re-sent in place (lite-06 `ANNOUNCE_RESTART`,
  lite-05's duplicate `ANNOUNCE`). `hand_out` counts only real starts today.
- `announce_wire_bytes`: the encoded size of every message on announce
  streams (requests, starts, ends, restarts), counted where the lite publisher
  and subscriber encode and decode them, plus the IETF namespace messages
  where `rs/moq-net/src/ietf` does.

Test the Prometheus rendering. Also test, through two origins joined by a
session, that one start, a re-price, and an end move each counter once and
that the wire bytes match the encoded messages.

## Related

- [Cluster simulator](/quest/m1/announce-tree/simulator.md) - reports its
  per-event counts with these
