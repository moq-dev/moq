# [S] Timestamp IDs handle the two legacy edges #9794 accepted

## Goal

The two cases [moq-dev/moq#9794](https://github.com/moq-dev/moq/pull/9794) accepted (its decision
G) behave correctly on the code `group-frame-timestamps` leaves:

- A draft-23 subscription bounded before a missing object, served from a draft-22 copy without a
  gap mark, is answered instead of refused.
- A lite-05/06 group whose timestamps decrease never advertises a draft-23 Largest Object past the
  decrease.

## Plan

Decided 2026-11-28 (maintainer): fix both rather than leave them accepted (rejected: keep decision
G's acceptance). Both need two failures at once, and the first runs through the draft-18..22 path
due for deletion at the next bump; if that path is already gone when this starts, the first case is
moot. Start after group-frame-timestamps, which rewrites legacy ingress re-keying and draft-23 ID
assignment, so this fixes the rewritten code instead of colliding with it (rejected: folding into
that lite-07 blocker; starting now beside it).

Public API: none. Wire: none beyond refusing or bounding what draft-23 already forbids.

