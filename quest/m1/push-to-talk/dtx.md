# [M] Push-to-talk suppresses silence instead of toggling the catalog

## Goal

Muting a push-to-talk publisher keeps its audio rendition enabled and its
subscription live, and sends no media while silent. The next utterance reaches
a listener with no catalog flip and no round trip, and listeners play comfort
noise across the silence without counting it as an underrun. The hold design,
listeners staying subscribed to a disabled rendition, is retired in the same
change. `just test push-to-talk` proves it.

## Plan

Planned 2026-11-04 with the maintainer, from the
[moq-dev/moq#7760](https://github.com/moq-dev/moq/pull/7760) latency
breakdown.

Decided 2026-11-04 (maintainer), reversing the epic's 2026-10-28 "Hold, not
DTX": a catalog toggle makes every listener wait out the `enabled` flip, so
the jitter buffer must hold at least a round trip for push-to-talk to work,
and the maintainer would rather fix DTX and avoid the catalog entirely.

- **Route:** whichever [Pick how the browser publisher suppresses
  silence](/quest/m1/push-to-talk/dtx-route.md) chooses.
- **Hold retires when this lands ✅**, not before, so push-to-talk keeps
  working on main meanwhile. Remove the players' default hold of a disabled
  rendition and restore the hang spec's "must not select" if nothing else
  relies on "should not select"; check video and exports before restoring.
- **Gaps play comfort noise ✅**, not plain silence. A suppressed span is
  intentional silence: it counts as no underrun, does not grow the buffer, and
  the jitter estimator ignores it
  ([Audio jitter target](/quest/m1/audio-jitter-target/README.md) specifies
  the estimator; record the exception there too).
- The publisher's mute becomes "suppress", not `enabled: false`. Keep
  `enabled` for a real stop (leaving the call), where a catalog change is
  correct.
- Mind the release flush's lookahead chunk. The disable path's `flush()`
  sends it at once, up to a frame ahead of its media
  ([moq-dev/moq#7881](https://github.com/moq-dev/moq/pull/7881) measured it adding
  10-22 ms to the median per-press spread). `"auto"` then measures an utterance that starts
  within its 2 s window against it, so suppression should not reintroduce it.

Update the epic's acceptance and `doc/` pages that describe hold.

## Required

- [Pick how the browser publisher suppresses silence](/quest/m1/push-to-talk/dtx-route.md) - the route this implements

