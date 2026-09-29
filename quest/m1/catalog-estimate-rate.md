# [S] A rising catalog estimate republishes at most once a second

## Goal

A publisher whose `jitter` or `delay` estimate keeps rising under load stops
republishing the catalog on every 1 ms step. Estimate-driven republishes are
rate-limited to one per second in `js/publish` and `moq-mux`: the first rise
publishes at once, and later rises inside the window coalesce into one publish
at the window's end carrying the latest value. Track additions, removals, and
config changes still publish immediately.

## Plan

Found in [#4529](https://github.com/moq-dev/moq/pull/4529): under load, the
lifetime-maximum `Estimator` (`js/publish/src/jitter.ts`, mirroring
`moq_mux::catalog::Estimator`) rises 1 ms at a time and each rise republishes
the catalog, which makes every player send a subscribe update on every track.

Decided with the maintainer:

- Rate limit, not quantization, so the advertised value stays exact.
- Leading and trailing edge, 1 s window: a first estimate arrives without
  delay and the catalog never trails the estimate by more than a window.
- Estimate-driven changes only; a structural catalog change never waits.
- Keep the draft's never-lower rule. #4529 also saw an estimate grow to about
  586 ms on an overloaded machine, so resuming refilled that whole delay. That
  is the rule working as written ("a burst it emitted once it may emit
  again"), not a bug for this quest.

A pending trailing publish folds into any structural publish that happens
first. Unit tests drive the window on a mocked clock in both languages.

Public API: none expected. Wire: none; catalog contents are unchanged, only
how often they are sent.

## Related

- [Audio jitter target](/quest/m0/audio-jitter-target/README.md) - the player-side target the advertised jitter floors
