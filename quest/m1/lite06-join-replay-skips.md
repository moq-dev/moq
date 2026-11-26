# [M] A lite-06 cold join plays its replayed audio instead of skipping it

## Goal

A `@moq/watch` player joining a retaining audio publisher over `moq-lite-06` plays the replay it
asked for instead of skipping through it: on the Nightly's runner class (`ubuntu-24.04-arm`), full
`just test media --media-engines firefox` cold starts log no `skipping missing groups` for audio.

## Plan

Found 2026-11-26 while measuring [moq-dev/moq#9620](https://github.com/moq-dev/moq/pull/9620)
([run 45699517110](https://github.com/moq-dev/moq/actions/runs/45699517110)). A lite-06 SUBSCRIBE
states no lag, so its live join is bounded by its max age alone, and a retaining audio encoder
replays about 2 s (`live(min none, max 2080ms)`). Decided 2026-11-23 (maintainer,
[moq-dev/moq#9511](https://github.com/moq-dev/moq/pull/9511)): that replay is correct. But every
lite-06 cold start then logs 3 to 6 `skipping missing groups` for audio
(`js/hang/src/container/consumer.ts`), and the player holds behind live and underflows; the media
steps' bounds absorb it today. Lite-07 joins state their lag (`live(min 130ms, max 2130ms)`) and
don't show it.

Recommendation: first remeasure after [moq-dev/moq#9552](https://github.com/moq-dev/moq/pull/9552)
lands, which replays retained audio as a few interval-sized groups instead of about 100 one-frame
groups that overrun stream credit; that may be why replayed groups arrive late and get skipped. If
the skips remain, find which groups the consumer skips and why they are missing when it reaches them,
and fix that where it starts. Rust and JS alike when the cause is in `moq-net`.

Public API: none expected. Wire: none expected.

## Waiting

- Maintainer: should this be fixed for lite-06 at all, given lite-07 joins don't replay their whole max age and the skips end once players speak lite-07?
  - Fix it (Recommended) - lite-06 is what clients negotiate by default today, so every cold start pays the skips until lite-07 is published and becomes the default
  - Drop it and wait for lite-07 - no work, but lite-06 viewers keep skipping replayed audio until then

## Related

- [moq-dev/moq#9552](https://github.com/moq-dev/moq/pull/9552) - groups `@moq/publish`'s replay at the declared interval; remeasure after it
