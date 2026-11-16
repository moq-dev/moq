# [M] A hole is waited for by the subscription's delay, with no third knob

## Goal

The container consumer has no separate hole wait: `set_wait` in
`moq_mux::container::Consumer` and `ConsumerProps.wait` in `@moq/hang` are
gone. A missing group is waited for the subscription's stated `Delay::min`,
or `Delay::max` when no minimum is stated, in Rust and JS alike. A native
audio player whose playout delay is a live estimate still skips a hole once
that delay has passed, as it does today.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit):

- ✅ Fold the hole wait into `Delay` (rejected: keep `set_wait` as a third
  delay knob and stop calling it a duplicate; drop it with no replacement
  rule). `Delay { min, max }` already says how far behind live the reader
  plays and how late a group stays useful; a third duration set through a
  setter is one more thing each player has to keep in step.

Facts found by the audit:

- `set_wait` (`rs/moq-mux/src/container/consumer.rs:1381`) caps the wait at
  the max delay (`:840`). Its one production caller is native audio, which
  refreshes it before every poll with `self.delay()`
  (`rs/moq-audio/src/decode/consumer.rs:598`), the live jitter estimate when
  `Options::estimate` is on (`:339`). So the [API review](/quest/m3/release/api-review.md)'s
  note that native audio "sets it to the `min` it already passes" is not
  always true, and a plain delete regresses
  [moq-dev/moq#8531](https://github.com/moq-dev/moq/pull/8531).
- JS mirrors the knob as `ConsumerProps.wait`
  (`js/hang/src/container/consumer.ts:43`), passed by two callers, both as
  `sync.out.maxDelay` (`js/watch/src/audio/decoder.ts:921`,
  `js/watch/src/audio/replay.ts:260`).
- Three days after #8531,
  [moq-dev/moq#8799](https://github.com/moq-dev/moq/pull/8799) gave the JS
  consumer the player's clock (`playhead`): a hole ends when the clock
  reaches the group past it. The clock already trails live by the estimate,
  so it carries the moving value the wait was added for.

For the PR, settle the home of native audio's live estimate first. Two ways:

1. Recommended: the playhead is the home. Once
   [native players skip a hole their clock passed](/quest/m2/audio-playout/rust-playhead-holes.md),
   the clock ends a hole at the estimate, and the static rule (`min`, else
   `max`) is only the fallback for a reader with no clock. Prove it by
   deleting the `set_wait` call and running #8531's regression tests
   (`rs/moq-mux/src/container/consumer.rs:2161`, `:2308`, `:2369` and
   moq-audio's reorder tests) with the playhead in place.
2. moq-audio writes its estimate into the subscription's `Delay::min` as it
   moves. `min` is Subscriber Min Age on the lite-07 wire, so each change is
   a SUBSCRIBE_UPDATE, and since [a cold join reaches back by it](/quest/m2/min-age-join.md)
   it would also move where a join starts. Take this only with a coarse step
   and a measured update rate, and say why the playhead did not do.

Then delete the setter and the prop, apply the one rule in both languages,
and update the consumer docs that name the wait
(`rs/moq-mux/src/container/consumer.rs:74`, the `ConsumerProps` docs). If
neither way keeps #8531's behaviour, report it on the PR before keeping the
knob: the maintainer rejected that end state.

Public API: `moq-mux` loses `container::Consumer::set_wait`; `@moq/hang`
loses `ConsumerProps.wait`. Wire: none.

## Required

- [Native players stop waiting on a hole their clock passed](/quest/m2/audio-playout/rust-playhead-holes.md) - gives the Rust consumer the clock that replaces the live wait

## Related

- [A cold join reaches back by Min Age](/quest/m2/min-age-join.md) - gives `Delay::min` a reader on the wire, which rules out using it as a fast-moving estimate
- [Native audio playout is a library](/quest/m3/audio-playout-library.md) - moves the placement this wait feeds into moq-audio
