# [L] Browser audio takes its holes from the ordered reader

## Goal

`@moq/watch`'s audio reads its track through `@moq/net`'s ordered reader with
a reader budget equal to what the ring holds, and plays each `Gap` the reader
reports as the hole: the audio container consumer keeps no hole wait of its
own and no longer passes `playhead`. Video keeps the playhead. The Chromium
`bursty` and `congestion` rows of `just test audio-quality` grade no worse
than before [moq-dev/moq#8799](https://github.com/moq-dev/moq/pull/8799)
gave the audio consumer the player's clock, and the Firefox `after resume`
window that PR fixed stays audible.

## Plan

Decided 2026-11-16 (maintainer, /quest-run): ✅ the transport decides an
audio hole, through an ordered reader whose budget is the ring's size
(rejected: first measure the playhead rule against a hand-written wait in
the container consumer, as this quest planned before; keep the playhead rule
and the separate wait).

Interim rule, decided 2026-11-17 (maintainer, /quest-run,
[moq-dev/moq#8982](https://github.com/moq-dev/moq/pull/8982)): ✅ browser
audio ends a hole once the group past it trails the clock by the ring's
stretch bound (`playable(sync, output)` in `js/watch/src/audio/playhead.ts`),
the rule #8982 ships (rejected: main's bare clock with only the replay harness
fixed and re-budgeted; the estimate wait, which brought back the Firefox resume
silence). It is the only option with no measured regression in the browser,
and native audio keeps its estimate wait until
[moq-dev/moq#8984](https://github.com/moq-dev/moq/pull/8984) settles one rule.
The work below replaces `playable` with the ordered reader's `Gap`, so
`playable` and the audio consumer's clock go with it.

Why the playhead rule goes: natively, ending an audio hole once the clock
reaches the group past it took `bursty` from 3 to 155 underruns a minute
(`just test audio-quality --runtimes native --codecs opus --duration 40
--seed 7`). A burst's groups land late together, so the group past a hole is
already due when the missing one lands a moment later in the same burst.
Native audio instead waits a hole out for its playout estimate from the
arrival of the group past it (`rs/moq-audio/src/decode/consumer.rs`), and
[moq-dev/moq#8984](https://github.com/moq-dev/moq/pull/8984) makes that wait
the subscription's `Delay::min`. Rust `track::Ordered`
([moq-dev/moq#8910](https://github.com/moq-dev/moq/pull/8910)) gives up on a
missing group by the same two clocks: the live edge's timestamp a budget past
it, or its successor's arrival a budget ago. So the rule lives once, in the
transport.

Work:

- Mirror `track::Ordered`'s `Item::{Group, Gap { range, error }}` in
  `@moq/net`, whose `Ordered` handle (`js/net/src/track.ts`) skips a stale or
  dropped group silently today, with the reader's own budget as in Rust.
  This is #8910's follow-up "mirror `Item` in `@moq/net` once a consumer
  needs it"; the bindings follow when a binding consumer needs it.
- The budget is what the ring holds (`ring.held(delay)` in
  `js/watch/src/audio/decoder.ts`), and follows it as the delay estimate,
  stretch, and output latency move. It is the reader's, not the
  subscription's: the subscription keeps reaching the estimator's 2 s
  ceiling past the delay, since a group the relay expires is never observed
  and the estimate could not rise after underruns.
- The audio container consumer (`js/hang/src/container/consumer.ts`, used
  from `js/watch/src/audio/decoder.ts` and `replay.ts`) reads ordered for
  audio, turns each `Gap` into its hole, and drops its own hole wait and the
  `playhead` it is passed.
- Measure the Chromium `bursty` and `congestion` rows before and after, a few
  runs each, since `congestion` is noisy. Confirm the resume hole with
  #8799's repro, `taskset -c 4-5 nix develop --command just test media
  --media-engines firefox --media-cases rename,pause`.

## Related

- [A hole is waited for by the subscription's delay](/quest/m2/delay-hole-wait.md) - settles the wait rule this moves into the transport
- [moq-net's last pub items with a single container-reader caller go private](/quest/m2/unused-api/net.md) - the Rust container reader moving onto `Gap`, this quest's native counterpart
