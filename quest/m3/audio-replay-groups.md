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
