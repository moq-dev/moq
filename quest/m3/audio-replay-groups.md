# [S] Rust retained audio replays in coarse groups

## Goal

`moq-audio`'s retained replay groups history as coarsely as the catalog's `sync.interval`
promises, as `@moq/publish` does, so a viewer's replay costs a few streams instead of one per
join interval and the live edge never waits behind it for stream credit.

## Plan

Found 2026-11-24 while fixing the `moq-lite-06` incumbent fallback in scheduled Interop
(see [the interop-green epic](/quest/m0/interop-green/README.md)): `@moq/publish` replayed 2.13 s
of retained audio as 106 one-frame groups, over the 100 streams a peer grants by default, so the
live audio queued for credit at every hop. With a relay granting 40 streams the fallback took
746 ms; grouping the history at the declared interval made it 223 ms.

`moq-audio`'s `Producer::publish` cuts a group every `join` (20 ms by default) whether the packet
is retained history or live, so a lite-06 viewer joining a retained Rust publisher pays the same
cost. Cut history at the broadcast's declared interval (the longest group of any rendition,
which an audio-only broadcast declares as the audio's own join), and keep live audio at `join`.
Mirror the JS test: a replay groups at the interval, the live frames after it at the join.

Decided 2026-11-24 (maintainer, /quest-plan, from
[moq-dev/moq#9552](https://github.com/moq-dev/moq/pull/9552)):

- History groups at the catalog's declared `sync.interval`: ✅ declared interval (rejected: one
  group for the whole replay, equally fast but it breaks the `sync.interval` promise HLS/DASH
  export cuts segments on; raising stream credit, which only moves the cliff).
- ✅ Keep this as its own m3 quest (rejected: fold into #9552, which would delay the interop fix;
  drop), since no CI case publishes retained audio from Rust today.
