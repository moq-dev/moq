# [XS] Watch caches audio codec support

## Goal

`js/watch`'s audio source checks codec support once per rendition config, as
video does, so an unmute resumes the AudioContext without waiting on a support
check.

## Plan

The implementation and focused validation are present in
[moq-dev/moq#7248](https://github.com/moq-dev/moq/pull/7248). Audio follows the
video cache pattern and shares its internal key protocol. Built-in probes reuse
the decoder configuration; custom probes include all config fields. Removed
renditions are pruned, rejected or canceled probes are not cached, and close
releases the cache.

Remaining: pass the full local gate before marking the PR ready. Two
`quest gate just ci` attempts failed in mbx with `Argument list too long`
during the WASM build and the Rust lint-tooling test. Removing unused Qt and
build-host flags from the environment did not resolve it. Keep this external
tooling failure outside the audio implementation's scope.

Verified: 429 watch tests, TypeScript checking, configuration invalidation,
probe replacement/removal/cancellation/close, the real libav support probe, and
AudioContext suspension/resumption. A Chromium 156 benchmark with baseline and
identical A/A controls confirms no additional native support queries on warmed
mute/unmute updates. The benchmark runs in the nightly JS series.

After the gate passes, resolve relevant findings, remove this quest and its
links, and mark the PR ready. Do not merge or enqueue it under the current
PR-only instruction.

Public API: none. Wire: none.
