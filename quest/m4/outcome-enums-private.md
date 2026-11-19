# [L] Private functions return what their callers use

## Goal

The private and crate-private functions the 2026-11-18 enum-return audit
flagged return the shape their callers use: no `bool`, `Option`, `Poll`, or
`Result` in disguise, no variant every caller ignores, and no report its
callers collapse the same way. No public API changes.

## Plan

From the 2026-11-18 enum-return audit (maintainer, /quest-plan); the public
half is the [outcome-enums epic](/quest/m2/outcome-enums/README.md). m4,
since nothing waits on it and it changes no API. Judge each against
REVIEW.md's "Return what callers use"; keep one whose named variants read
better than the bare type at its one call site, and say so in the PR.

Found (re-check each on the head; paths under `rs/` unless noted):

- moq-net: `ietf/publisher.rs` `Advert` (`Option<AdvertParams>`),
  `WaitResume::poll`'s `Result<(), Option<Served>>` (`Poll<Option<Served>>`),
  `Served::Paused` (not an end; it is why `status()` invents an "internal
  error paused"), and `Watched::lapsed` (always `Until`; return the
  `Instant`); `ietf/subscriber.rs` `Admit` (a struct) and
  `namespaces::Ended` (nobody reads the `Ok` value); `auth.rs`
  `poll_answer`'s `Answer` (only tests read it) and `Reply`
  (`Result<Grant, Refusal>`); `route/node.rs` `forward`'s
  `Option<Option<Via>>` (`Poll<Option<Via>>`).
- moq-uring `quic/web.rs` `VarPoll` (`Poll<Option<u64>>`); moq-relay
  `auth/refresh.rs` `Change` (`Result<Grant, Reason>`, with `Withdrawn` its
  own event).
- moq-mux: `jitter.rs` `Push` (callers only check `Late`; the other cases
  repeat counters); `ts/step.rs` `scan`/`verdict`'s
  `Option<Result<Step, u64>>`, whose `Err` is not an error, and `Reading`
  (`Option<(Way, Duration)>`).
- moq-audio `ring.rs` `read*` -> `Read` (frames read; the other variants
  repeat counters); moq-video capture `pump::Read` (`Frame` and `FrameAt`
  differ only by an optional timestamp).
- moq-hls `export/segments.rs` `Next` (exactly `Poll<Option<Row>>`);
  moq-cli `play/window.rs` `Presented` and `args.rs` `ParseError::kind`
  (only tests read it); moq-transcode `rung.rs` `Ended` and `feed.rs`'s
  `Some(Finished)` / `None` (backwards from `Result<Option<Item>>`).
- moq-ffi and libmoq: `ffi.rs` `Completion` (every site calls `.result()`;
  return the `Poll`), libmoq `consume.rs` `RawStep`.
- js/net: `track.ts` `#feedStart`'s `"pending"` (four of six callers rewrite
  it) and `#askFetch`'s `boolean | "pending"`; `lite/publisher.ts` `take`
  (throw the error); `connection/pool.ts` `offered`, which returns a caught
  `TypeError`; js/watch `media.ts` `nextMedia`'s invented `finish` and its
  `instanceof NetError.Stream` match.

Out of scope: vendored moq-rtmp `rml` and moq-congestion's libwebrtc port.

Public API: none. Wire: none.
