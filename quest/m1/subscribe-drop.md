# [L] SUBSCRIBE_DROP accounts for every group

## Goal

A lite subscriber can tell "not yet" from "never" for every stream group in
its subscription. Each sequence in range either arrives on a Group Stream, is
sent as a datagram, or is named by a SUBSCRIBE_DROP, including sequences the
publisher skipped. Only a lost datagram stays unaccounted. Rust and
`@moq/net` publishers send it on every lite version that has it, and moq-lite-07
brings it back in place of `Stream Count`.

## Plan

Today SUBSCRIBE_DROP is on the wire for lite-03 through lite-06 and the Rust
subscriber accounts for it, but no publisher sends it. lite-07 (still
`moq-lite-07-wip`, unpublished) removed it for a `Stream Count` on
SUBSCRIBE_END (#4224).

Decided:

- lite-07 restores SUBSCRIBE_DROP and removes `Stream Count`. With every
  sequence accounted for, the count is redundant; one mechanism instead of two.
  A reset whose header may be lost is covered by a DROP.
- A reliable reset ([Reliable stream reset](/quest/m1/quic/reliable-reset.md))
  that keeps the stream header acts as a one-group drop, an optimization over
  sending the DROP.
- Publishers send SUBSCRIBE_DROP on lite-03 through lite-06 too: for every group
  in range they won't deliver (expired, deprioritized, or reset without its
  header delivered) and for every explicit gap. Publishers that skip sequences
  (`cut` and group discontinuities in the media layers) must mark the gap so
  the net layer can drop it.
- Datagram groups stay best effort. A publisher counts a datagram as
  delivered, so a lost one leaves an uncovered hole that waits out the tail
  grace, as today.
- A resumed group that is the new copy's latest ends with the DROP's error
  when the copy drops it.
- A dropped or aborted group is visible to readers, not silently skipped.
  #4533 found the Rust model releases an aborted group's sequence and skips it,
  so a truncated first object is indistinguishable from a group never sent.

Learned from the shelved [#4998](https://github.com/moq-dev/moq/pull/4998)
(maintainer, 2026-10-07; its approach is on `wip/4998-cut-siblings`):

- A group a stream cancel or session close cut below a finished end must be
  named (a SUBSCRIBE_DROP) and surfaced to readers, never a clean end.
- Carry the cut per group. A publisher that resets the whole subscription for
  one cut group makes the downstream relay resubscribe in a loop.
- The front's resume layer (`model/resume.rs`) treats any error from a copy as
  a dead route and waits for a replacement, so a reader error for a cut hangs a
  relay. Surface a cut on a complete copy to the reader instead.
- Cut tracking must survive the cache reclaiming the aborted slot (expiry,
  eviction, and the pool's drain sweep), and the ordered reader's
  sealed/closed check must not return a clean end before looking for a cut.
- Fetched backfill that is abandoned, and a `DeliveryTimeout` reset, are
  deliberate holes, not cuts, like old, evicted, and lagged groups.
- A cut above a reader's group cap is skipped for good, so a reader whose cap
  `set_groups` later raises over it must still see the cut, not a clean end.
- moq-tokio's
  `subscription_end_integrity::a_subscription_cut_by_the_publisher_disconnecting_does_not_end_clean`
  flakes under load (`Ok(None)` with 10 of 20 frames, #4332): the code can
  still end it clean until this lands, and it should pass reliably after.

Update `drafts/draft-lcurley-moq-lite.md` (SUBSCRIBE_DROP, SUBSCRIBE_END, the
lite-07 changelog), `doc/concept/moq-lite.md`, and the Rust and JS lite
publishers, subscribers, and tail accounting. Run `just drafts check` and
`just test interop --all`.

Regression tests: a publisher that expires a group, skips a sequence, and
resets a stream before its header; on each version the subscriber settles
without waiting out the grace.

Add the lite-07 drop case to the tail interop harness from
[track tail interop](/quest/m1/track-tail-interop.md): Rust and JS
subscribers both settle on SUBSCRIBE_DROP through the relay, so a group the
publisher skipped or never opened ends the track without waiting out the
grace. Decided in the 2026-09-30 audit: the case moved here so the basic
tail interop could land first.


## Related

- [Track tail interop](/quest/m1/track-tail-interop.md) - the Rust-JS tail harness the lite-07 drop case extends
