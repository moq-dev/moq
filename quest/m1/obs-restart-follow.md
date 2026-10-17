# [S] The OBS source follows a broadcast's restarts

## Goal

The OBS MoQ source keeps playing across an epochless GOAWAY drain or a
publisher replacement. When `AnnouncedBroadcast::available()` hands out the
next publisher instance, the source drops its catalog and track readers for
the old one and subscribes to the new one, without blanking for longer than
the new instance takes to deliver a keyframe.

## Plan

`cpp/obs/src/moq-source.cpp` calls `available()` once (`moq_source_on_connect`)
and stops when its catalog ends, so a replaced instance leaves the source
stopped once the old route dies and its tracks end with an error. Since moq-ffi's `available()` now
resolves each instance in turn, re-arm it after every instance and rebuild the
`Connection`'s consumer, catalog and tracks on each one, reusing the decoders
when the codec config is unchanged.

Cover it in `cpp/obs/test` with a replacement while the source plays.

Public API: none. Wire: none.
