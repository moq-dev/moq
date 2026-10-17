# [S] noq releases the acknowledged-close fix on 1.3.x

## Goal

A `web-transport-moq` 1.3.x after 1.3.5 is on crates.io with the
acknowledged-close fix from [moq-dev/noq#40](https://github.com/moq-dev/noq/pull/40),
so a refused WebTransport session delivers its CLOSE capsule before teardown.
Check crates.io for a 1.3.x newer than 1.3.5. Delete this condition once it
ships.

## Plan

Backport noq#40's merge commit (e27c0ce) to noq's `release/1.3`, following
its existing `-1.3` fix branches, with both regressions:
`close_waits_for_capsule_ack` and `close_ack_wait_ends_with_connection`.
Never move the published line to 2.x as the fix.

Public API: none. Wire: none; the CLOSE capsule is delivered, not changed.

## Related

- [Release pin](/quest/m0/refusal-close-backport.md) - adopts the published fix
