# [S] The drill catches a subscriber that leaks broadcasts again

## Goal

`Tests (test drill-sensitivity)` passes: with the `subscriber-leaks-broadcasts`
fault applied, `cancel_under_backpressure_releases_the_reader::loopback`
fails, as `test/drill/README.md` says it must.

## Plan

Found by the 2026-11-20 triage of Nightly run
[44767045952](https://github.com/moq-dev/moq/actions/runs/44767045952); it
passed on 11-17 through 11-19.

```
FAIL: cancel_under_backpressure_releases_the_reader::loopback still passed with 'subscriber-leaks-broadcasts' applied; it is not watching this behavior
```

Find the 11-19..11-20 commit that changed this: either the fault no longer
applies to the code path (the patch target moved), or the test no longer
observes the release (a change made the leak harmless or unobservable).
Fix whichever moved, the fault or the test, so the test watches the
behavior again; don't drop the pairing.

Public API: none. Wire: none.
