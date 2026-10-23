# [XS] The lite draft agrees that a forbidden value closes the session

## Goal

`drafts/draft-lcurley-moq-lite.md` stops contradicting its own Message Length
rule: every forbidden value in a Setup or bidirectional stream message closes
the session with PROTOCOL_VIOLATION, which is what `rs/moq-net` and `js/net`
already do on every lite version. Wire: no change.

## Plan

Found by an external conformance runner and verified against the code
2026-10-22. [#5550](https://github.com/moq-dev/moq/pull/5550) made any
control decode error close the session, on every lite version, but left two
sentences that still say "reset the stream":

- Line 222, Frame bounds: a `Frame Start` without the group it qualifies, or a
  `Frame End` without a `Group End`. `decode_frame_bounds` in
  `rs/moq-net/src/lite/subscribe.rs` (SUBSCRIBE, SUBSCRIBE_UPDATE) and the
  range check in `lite/fetch.rs` return `InvalidSubscribeLocation`, which
  `RequestServe` treats as a control violation.
- Line 1366, Timescale: a lite-05/06 TRACK_INFO with Timescale 0.
  `lite/track.rs` decodes it as `InvalidValue`. Confirm the subscriber closes
  the session in Rust and JS; if either only resets, fix it in the same PR.

Reword both to close the session, and grep the draft for any other forbidden
value whose stated response is a stream reset. Leave the legitimate resets
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
