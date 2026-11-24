# [M] A Firefox player on ARM keeps running through every resume

## Goal

On the Nightly's runner class (`ubuntu-24.04-arm`), a full `just test media
--media-engines firefox` run's lifecycle player skips no audio groups as missing
after a resume (pause, rejoin, or reattach), because its main thread no longer
stops for hundreds of milliseconds applying the resume's burst of `moq-lite-06`
responses and replayed groups.

## Plan

[moq-dev/moq#9524](https://github.com/moq-dev/moq/pull/9524) found and removed a
harness-only third of the stop: Playwright's Juggler makes every page a
debuggee, so Firefox recorded an async stack at every await and promise
reaction (`js::SavedStacks::saveCurrentStack`, 24 to 33% of the resume's
samples), which no user pays. With `javascript.options.asyncstack` off in the
harness, 12 plain full Firefox runs on ARM passed 12 of 12 (11 of 12 before),
and resume audio skips over 0.5 s fell from 17 to 9, in
[run 45338455063](https://github.com/moq-dev/moq/actions/runs/45338455063):

| Resume | Skips, harness as on main | Skips, async stacks off | Typical skip |
| --- | --- | --- | --- |
| Pause | 7 of 12 runs | 4 of 12 | 3.0 to 3.5 s |
| Rejoin (unsubscribe) | 7 of 12 | 5 of 12 | 1.2 to 2.2 s |
| Reattach (detach) | 3 of 12 | 0 of 12 | 2.8 to 3.0 s |

What remains, measured in that PR with Gecko profiles (`MOZ_PROFILER_STARTUP=1`,
`MOZ_PROFILER_SHUTDOWN=<dir>/p-%p.json`, `MOZ_PROFILER_STARTUP_FEATURES=nostacksampling`
for LongTask markers alone, `js,cpu` for stacks; Playwright's Firefox honors them):

- Each resume is one long task on the player's main thread, a WebSocket
  `message` event (the Firefox player falls back to QMux over WebSocket, which
  packs records up to 16382 bytes into one message). On ARM with async stacks
  off, markers only: pause 60 to 530 ms, rejoin a median near 260 ms, reattach
  near 360 ms. On x86 the same tasks are 70 to 90 ms.
- The burst grows with the track's history, not the join's window. The audio
  track is keyed by µs timestamps, and before lite-07 there is no SKIP, so the
  relay sends one SUBSCRIBE_DROP per gap between groups (about 52 a second), and
  at each join `TrackRun::resolve` in `rs/moq-net/src/lite/publisher.rs`
  forwards every final drop below the start too, deliberately, for a
  downstream relay filling below it. A group key sits between each pair of
  gaps, so none merge. Locally the bursts were 140 drops at the pause resume,
  394 at the rejoin, and 627 at the reattach.
- Stacks with async stacks off spread the cost: the lite subscriber's
  `runResponses` about 15%, `Track` `#askFills` about 14%, zod parsing about
  6%, and the rest across `#tryRecvGroup`, `#tryDrop`, `#drop` (which walks
  every `#cached` entry per drop), and hang's consumer.
- Ruled out: applying a burst's buffered responses together. Decoding them
  first awaits once per response, so the replayed group streams land while the
  drops are still unapplied, and each arrival judges hundreds of open gaps
  (`#askFills`, `#landing`, `Ledger.gaps`): a full local run's rejoin stop grew
  from 71 to 250 ms and ARM's doubled. Only the pause resume improved.
- Ruled out by [moq-dev/moq#9511](https://github.com/moq-dev/moq/pull/9511):
  the retained-audio replay of 2.13 s.

Next, cut the per-resume work to what the join's window needs, at the root:

1. Decide whether a relay must name a lite-06 subscriber's final drops below
   its start. They exist so a downstream relay filling below the start learns
   the keys' fate; a player never reads there. Options: name them as runs
   coalesced across the groups between (needs a wire change, so lite-07 or
   later only), or send them only once the subscriber asks below its start (a
   fill already answers a missing key). Model it with `/verify-subscribe`.
2. Profile the rest on x86 with `taskset` to one or two CPUs, where it shows
   the same shape, and remove per-drop or per-arrival walks over the whole
   cache or drop set in `js/net/src/track.ts`.

Measure the stop and the skips on ARM before and after with a temporary
`pull_request` workflow, as #9524 did. Loosening the bounds or retrying is not
a fix.

Public API: none expected. Wire: none unless option 1 picks the coalesced runs.
