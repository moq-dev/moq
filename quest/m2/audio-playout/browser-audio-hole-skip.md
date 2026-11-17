# [M] Browser audio keeps a hole a late burst still fills

## Goal

`@moq/watch`'s audio no longer skips a missing group that lands a moment
after the group past it: the Chromium `bursty` and `congestion` rows of
`just test audio-quality` grade no worse than before
[moq-dev/moq#8799](https://github.com/moq-dev/moq/pull/8799) gave the audio
consumer the player's clock, while the Firefox `after resume` window that PR
fixed stays audible.

## Plan

Native evidence, from the PR that gave the Rust consumer the same clock
(`just test audio-quality --runtimes native --codecs opus --duration 40
--seed 7`): ending an audio hole once the clock reaches the group past it
took `bursty` from 3 to 155 underruns a minute. A burst's groups land late
together, so the group past a hole is already due when the missing one lands
a moment later in the same burst. Ending it at the clock plus the 200 ms
stretch held `bursty` level but left `congestion` worse than the native
estimate wait (A/V lag p95 808 to 946 ms against 288 to 631 ms over three
runs each). `moq play` gives the clock to video only for that reason.

`js/watch/src/audio/decoder.ts` passes `playhead: this.sync` with a
`wait` of `sync.out.maxDelay`, so the browser has the rule that measured
worst natively. First measure: the Chromium `bursty` and `congestion` rows
with and without that `playhead`, a few runs each, since `congestion` is
noisy.

If the browser regressed, recommended: audio waits a hole out for its
playout estimate from the arrival of the group past it, as native audio does
(`rs/moq-audio/src/decode/consumer.rs`, `set_wait(self.delay())`), instead of
the playhead. That still ends the resume hole within one playout delay.
Confirm with #8799's repro, `taskset -c 4-5 nix develop --command just test
media --media-engines firefox --media-cases rename,pause`. Video keeps the
playhead.

## Related

- [A hole is waited for by the subscription's delay](/quest/m2/delay-hole-wait.md) - removes the separate wait this would keep for browser audio, so the two settle the same rule
