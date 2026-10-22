# [M] JS frames

## Goal

The js/net frame path (`js/net/bench/frames.ts`) is no slower than
`release` when measured in a browser, or the gap is accepted with a reason
recorded in the [epic](/quest/m1/release-perf/README.md).

## Plan

On bun (2026-10-21), the lite rows were 1.2 to 1.3x slower and the IETF rows
1.3 to 2.2x. JS perf is judged in a browser, so measure there first, against
`release` (the bench file is the same on both branches).

Leads from reading code:

- Lite and IETF: [#4226](https://github.com/moq-dev/moq/pull/4226) turned
  `Signal.race` into the lazy `Changed` class. Each wait allocates a `Changed`,
  the `then` getter builds an extra Promise, and `#finish` adds a microtask
  hop. Every reader wake in `group.ts` `#changed()` pays for all of that.
  Resolve listeners synchronously when the winner is not a thenable, and add a
  race-heavy case to `js/signals/bench/effect.ts`.
- IETF: per-group `track.writeGroup` to `ordered.nextGroup` cost. Candidates
  are [#5803](https://github.com/moq-dev/moq/pull/5803),
  [#5725](https://github.com/moq-dev/moq/pull/5725),
  [#5836](https://github.com/moq-dev/moq/pull/5836),
  [#5930](https://github.com/moq-dev/moq/pull/5930), and the eager `new Error()`
  plus `FinalizationRegistry` per `Effect` from
  [#4876](https://github.com/moq-dev/moq/pull/4876), if a path creates
  Effects per group.
