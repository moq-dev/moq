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

- The Rust consumer has the clock too
  (`moq_mux::container::Consumer::set_playhead`), and `moq play` gives it to
  video only. Given to native audio it measured worse than the estimate wait
  (`just test audio-quality --runtimes native --codecs opus --duration 40
  --seed 7`): ending a hole at the clock cost `bursty` 155 underruns a minute
  against 3, since a late burst's successor is already due when the missing
  group lands a moment later; ending it at the clock plus the 200 ms stretch
  held `bursty` and `mild` level, but over three runs each `congestion` ran
  1714 to 1925 underruns a minute against 1307 to 1767 without it, and an A/V
  lag p95 of 808 to 946 ms against 288 to 631 ms. The estimate wait gives
  every hole the estimate from the arrival of the group past it, which a late
  burst needs.
- Browser audio ends a hole once the group past it trails the clock by the
  ring's stretch bound, the furthest the ring plays late audio
  ([moq-dev/moq#8982](https://github.com/moq-dev/moq/pull/8982)): the
  variant that lost natively on `congestion`. In the browser it graded no
  worse than the bare clock or the wait alone on the Chromium `bursty` and
  `congestion` rows, and the replays show the bare clock skipping holes a
  burst fills 1 ms later. The estimate wait alone (no clock) left the Firefox
  resume window silent longer (9 to 41 samples against 6 to 12).

For the PR, settle the home of native audio's live estimate first. Two ways:

1. The playhead is the home: the clock ends a hole, and the static rule
   (`min`, else `max`) is only the fallback for a reader with no clock. The
   measurements above count against it for audio unless a variant beats the
   estimate wait on `bursty` and `congestion`. Prove any variant by deleting
   the `set_wait` call and running #8531's regression tests
   (`rs/moq-mux/src/container/consumer.rs:2161`, `:2308`, `:2369` and
   moq-audio's reorder tests) with the playhead in place, plus those rows.
2. moq-audio writes its estimate into the subscription's `Delay::min` as it
   moves. `min` is Subscriber Min Age on the lite-07 wire, so each change is
   a SUBSCRIBE_UPDATE, and since a cold join reaches back by it
   it would also move where a join starts. Take this only with a coarse step
   and a measured update rate, and say why the playhead did not do.

Then delete the setter and the prop, apply the one rule in both languages,
and update the consumer docs that name the wait
(`rs/moq-mux/src/container/consumer.rs:74`, the `ConsumerProps` docs). If
neither way keeps #8531's behaviour, report it on the PR before keeping the
knob: the maintainer rejected that end state.

Public API: `moq-mux` loses `container::Consumer::set_wait`; `@moq/hang`
loses `ConsumerProps.wait`. Wire: none.

## Related

- [Native audio playout is a library](/quest/m3/audio-playout-library.md) - moves the placement this wait feeds into moq-audio
