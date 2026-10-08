# [L] The first FETCH does not wait for the track's info

## Goal

A fetch-only reader's first FETCH goes upstream together with the request for
the track's info, on moq-lite and moq-transport, at every hop, removing a
round trip per hop. The fetched group is still handed out only once that
route's info is known and passes the origin's consistency check. Peers that
send them serially keep working unchanged.

## Plan

Decided 2026-10-07, while landing #4974 (fetch-only IETF demand), which keeps
a sequential TRACK_STATUS before the first FETCH. Moved from m2 into
[Pipelined requests](/quest/m1/pipeline-requests/README.md) on 2026-10-08:
the maintainer called the extra round trip not worth keeping, for FETCH as
for SUBSCRIBE, without waiting on a latency measurement.

Why the round trip exists (facts, 2026-10-07): nothing in a FETCH request
needs the info. Two serial gates do:

- The origin accepts the logical track with the first copy's info (timescale,
  max_age, priority) and refuses later copies whose info differs
  (`model/front.rs` `track_info`), because a relayed group's raw timestamps go
  downstream in the group's own timescale while downstream decodes them with
  the logical track's advertised one. A fetch is routed only to a copy already
  spliced (`resume::Fetching`, `TrackIo::splice`).
- Both sessions register their fetch handler only after their info exchange:
  lite's `TrackServeRun` runs TRACK_INFO first because FETCH frames are
  timestamped in its units (only on wires with a track stream; older lite
  serves with default info and has no gate to remove); IETF takes the
  timescale only from SUBSCRIBE_OK or TRACK_STATUS_OK.

What needs the info is decoding the response and approving the group, so:

- Origin: let `resume::Fetching` send the copy-level fetch to the copy whose
  info is in flight (a staged generation), resolving only once that copy is
  spliced. On a refused or mismatched info, or a detach, drop the pending
  fetch and re-issue on the next route, as failover already does. Never hand
  out a group before the info check passes. This gate is FETCH's alone:
  subscription demand already reaches a copy before it is spliced.
- lite: register the fetch handler up front and run FETCH alongside
  TRACK_INFO, leaving the response unread in the QUIC stream until the info
  lands (which also covers lite-07 untimed framing). On a failed TRACK_INFO,
  reject the fetch and reset the stream.
- moq-transport: send TRACK_STATUS alongside the FETCH and take timestamps'
  units from it (or FETCH_OK properties where present). Draft-17's
  TRACK_STATUS answer carries no properties, and its FETCH_OK timescale is
  decoded but not surfaced (`ietf/fetch.rs`), so draft-17 keeps #4974's
  SUBSCRIBE fallback and is not pipelined.

The line's shared decisions apply: early response data stays unread in QUIC,
a failed info fails the fetch, and legacy serial peers keep working.

Tests: a fetch-only reader's FETCH is on the wire before the info answer; a
refused info drops the fetch and retries the next route; a group is never
released before its route's info passes; a serial peer against a pipelining
one and the reverse. Measure the first-fetch latency before and after.

Public API: none. Wire: none (ordering only).

## Related

- [Pipelined SUBSCRIBE](/quest/m1/pipeline-requests/subscribe.md) - the same change for subscriptions
- [FETCH_OK properties](/quest/m1/fetch-ok-properties.md) - fills the FETCH_OK properties this reads where present
- [moq-transport ranges](/quest/m1/subscribe-ranges/ietf.md) - adjacent code in `ietf/subscriber.rs` and `model/origin.rs`; this lands first and ranges rebases
