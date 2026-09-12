# [M] Give Connection one recovery and terminal-state contract

## Goal

Changing `share` or transport options does not change whether refreshed
credentials can recover a connection, and `closed` always describes the
handle's actual terminal state.

## Plan

At dev `e2350b39a`, the shared path rejects `Connection.closed` on an auth
failure but leaves its Effect alive (`js/net/src/connection/pool.ts:214`).
Changing URL can acquire another loop and reconnect with an already-rejected
`closed`. The private path creates one Reload (`:229`), whose auth failure
permanently closes its Effect (`connection/reload.ts:316,342`); changing URL
cannot revive it. These are source-traced behaviors, not runtime proofs yet.

Real caller: moq.pro `app/src/lib/live.svelte.ts:75,113` renews JWTs and sets
the connection URL. The audit read `/home/kixelated/work/moq.pro`; no consumer
files were changed. Its pinned API is older, so preserve this use case rather
than treating old type names as defects in the new API.

Decided: a new URL can recover the same handle;
expose attempt failures separately and reserve `closed` for final disposal.
Align the observable close result with other net
handles where practical; settle the chosen type before implementing it.

Test shared/private auth failure followed by URL replacement, exhausted
retries, disable/re-enable, explicit close, and lease cleanup. Ensure a
terminal handle cannot reconnect and a recoverable one has not signalled
terminal closure. Use existing connection tests plus the external consumer
fixture; no timer-based workaround for the state-machine discrepancy.

Public API: likely breaking lifecycle/close observation changes. Wire: no
format change. Run JS checks and tests and the browser reconnect proof.

## Related

- [Close classification](/quest/m1/js-close-classification.md) - owns stream failure classes and media error visibility, not Connection lifetime
