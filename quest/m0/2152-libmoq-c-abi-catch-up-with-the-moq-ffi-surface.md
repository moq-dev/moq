# [S] libmoq: C ABI catch-up with the moq-ffi surface

## Goal

Implement and verify the behavior tracked in [#2152](https://github.com/moq-dev/moq/issues/2152)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

The libmoq C ABI (`rs/libmoq/src/api.rs`) has fallen well behind moq-ffi. All of these are additive (new symbols), so none block the dev->main merge, but the backlog is getting long:

- **Subscription options**: `moq_consume_track` takes no options; no priority/ordered/stale/group-range equivalent of `MoqSubscription`, and no mid-stream update.
- **Track info on publish**: `moq_publish_track` cannot set `timescale`/`priority`/`ordered`/`cache` (no `MoqTrackInfo` equivalent).
- **Fetch**: no `fetch_group`, and no dynamic group serving (moq-ffi gains these in #2142; mirror the shape).
- **Dynamic track/broadcast serving**: no `requested_track`/`requested_broadcast` path at all.
- **abort with error code**: only clean close/finish exists; no abort(code) for tracks/groups.
- **Server / two-phase accept**: no server-side API (moq-ffi has `MoqServer`/`MoqRequest` with the SETUP path); C embedders cannot accept sessions.
- **Client TLS knobs**: roots/system-roots/fingerprints/disable-verify are env-only; moq-ffi exposes them as options.
- **Datagrams**: tracked with the moq-ffi datagram issue; mirror whatever lands there.
- **Raw-frame timestamps**: raw consume reports `timestamp_us = 0`; tracked with the raw-frame timestamps issue.

Suggest splitting off pieces as they're picked up rather than one mega-PR. Each addition also touches `cpp/obs` consumers only if used, plus `doc/lib/c` per the Cross-Package Sync table.

## Closes

- [#2152](https://github.com/moq-dev/moq/issues/2152) - close this issue when the quest finishes
