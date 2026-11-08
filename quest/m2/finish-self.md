# [S] Finishing consumes the producer

## Goal

Every public importer and producer whose `finish` takes `&mut self` takes
`self` instead, so a finished producer cannot be written to and no runtime
"already finished" refusal is needed. moq-rtmp and moq-srt already consume
on finish. A `Clone` producer that shares its state (moq-json's
`snapshot`, `stream`, and `window` producers, and moq-mux's
`catalog::Producer`) still consumes the handle it finishes with, but its
other clones can still write, so it keeps the shared runtime refusal.

## Plan

Decided 2026-11-07 (maintainer, /quest-plan from the API sweep of open PRs
and remaining quests): ✅ settle it as m2 before the cut (rejected: leaving
it to [a failed finish is final](/quest/m1/explicit-state/finish-final.md)'s
"if this child takes it", which ships `finish(&mut self)` plus a refusal or
breaks after the cut). finish-final keeps the internal state work; this
quest changes the signatures. A `finish` that can fail returns the
producer in its error when the caller can retry, and the PR says which do.
List every changed signature with its migration in the Impact section,
including moq-ffi and moq-c, whose handles keep a runtime check because a
foreign caller can't be consumed.
