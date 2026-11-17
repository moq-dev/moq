# [S] moq-net's last pub items with a single container-reader caller go private

## Goal

`track::Subscriber::next_in_range`, `is_skipped`, and `poll_start` are
crate-private, and so is `Subscriber::with_range` if its last production
caller can leave. Their one consumer is moq-mux's container reader, which
orders groups itself; `track::Ordered` reports a `Gap`, so the reader can
move onto it.

## Plan

What is left of the moq-net sweep, which removed or hid every other item
without a caller.

- `next_in_range`, `is_skipped`, and `poll_start` go crate-private in the
  same PR that moves the container reader onto `Ordered`'s `Gap`.
- `Subscriber::with_range` stays `pub` for now: its last production caller is
  `rs/moq-mux/src/container/source.rs`'s resubscription, which resumes from
  the group after the one it already read. A time start names the group
  containing it, so a `Subscription` range cannot leave that group out. If
  the reader can skip it through `Gap` or its own cursor, make `with_range`
  crate-private and move the callers in moq-mux and moq-json tests to
  `Subscription::ranges`; otherwise it stays, and this quest records why.
- `origin::Consumer::covering` stays `pub`: moq.pro and moq-net's own
  `benches/origin.rs` call it.

Public API: breaking, removals only. Wire: none.

## Related

- [Browser audio holes](/quest/m2/audio-playout/browser-audio-hole-skip.md) - the browser counterpart of the reader moving onto `Gap`
