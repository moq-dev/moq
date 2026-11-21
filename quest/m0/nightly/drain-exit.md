# [M] A draining relay exits once its last session leaves

## Goal

A relay told to drain exits soon after its last session leaves, so
`Tests (test drain)` and the 6-relay mesh replay's rolls pass again.

## Plan

Found by the 2026-11-19 triage of Nightly run
[44562804083](https://github.com/moq-dev/moq/actions/runs/44562804083); the drain test passed on 2026-11-18.

```
error: relay A was still running 10s after its last session left
```

The 6-relay mesh replay hung the same night on `roll f to new: hung x1` and
`roll b to old: hung x1`, which a relay that never exits would explain.
Likely cause: [moq-dev/moq#9158](https://github.com/moq-dev/moq/pull/9158)
(landed 2026-11-19 10:52 UTC), which holds dialed cluster links until the peer
closes or the deadline passes. Confirm by reverting it locally first. The fix
keeps what #9158 wanted for a live relay while letting a draining one close
its dialed links once nothing needs them.

Caused by: [moq-dev/moq#9158](https://github.com/moq-dev/moq/pull/9158), if
confirmed.

Public API: none expected. Wire: none.
