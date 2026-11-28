# [L] The last error-variant matches outside moq-lite stop

## Goal

No call site outside moq-lite's control streams decides behavior by
matching an error variant (AGENTS.md: "Never match on errors"). Each site
either ends its work on any error, or stops needing to tell the case apart
because the callee no longer reports a normal outcome as an error.

## Plan

From the 2026-11-18 enum-return audit (maintainer, /quest-plan). Settled
approach, from [moq-dev/moq#9041](https://github.com/moq-dev/moq/pull/9041):
first ask whether the caller needs to tell the case apart at all, and abort
on any error when it does not; a classifier method is the last resort, as
`moq_net::Error::retry()` is.

Sites, with recommendations (re-grep each on the head):

- moq-mux importers: `ts/import.rs` and `flv/import.rs` continue on
  `Err(MissingKeyframe)`. Drop the deltas before the first keyframe by the
  importer's own keyframe flag before writing, as
  [error-outcomes-rest](/quest/m3/error-outcomes-rest.md) recommends for
  moqsink, so the producer is never asked. The same for `NotPublished` in
  `container/producer.rs`.
- moq-transcode `choose_source` returns `Err(NoSource)` to mean "wait":
  return `Result<Option<_>>`.
- moq-hls `import.rs` matches `ResourceRefused`, `Decode`, and
  `PlaylistEnded` beside the existing `err.settled()`: fold them into it, or
  end the import on any error.
- moq-cli `rtsp.rs` `retries()` matches `moq_rtsp::Error::Unauthorized` and
  `NoSupportedStream`: `moq_rtsp::Error` answers retry the way
  `moq_net::Error::retry()` does.
- moq-auth's revalidation (`client.rs`) and moq-relay's `auth.rs` match
  `Refused`, `UselessGrant`, and `GrantExpired` to choose between ending a
  lease and retrying: have validation return the `lease::Reason` that ends
  it, or nothing to retry.
- moq-net's ietf layer: six `matches!(err, Error::GoingAway)` after
  `control.accept()`, `is_protocol_violation` (12 callers), and
  `Served::status`'s `Unauthorized` match. Report the case where it is
  detected, as `coding::Recv` does; the ietf layer is a low-effort
  compatibility layer, so prefer deleting the distinction where the session
  can just close.
- moq-ffi and libmoq match `moq_net::Error::{Closed, Dropped, Cancel}`
  to mean a clean end. Where a moq-net poll reports a clean
  end as an error, it returns `Ok(None)` instead; that part is breaking in
  moq-net, so split it to m2 if it is more than a mechanical change. (Their
  stale-group `Old` matches went when a group's failure became that group's
  gap.)
- Single-group fetches answer a miss apart from a failure by matching
  `NotFound`, `Old`, and `Evicted`: moq-hls's export (`unservable` in
  `export/rendition.rs`, 404 versus 500) and moq-relay's HTTP fetch
  (`web.rs`). Recommended: have the fetch report "not served" as its own
  outcome where moq-net decides it, so neither caller reads the error.
- Additional `moq_net::Error::{Closed, Unroutable}` matches (added 2026-11-21
  from the pre-cut full-codebase audit): `rs/moq-srt/src/ts.rs:248,668`,
  `rs/moq-boy/src/input.rs:151` (if `moq-boy` is still in tree),
  `rs/moq-gst/src/source/imp.rs:896,2604-2606`, `rs/moq-hls/src/server/mod.rs:247`,
  `rs/moq-cli/src/archive.rs:162`, `rs/moq-cli/src/report.rs:585`, and
  `rs/moq-cli/src/play/media.rs:105`.
- Smaller: moq-archive `Error::NotFound` (`consumer/track.rs`), moq-audio
  `InvalidCapture` (`encode/capture.rs`), moq-video `UnsupportedCodec`
  (`decode/backend/mod.rs`), moq-webrtc `ice::Error::Closed`
  (`session.rs`), and moq-uring's `UnexpectedEnd` matches on
  `web_transport_proto` errors (`quic/web.rs`), whose fix may belong
  upstream in moq-dev/web-transport.

Not here: moq-lite's control streams
([control-stream-recv](/quest/m3/control-stream-recv.md)), the moq-json
stream producer and moqsink ([error-outcomes-rest](/quest/m3/error-outcomes-rest.md)),
moq-quic's stream errors (reshaped in
[moq-dev/moq#9152](https://github.com/moq-dev/moq/pull/9152)), `create_group`'s `Duplicate` (gone since
[moq-dev/moq#9223](https://github.com/moq-dev/moq/pull/9223), which added
`claim_group` for racing writers), and
moq-e2ee ([e2ee-format](/quest/m3/e2ee-format.md)).

Public API: none expected, except a moq-net clean end becoming `Ok(None)`,
which goes to m2 if split. Behavior: sites that now end on any error say so
in the PR. Wire: none.

## Related

- [error-outcomes-rest](/quest/m3/error-outcomes-rest.md) - the two sites the previous run left
- [control-stream-recv](/quest/m3/control-stream-recv.md) - moq-lite's control-stream reads
