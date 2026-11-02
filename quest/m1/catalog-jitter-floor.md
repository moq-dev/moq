# [S] A reconfigure keeps the published jitter and delay floor

## Goal

Re-setting an already-published rendition, such as an H.264 or H.265
same-codec resolution change, never lowers the jitter or delay its catalog
already advertises for a field the new config leaves unset. The reconfigure
publishes with the higher of the two, as a measurement does, instead of
failing with `JitterDecreased` and aborting the TS import.

## Plan

Decided 2026-10-31 (maintainer, the /quest-complete requeue session), found
by the sessions of [moq-dev/moq#7436](https://github.com/moq-dev/moq/pull/7436)
and [moq-dev/moq#7387](https://github.com/moq-dev/moq/pull/7387). Planned
here in m1 as its own [S] rather than fixed inside #7436 (no test there needs
it) or dropped: it blocks the conformance test in #7387 once that fixture is
repaired.

`Rendition::set` (`rs/moq-mux/src/catalog/tracks.rs:655`) resolves the new
config from what it supplies plus the last measurement (`Self::resolved`),
then `check_decrease` refuses anything below the catalog's current jitter or
delay. `Rendition::estimate` (`:711`) already floors its result at the
published values (`resolved.jitter.max(published.jitter)`, `:725-726`).
After a measurement raises jitter mid-run, a reconfigure that supplies no
jitter resolves to the lower detected value and fails. Floor the unsupplied
jitter and delay in `set` at the catalog's published values the same way, so
`check_decrease` still refuses only a value the caller explicitly lowers.

`published_text_jitter_never_decreases` (`:1078`) expects a `None` re-set
after a supplied 100 ms to fail; with the floor it publishes 100 ms. Keep
the explicit-lower case failing and update the `None` case to the floor.

Regression test: a unit test in `tracks.rs` that publishes a video config,
raises its jitter with `estimate`, then `set`s a config with new dimensions
and no jitter, and asserts it succeeds with the raised jitter still
published.

Public API: none. Wire: none.

## Related

- [HLS and DASH segment conformance](/quest/m1/hls-conformance.md) - the conformance test this unblocks
- [HLS export follows a resolution change](/quest/m1/hls-reinit-same-codec.md) - the same-codec reconfigure that hits it
