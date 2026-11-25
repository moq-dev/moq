# [M] A relay's rejoin keeps the retained audio its player asks for

## Goal

On the Nightly's runner class (`ubuntu-24.04-arm`), full `just test media --media-engines firefox`
runs pass `rejoin reaches live` and `reattach reaches live` without loosening their bounds. When a
relay's lingering copy resubscribes, the retaining audio publisher replays the part of its retention
the player's join reaches, so the player doesn't rejoin across a gap.

## Plan

Found while validating [moq-dev/moq#9572](https://github.com/moq-dev/moq/pull/9572). After that fix,
5 of 24 full Firefox runs on ARM failed `rejoin reaches live` or `reattach reaches live`, against 0 of
24 for `main` with the fix reverted ([run 45537896980](https://github.com/moq-dev/moq/actions/runs/45537896980),
[run 45545588155](https://github.com/moq-dev/moq/actions/runs/45545588155)). At those steps the
encoder's inputs match in both variants, and so do the players' worst underflows (rejoin median
about 600 ms, tail up to 1.7 s on `main` and 2.2 s with the fix). So the split is unexplained, and
the gap below is common to both.

A temporary trace of the fixture's audio demand shows the same order at every rejoin and reattach:

- The relay's copy lingered, so it answers the lite-06 player's TRACK itself, and the fixture's
  audio becomes wanted and subscribed together.
- The relay's first upstream SUBSCRIBE is `live(min 0, max 2080 ms)`. `replayBudget` takes the
  smaller bound, 0, so the encoder expires its whole retention.
- The update a moment later, `live(no min, max 2183 ms)`, would have kept 2.2 s, but nothing is
  left to replay. The player skips 1 to 4 s of missing audio groups and underflows.

Find why a lite-06 join reaches upstream with a min of 0 first. A lite-06 SUBSCRIBE states no lag,
so its join should be bounded by its max alone. Fix that at the relay (Rust and JS alike), not in
the encoder. Then measure both steps on ARM before and after with a temporary workflow, as
[moq-dev/moq#9572](https://github.com/moq-dev/moq/pull/9572) did.

Public API: none expected. Wire: none expected.
