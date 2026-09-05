# [M] IETF error codes come from the draft registry

## Goal

Every error code moq-net and js/net put on a moq-transport wire is a
registered value for the negotiated draft, in both directions: the
SUBSCRIBE_ERROR, FETCH_ERROR, and REQUEST_ERROR payloads, and the RESET_STREAM
and STOP_SENDING codes on data and request streams. A relay built on these
crates passes the interop runner's subscribe-error and subscribe-before-announce
cases without the peer's compatibility branch, and a routine cancellation is
read as CANCELLED rather than INTERNAL_ERROR.

Boundaries: moq-lite's own code spaces are untouched, and js/net's inability to
carry a code on a locally raised stream error stays with
[JS stream codes](/quest/m1/js-net-stream-error-codes.md), which is about the
moq-lite space and the abstraction, not the registry.

## Plan

What the tree does today, identically on main and dev:

- `rs/moq-net/src/ietf/publisher.rs` `run_subscribe` rejects with the literal
  `404` at three sites and `run_fetch_stream` with `500`;
  `rs/moq-net/src/ietf/subscriber.rs` `write_error` uses `400`.
  `reject_subscribe`, `reject_fetch`, and `write_error` take a bare `u64`, and
  `ietf::SubscribeError`, `ietf::FetchError`, and `ietf::RequestError` carry a
  bare `error_code`. The only named IETF code type is `TrackStatusCode`
  (`rs/moq-net/src/ietf/track.rs`), for a different registry, plus a
  function-local `NOT_SUPPORTED: u64 = 0x3` in the subscriber.
- `js/net/src/ietf/publisher.ts` `runSubscribe` writes `errorCode: 404` on both
  the draft-14 `SubscribeError` and the draft-15+ `RequestError` branch;
  `js/net/src/ietf/subscriber.ts` uses `400` and `409`; `errorCode` is a plain
  number everywhere.
- `js/net/src/ietf/subscriber.ts` `runPublish` writes a draft-14 `PublishError`
  with the function-local `NOT_SUPPORTED` and a `RequestError` on later
  drafts; the Rust subscriber's publish handling is the same shape.
- Stream resets on an IETF session send `Error::to_code()`, the moq-lite space
  where `Cancel` is `0`; draft-19 section 3.3.4 assigns `0x0` to INTERNAL_ERROR
  and `0x1` to CANCELLED. `Error::from_transport` maps only `0` back to
  `Cancel`, so both directions have to move together. #2993 fixed one site
  (`cancel_subscribe`, a named `STREAM_CANCELLED`).
- `Version::Draft14` through `Draft20` are all negotiated, straddling the
  draft-19 consolidation into REQUEST_ERROR, so the values are per version.

The work:

- One named code type per registry (request errors, stream errors), with
  `Encode<Version>` and `Decode<Version>` in the `TrackStatusCode` style, that
  maps `Error` variants to the registered value for the negotiated draft and
  back. An incoming code with no named value decodes to the remote/opaque
  variant, never to a named one. Cite the draft section each value comes from
  in the type's docs.
- Use it at every construction site above, in Rust and JS, and in
  `Reader::abort` / `Writer::abort` on IETF sessions. Delete the literals and
  the function-local constant. `Error::NotFound`'s IETF wire value comes from
  the same mapping.
- Tests: encode/decode round-trips per version; a draft-14 PublishError and a
  draft-15+ RequestError round-trip for the publish path; a subscribe for a missing
  broadcast rejects with the not-found value of each draft; a cancelled
  subscription resets with CANCELLED and a moq-net peer reads it back as
  `Cancel`; the two interop runner cases pass without their COMPAT branch.

The reporter of #3359 offered a PR and asked whether to prefer a version-aware
enum over a flat one: version-aware, per the above.

## Closes

- [#3359](https://github.com/moq-dev/moq/issues/3359) - close this issue when the quest finishes
- [#3001](https://github.com/moq-dev/moq/issues/3001) - close this issue when the quest finishes

## Related

- [JS stream codes](/quest/m1/js-net-stream-error-codes.md) - the moq-lite half for js/net
- [#3187](/quest/m1/3187-preserve-structured-protocol-error-codes-across-ffi-and-c.md) - the same codes crossing the FFI
