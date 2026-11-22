# [S] Enforce lite-07 ProtocolViolation on malformed subscribe and unknown announce messages

## Goal

A lite-07 subscriber immediately closes the session with
`SessionError::ProtocolViolation` when `Recv::Malformed` arrives on a Subscribe
Stream (never hiding it behind `ServeMode::Failed`) and rejects unknown Announce
Stream message types on Lite07+ rather than skipping them.

## Plan

Decided 2026-11-21 (maintainer, /quest-plan of the exhaustive pre-cut
full-codebase audit): ✅ place high-severity and impactful protocol correctness
bugs in `m1` (rejected: `m0` with `release` backports; `m3`).

1. **Close immediately on `Recv::Malformed` instead of entering `ServeMode::Failed`
   (`rs/moq-net/src/lite/subscriber.rs:13278-13309,13546-13553`).**
   - When a malformed response arrives on a Subscribe Stream, entering
     `ServeMode::Failed` waits on active Group Streams; if the local subscriber
     unsubscribes first, the `ProtocolViolation` session close is silently
     dropped.
   - Close the session immediately with `SessionError::ProtocolViolation` on
     `Recv::Malformed`.
2. **Restrict `Self::Skipped` on Announce Stream to Lite06 (`!version.has_routes()`)
   (`rs/moq-net/src/lite/announce.rs:666-671`,
   `rs/moq-net/src/lite/subscriber/routed.rs:599,743`).**
   - On Lite07+, unknown Announce Stream message types are protocol violations,
     not skippable extensions. Gate the `Self::Skipped` fallback on
     `!version.has_routes()` and close with `ProtocolViolation` on Lite07+.

Note: `DatagramServe`'s missing `Recv::Malformed` session close is tracked in
[control-stream-recv](/quest/m3/control-stream-recv.md).

Public API: none. Wire: enforces existing lite-07 spec rules. Add unit tests
for both cases in `rs/moq-net` (including unsubscribing immediately after
receiving a malformed Subscribe Stream message).
