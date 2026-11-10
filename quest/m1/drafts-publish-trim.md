# [S] Draft publishing keeps the upload and drops changelog rewriting

## Goal

`sh/drafts/publish.sh` uploads a draft to the datatracker and nothing else
automated: the changelog heading rewriting, about half of its 298 lines, and
most of the 653-line `publish.test.sh` that every `just drafts check` runs
are gone. The 17 drafts publish a few times a year, so the author edits the
changelog by hand.

## Plan

Planned 2026-11-09 (maintainer, /quest-plan from the deletion audit).
Decided: ✅ trim (rejected: keep the automation).

Keep whatever test still guards the upload path. Update `drafts/` docs or
CONTRIBUTING text that describes the removed step.

Public API: none. Wire: none.
