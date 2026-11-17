# [M] Five call sites stop matching on errors

## Goal

None of these decide behavior by matching an error variant (AGENTS.md:
"Never match on errors"); each callee reports the case as a typed outcome
instead:

- `rs/moq-stats/src/produce.rs`: whether an oversized frame aborts a track.
- `rs/moq-hls/src/import.rs`: refusing a rewind (`TimestampRewind`,
  `NonMonotonicDecodeTime`) instead of retrying; give the fMP4 importer a
  typed rewind outcome, as [moq-dev/moq#8939](https://github.com/moq-dev/moq/pull/8939)
  did for TS import.
- `rs/qmux/src/driver.rs` `deliver`: a WebSocket message over the record
  limit fails in tungstenite with `Capacity` and lands in the catch-all, so
  the session resets with no CONNECTION_CLOSE. The WebSocket transport
  reports "over the record limit" as an outcome and the driver sends
  FRAME_ENCODING_ERROR. Also make `js/qmux` close violations with QUIC
  transport codes, as rs/qmux does, keeping 1002/1003 for the WebSocket
  close only.
- `rs/moq-uring/src/quic/driver/connection.rs` and
  `rs/moq-tokio/src/quic/driver/connection.rs`: both skip publishing a close
  reason when it is `Error::LocallyClosed`, then guess whether the close is
  staged; moq-quic says directly whether a close is still owed.

## Plan

Found by the 2026-11-17 pre-cut correctness audit, from source. None breaks
today; each turns a future error change into a silent behavior change (a
retry loop, a lost CONNECTION_CLOSE). Each is internal; check the HLS and
qmux ones with a test that drives the case end to end.

Public API: none. Wire: none.

## Related

- [Session codes](https://github.com/moq-dev/moq/commit/b4c1db719358a25e351771e8ad33f01d45169bb0) - errors classify themselves in moq-net
