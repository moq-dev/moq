# [S] Quiet catalog after a cleared resume floor on release

## Goal

On `release`, a relay that resumes a broadcast after a reconnect serves its
catalog even when the catalog track is quiet and the resume floor has been
cleared, or this quest is deleted because the stall does not reproduce there.

## Plan

#4940 (`35fa44f6b`) fixed the stall on `main`. Triage suspected `release`
has it too (spliced `start_at` only raises) but did not prove it. Reproduce
first with a release-side test; port only if it fails. The publisher half of
#4940 applied cleanly; the resume and track halves need adapting.
