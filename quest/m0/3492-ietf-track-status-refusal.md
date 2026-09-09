# [XS] TRACK_STATUS is refused, not dropped

## Goal

A moq-transport peer that sends TRACK_STATUS gets an immediate refusal with the
registered NOT_SUPPORTED code on every negotiated draft, instead of a warning in
our log and a request that hangs until the peer's timeout. TRACK_STATUS itself
stays unimplemented: all it adds over TRACK_INFO is a snapshot of the latest
object, and answering it honestly needs either a new API or a throwaway
SUBSCRIBE. PR #3519 built the latter and was closed for it.

## Plan

`rs/moq-net/src/ietf/publisher.rs` matches `ietf::TrackStatus::ID` with a
warning and an empty future; `session.rs` already routes the stream there, so
nothing is session-fatal. Mirror `run_publish_stream` in `subscriber.rs`, which
answers PUBLISH with NOT_SUPPORTED and closes the writer explicitly (#3348):

- Decode the request so the stream is consumed, reply with the per-draft
  refusal (TRACK_STATUS_ERROR on draft-14, REQUEST_ERROR from draft-15 on) using
  the code type [IETF error codes](https://github.com/moq-dev/moq/pull/3531) introduces,
  and close the writer. On draft-14 and 15 the request rides a virtual stream
  over the control stream whose reset is a no-op, so the explicit reply is the
  only way bytes reach the peer.
- `js/net/src/ietf/publisher.ts` already replies; make it send the same code.
- Tests: a byte-exact transport-log test per version that a TRACK_STATUS request
  yields the refusal and nothing else.

Branch from dev, where the error registry lands.

## Required

- [IETF error codes](https://github.com/moq-dev/moq/pull/3531) - the registered NOT_SUPPORTED value per draft comes from its code type

## Closes

- [#3492](https://github.com/moq-dev/moq/issues/3492) - close this issue when the quest finishes
