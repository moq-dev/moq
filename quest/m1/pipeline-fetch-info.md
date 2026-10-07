# [L] The first FETCH does not wait for the track's info

## Goal

A fetch-only reader's first FETCH goes upstream together with the request for
the track's info, on moq-lite and moq-transport, removing a round trip per hop.
The fetched group is still handed out only once that route's info is known
and passes the origin's consistency check.

## Plan

Decided 2026-10-07, while landing #4974 (fetch-only IETF demand), which keeps
a sequential TRACK_STATUS before the first FETCH. Start after #4974 lands.

Why the round trip exists (facts, 2026-10-07): nothing in a FETCH request
needs the info. Two serial gates do:

- The origin accepts the logical track with the first copy's info (timescale,
  max_age) and refuses later copies whose info differs
  (`model/front.rs` `track_info`), because a relayed group's raw timestamps go
  downstream in the group's own timescale while downstream decodes them with
  the logical track's advertised one. A fetch is routed only to a copy already
  spliced (`resume::Fetching`, `TrackIo::splice`).
- Both sessions register their fetch handler only after their info exchange:
  lite's `TrackServeRun` runs TRACK_INFO first because FETCH frames are
  timestamped in its units; IETF takes the timescale only from SUBSCRIBE_OK or
  TRACK_STATUS_OK.

What needs the info is decoding the response and approving the group, so:

- Origin: let `resume::Fetching` send the copy-level fetch to the copy whose
  info is in flight (a staged generation), resolving only once that copy is
  spliced. On a refused or mismatched info, or a detach, drop the pending
  fetch and re-issue on the next route, as failover already does. Never hand
  out a group before the info check passes.
- lite: register the fetch handler up front and run FETCH alongside
  TRACK_INFO, leaving the response unread in the QUIC stream until the info
  lands (which also covers lite-07 untimed framing). On a failed TRACK_INFO,
  reject the fetch and reset the stream.
- moq-transport: send TRACK_STATUS alongside the FETCH and take timestamps'
  units from it (or FETCH_OK properties where present).

Tests: a fetch-only reader's FETCH is on the wire before the info answer; a
refused info drops the fetch and retries the next route; a group is never
released before its route's info passes. Measure the first-fetch latency
before and after.

Public API: none. Wire: none (ordering only).
