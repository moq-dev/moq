# [S] GStreamer publishes under epochs

## Goal

`moqsink` publishes each run under a fresh epoch, so a restarted pipeline is
a clean takeover for viewers.

## Plan

`moqsink` takes the origin default per session. When
[#3115](/quest/m2/3115-moqsink-the-publication-has-no-generation-so-a-flush.md)
lands, each of its publication generations is a new epoch. Update
`doc/bin/gstreamer.md` if it shows paths.

Decided in the 2026-10-05 audit: the OBS half moved to
[OBS publishes under epochs](/quest/m1/obs-epoch.md), so the release gate no
longer waits on the C++ line. `moqsink` is Rust on moq-net and needs nothing
from it. moq-c announces through `broadcast::Producer::announce`, so it only
inherits one, since Origin mints it at `create_broadcast`; the OBS quest checks that.


## Related

- [OBS publishes under epochs](/quest/m1/obs-epoch.md) - the OBS plugin's half, after the C++ line
