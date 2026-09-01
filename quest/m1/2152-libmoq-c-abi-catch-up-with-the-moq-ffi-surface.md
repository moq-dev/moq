# [S] libmoq: C ABI catch-up with the moq-ffi surface

## Goal

Implement and verify the behavior tracked in [#2152](https://github.com/moq-dev/moq/issues/2152)
within the issue's stated scope and boundaries.

## Plan

Rescoped during the 2026-08 grooming: subscription options, track info, abort
codes, and client TLS roots landed in dev's rs/libmoq. Remaining gaps:
fetch_group, dynamic track/broadcast serving, server-side accept, and
datagrams, tracked against the dev FFI surface.

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

Video decode is the widest hole: `moq-ffi` depends on `moq-audio` but not
`moq-video`, so the UniFFI bindings only ever see raw encoded frames, and
`libmoq`'s `moq_consume_video_raw` is H.264-only with no format or resolution
knob. `moq play` is the worked example of what the shape should be.

## Closes

- [#2152](https://github.com/moq-dev/moq/issues/2152) - close this issue when the quest finishes
