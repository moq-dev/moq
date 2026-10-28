# [S] The JS audio encoder re-enables without a probe

## Goal

Re-enabling a disabled `Audio.Encoder` republishes the catalog as enabled and
encodes the first captured frame at once, without building a new probe
encoder. Resumed chunks carry their capture timestamps.

## Plan

Traced 2026-10-28 from [#7101](https://github.com/moq-dev/moq/issues/7101):
`enabled = true` builds a new probe `AudioEncoder` (encode and flush,
`js/publish/src/audio/encoder.ts:457-535`), and the catalog stays disabled
until it finishes (`:545-569`). That costs ~10-30 ms.

Decided 2026-10-28 (maintainer): keep the probe result (the encoder
settings and decoder description) across a disable while the capture format
and settings are unchanged. A changed format or settings probes again, as
today.

Keep the reset at a timestamp gap (`:693-702`). Chrome stamps encoder output
from its first input plus the samples encoded since, ignoring later jumps.
Without the reset, chunks after a mute would trail capture time by the mute's
length. The reset also drops only chunks from before the gap. Rejected:
rebasing output timestamps by hand to avoid the reset, which adds a second
clock for a cost of under a frame.

Tests: a disable/enable cycle with unchanged capture flips the catalog in the
same tick, without a new probe. After a multi-second mute, resumed chunk
timestamps match capture time. A changed sample rate probes again.

Public API: none. Wire: none.
