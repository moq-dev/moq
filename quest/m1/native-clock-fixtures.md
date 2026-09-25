# [S] Native capture proves the broadcast clock in CI

## Goal

Per-PR CI drives the native video and audio capture publishers through clock
edge cases and asserts the published timestamps: simultaneous A/V, a late
first frame, a restart to zero, a restart after idle, a system-wall
adjustment, and retained archive playback. Anything they catch is fixed here.

## Plan

Native video already maps the device timeline onto `catalog.clock()` at open,
and native audio stamps arrival on it. The fixtures exercise publisher
integration with a synthetic device source and an injected clock, rather than
only the clock helper. No new clock API or catalog representation.

## Related

- [CLI import clock](/quest/m1/cli-import-clock.md) - the same scenarios through `moq import`
