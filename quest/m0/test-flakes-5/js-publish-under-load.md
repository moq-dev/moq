# [S] js/publish's drift and backlog encoder tests pass under load

## Goal

`js/publish`'s audio drift tests (`src/audio/drift.test.ts`) and
`a backlog encode refusal aborts its rendition when demand arrives` (`src/audio/encoder.test.ts`)
pass in a loaded `just ci`, fixed at their cause.

## Plan

Seen 2026-11-27 in a `just ci` run at load average 40, on a branch that didn't touch `js/publish`:
three drift tests and the backlog refusal test timed out, then passed on rerun. Reproduce under
load first, then move each off wall-clock deadlines (a paused or manual clock, or an awaited event),
or fix whatever in the code under test is slow.

Public API: none. Wire: none.
