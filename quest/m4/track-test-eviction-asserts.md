# [XS] JS eviction tests prove the eviction happened

## Goal

Every `js/net/src/track.test.ts` test that checks behavior surviving a cache
eviction first asserts the eviction happened, so each one fails with
eviction turned off instead of passing.

## Plan

Decided 2026-11-03 (maintainer, follow-up planning after the 2026-11-03 /quest-afk and /quest-complete runs), milestone:

- m0
- m1
- ✅ m2

Today they pass with eviction off, so they don't guard what they name.
Candidates include "an evicted successor with unread frames still starts the
wall clock" (around line 1902), "a failed successor hands the wall clock on
after the cache evicts it" (around 1944), and "retention eviction surfaces as
a gap for a handed-out group" (around 2040); check the rest of the file the
same way.

- Assert the evicted group is gone from the cache (or the eviction counter
  moved) before the assertion the test is named for.
- Confirm each fails with eviction disabled, then passes as written.

Public API: none. Wire: none.
