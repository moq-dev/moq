# [XS] Trace replays gate every PR

## Goal

The recorded-trace replay rows run on any PR that touches `js/watch`,
`js/hang`, or `test/audio-quality`, graded against their exact budgets, so a
regression in the rings or `Sync` fails the PR that caused it instead of the
next nightly. The browser and shaper rows stay nightly.

## Plan

Replays need no browser, relay, or shaper: `run.sh` feeds each trace through
`test/audio-quality/clients/js/replay.ts` and they finish in seconds, so they
are cheap enough for a merge gate, unlike the rest of the matrix. Wire a
replays-only mode into the scoped JS path of `just check` (decided with the
maintainer over folding it into the watch quest's `replay.test.ts`), so CI
and local checks agree.

Replay rows are deterministic, so a change that moves them updates
`budgets.json` in the same PR, with the new numbers in its description.

Public API: none. Wire: none.
