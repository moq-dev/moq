# [S] moq play keeps its audio output open across a replacement

## Goal

When a broadcast is replaced or falls back, `moq play` switches to the new
run without closing and reopening its audio output. The switch then costs
only the configured routing, codec, and playback delays.
`just test native-play` passes repeatedly within its derived 1495 ms bound.

## Plan

Today each new instance opens its own speaker (`rs/moq-cli/src/play/media.rs`
calls `self.output.speaker()` per run). On a loaded host
(`rs/moq-audio/src/playback/driver.rs` "opened audio output") that took 100
to 220 ms per switch, and the PCM after the switch stayed glitchy past the
bound in 3 of 4 native-play runs while landing
[moq-dev/moq#6548](https://github.com/moq-dev/moq/pull/6548). Keep one output
for the player's lifetime and hand it from run to run. Reopen it only when the
new run's rate or channel layout differs. Then rerun the native-play lane
several times on a loaded host, and fix any glitch that remains at its cause.

Public API: none expected. Wire: none.
