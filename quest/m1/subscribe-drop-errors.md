# [L] Readers see SUBSCRIBE_DROP

## Goal

A group the publisher drops or cuts ends its reader with an error, never a
clean end or a silent skip. On both Rust and `@moq/net`, a lite SUBSCRIBE_DROP
settles the dropped group's reader as soon as it arrives, before the track's
FIN, and a cut stays visible across cache reclaim, cap changes, and route
recovery. Datagram groups stay best effort.

## Plan

The wire already accounts for every stream group: lite-05 and later name each
sequence a publisher gives up with SUBSCRIBE_DROP, and lite-07 restored it in
place of `Stream Count` (#5983). Today that accounting only settles the
subscription tail; it is not a terminal disposition for live readers or
recovery.

Open, decide first: what `SubscribeDrop.error` means to a reader.
lite-05/06 define it as an application-specific code, with 0 meaning
unavailable without an error, so those values cannot be silently
reinterpreted.

1. (recommended) lite-07 carries the stream-reset error codes, so a DROP ends
   the reader with the same error a reset keeping its header would.
   lite-05/06 keep the application-specific meaning, surfaced as one fixed
   error. Changes lite-07 semantics, so settle it before
   [Finalize moq-lite-07](/quest/m1/lite07-finalize.md).
2. The field stays application-specific on every version, and every DROP ends
   the reader with one fixed error carrying the code.

Decided:

- A resumed group that is the new copy's latest ends with the DROP's error
  when the copy drops it. Audit clarification (2026-10-10): test this while
  the subscription is still open, with no successor and no later writes.
  Both Rust and JS must settle the group wait on DROP, not only mark the
  tail accounted at SUBSCRIBE_END. A pending recovery fetch cannot hide
  that disposition or restart an unbounded route-retry loop.
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

Regression tests: an early DROP settles the dropped group for its reader
before the track's FIN, on both stacks, including a resumed latest group.
Update `drafts/draft-lcurley-moq-lite.md` if option 1 changes what Error Code
means on lite-07, then run `just drafts check` and `just test interop --all`.

## Related

- [Resume reorder](/quest/m1/resume-reorder.md) - late stream headers are not proof that a group was dropped
