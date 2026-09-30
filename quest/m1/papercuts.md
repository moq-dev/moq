# [S] Papercuts from the 2026-09-30 spawn

## Goal

Three small fixes found while landing the m0 quests: `@moq/net` refuses to
serve a broadcast it did not produce, `just fix` and `just check` leave the
gitignored `.scratch/` alone, and `remote_wake_unparks` passes under load.

## Plan

- JS republish: `@moq/net` publishes only what it produces (decided with
  moq-dev/moq#4599), naming its own origin. When the lite publisher resolves
  a request to a received route rather than an originated one (the
  `local(..) ?? demand(..)` path in `js/net/src/lite/publisher.ts`,
  `RouteEntry.originated` in `origin.ts`), refuse it loudly instead of
  labeling upstream content with the local origin.
- `.scratch/`: add it to `.taplo.toml`'s excludes and to `.remarkignore`, so
  `taplo format` and `sh/markdown.sh` stop rewriting agents' scratch clones.
  biome, nixfmt, just, and shfmt already skip it.
- `rs/moq-uring/src/worker.rs` `remote_wake_unparks`: observe that the worker
  parked instead of sleeping 50 ms and asserting elapsed wall time, per the
  rule that unit tests mock time.

Public API: none. Wire: none.
