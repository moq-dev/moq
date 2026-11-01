# [S] A JS relay starts an unresolved copy's subscription at its newest group

## Goal

On lite-01..05, a SUBSCRIBE with no start group served by a JS publisher from a track copy that has no groups yet starts at the newest group once the copy resolves, instead of replaying the whole cache from group 0, matching the Rust fix in [moq-dev/moq#7328](https://github.com/moq-dev/moq/pull/7328).

## Plan

Found by the agent on [moq-dev/moq#7328](https://github.com/moq-dev/moq/pull/7328). `cursorFloor` (`js/net/src/lite/publisher.ts:419-423`) returns `latest ?? 0` and is called once right after subscribe (`:890`) with `hooks.latestSequence(track)` (`js/net/src/track.ts:2249`); a related fallback is at `:1290`. Rust's fix re-reads `latest_sequence()` once the track resolves.

Decided 2026-10-30 (maintainer, follow-up planning after the /quest-afk run). Port the Rust rule rather than invent a JS one: an unresolved copy defers the floor until it resolves, then floors at the newest group. Test a JS relay serving a copy that resolves after the SUBSCRIBE arrives.

Public API: none. Wire: none.
