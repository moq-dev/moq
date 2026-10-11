# [L] Publishers drop a group the moment they give it up

## Goal

On lite-05 and lite-06, the Rust and `@moq/net` publishers send a
SUBSCRIBE_DROP for a sequence as soon as they give it up, and still name every
sequence the subscription never got just before the FIN. A subscriber to
either publisher, directly or through a relay, settles without waiting out its
grace. Both implementations behave identically.

Non-goals, kept in [SUBSCRIBE_DROP](/quest/m1/subscribe-drop.md): resets
before the group header, lite-07, and surfacing a drop to readers.

## Plan

Today only the Rust publisher sends SUBSCRIBE_DROP, and only at the end
(`drop_unserved` in `rs/moq-net/src/lite/publisher.rs`). The JS publisher
never sends one, so every sequence it skips leaves its subscriber waiting out
the grace (raised by Codex on moq-dev/moq#5301).

Decided (maintainer, 2026-10-11):

- Rust and JS together, identical behavior, rather than JS catching up to
  Rust's end-of-track sweep alone.
- An early DROP covers:
  - cursor skips: sequences below a SUBSCRIBE_START pin, below
    `position_cursor`/`positionCursor`, or below the pre-06 SUBSCRIBE_UPDATE
    re-pin (moq-dev/moq#5301);
  - max-delay expiry: a group skipped for aging past the subscriber's max
    delay. This also closes the parent's note that end-of-track aging misses
    a gap the subscriber still waits on;
  - explicit gaps, only when the upstream publisher named them: an incoming
    SUBSCRIBE_DROP, or a producer cut. A sequence that simply has not arrived
    is never inferred dropped, since a reordered group may still show up.
- Forwarding an upstream drop needs the track model to record a sequence as
  dropped. Add that per-sequence mark to both models (`rs/moq-net/src/model/track.rs`,
  `js/net/src/track.ts`), set by an incoming SUBSCRIBE_DROP and by a producer
  cut, and have both publishers forward it as an early DROP. The parent
  later reads the same mark to surface drops to readers. The mark must survive
  the cache reclaiming the slot (see the parent's notes from #4998).
- The end-of-track sweep stays as the backstop; a sequence already dropped
  early is not named twice.

Update the lite draft's SUBSCRIBE_DROP text to say a publisher SHOULD send it
as soon as it gives a sequence up. Run `just drafts check`.

Tests, with mock time:

- Mirrored Rust and JS publisher unit tests for each trigger: the subscriber's
  owed range settles at the skip, before the track ends and without the grace.
- A relay forwards an upstream DROP downstream on both stacks.
- A lite-05 and a lite-06 case in `just test interop --tail`: a JS publisher
  skips a group and a Rust subscriber settles without waiting out its grace,
  and the reverse.

## Related

- [SUBSCRIBE_DROP](/quest/m1/subscribe-drop.md) - the parent: lite-07, resets before the header, readers
- [Tail arrivals](/quest/m1/tail-arrivals.md) - per-track live dispositions survive cache eviction
