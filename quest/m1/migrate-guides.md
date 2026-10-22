# [S] Binding migration guides state the net change

## Goal

The `## Unreleased` sections of `rs/moq-ffi/MIGRATE.md` and the wrapper
guides (`py/moq-rs`, `kt/moq`, `dart/moq`, `go/wrapper`, `swift`) describe
what changed from the last published version, with no contradictions.

## Plan

Found by an external API audit on 2026-10-22; fix now so later entries build
on correct ones.

- Broadcast `finish`: each guide says both "the new
  `MoqBroadcastProducer::finish`" (#6157) and "`finish` is removed" (#4152).
  The published 0.4.9 `finish()` was a deprecated alias of `close()`; it now
  returns a `MoqBroadcastFinish` that lingers. Replace both with one entry
  saying that.
- `rs/moq-ffi/MIGRATE.md` says a raw track is timed by default. The
  `MoqTrackInfo.timescale` default is `None` (untimed), and 0.4.9 defaulted
  to microseconds, so the change is timed to untimed by default.
- Async shutdown (7bbac32721): `MoqSession::shutdown` is now async, fallible,
  and drains. Only `swift/MIGRATE.md` says so; add it to `rs/moq-ffi`,
  `py/moq-rs`, `kt/moq`, `dart/moq` and `go/wrapper`.

Read the rest of each section for other entries a later one reversed. The
cut's [migration audit](/quest/m2/release/migrate-audit.md) does this for
every package; this quest fixes the binding guides already known to be
wrong. Public API: none. Wire: none.
