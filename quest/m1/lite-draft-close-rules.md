# [XS] The lite draft agrees that a forbidden value closes the session

## Goal

`drafts/draft-lcurley-moq-lite.md` stops contradicting its own Message Length
rule: every forbidden value in a Setup or bidirectional stream message closes
the session with PROTOCOL_VIOLATION, which is what `rs/moq-net` and `js/net`
already do on every lite version except for FETCH response frames. Wire: no change.

## Plan

Found by an external conformance runner and verified against the code
2026-10-22. [#5550](https://github.com/moq-dev/moq/pull/5550) made any
control decode error close the session, on every lite version, but left four
sentences that still respond to a forbidden value on the stream alone:

- Line 222, Frame bounds: a `Frame Start` without the group it qualifies, or a
  `Frame End` without a `Group End`. `decode_frame_bounds` in
  `rs/moq-net/src/lite/subscribe.rs` (SUBSCRIBE, SUBSCRIBE_UPDATE) and the
  range check in `lite/fetch.rs` return `InvalidSubscribeLocation`, which
  `RequestServe` treats as a control violation.
- Line 1366, Timescale: a lite-05/06 TRACK_INFO with Timescale 0.
  `lite/track.rs` decodes it as `InvalidValue`. Confirm the subscriber closes
  the session in Rust and JS; if either only resets, fix it in the same PR.
- Line 1096, Hop Count: a Hop Count that disagrees with the Hop ID entries
  that follow says "close the stream", but ANNOUNCE rides a bidirectional
  stream and both implementations already treat the decode failure as
  session-fatal.
- Line 565, FETCH response frames: a FRAME that fails to decode against the
  cached TRACK_INFO resets the stream and re-requests it, though a FETCH
  response is bidirectional. Here the code agrees with the text, not the
  rule: JS `#runFetchResponse` aborts the stream and Rust `FetchServeRun`
  aborts only the fetched group. Decide at the start whether this case is
  exempt or closes the session too. Recommended: exempt it and say so beside
  the Message Length rule, because the reset pairs with a re-request after a
  stale cache, which is recovery rather than a forbidden value; close only if
  the cache scoping makes that staleness impossible.

Reword all four to close the session, and grep the draft for any other forbidden
value whose stated response is a stream reset or close. Leave the legitimate resets
alone: an unknown stream type, a publisher that cannot serve a range, and a
publisher without Probe. Extend the lite-07 changelog entry for the Message
Length rule to name frame bounds.

Decided 2026-10-22: close the session on every version, lite-06 included,
rather than reset the stream or gate on the negotiated version. Only a broken
peer sends these values, and no released subscriber of ours does, so the
change reaches no conforming peer. A version branch would exist only to treat
a broken peer more gently, and a reset lets the peer repeat the violation on
the next stream. lite-06 thereby deviates from its published text toward
broken peers; the runner's `L06-3-6-MUST-023` row stays failing as an
accepted deviation.

Validate with `just drafts check`.

## Closes

- [#6405](https://github.com/moq-dev/moq/issues/6405) - lite-06 SUBSCRIBE with Frame End but no Group End closes the session
