# [S] OBS publishes under epochs

## Goal

The OBS plugin publishes each Start Streaming under a fresh epoch, so a stop
and start in OBS is a clean takeover for viewers, like every other
first-party publisher in the [broadcast epoch](/quest/m0/broadcast-epoch/README.md)
line.

## Plan

Split from the m0 GStreamer and OBS quest in the 2026-10-05 audit, so the
release gate waits only on `moqsink`.

- OBS gets the epoch through the generated C++ bindings from moq-ffi, which
  the C++ line moves the plugin onto (decided in the 2026-09-30 audit: libmoq,
  now moq-c, gets no new API).
- Nothing mints by default: the plugin mints one per Start Streaming and
  announces it through the route.
- Show the epoch in the dock, and update `doc/bin/obs.md` if it shows it.

## Required

- [C++ through moq-ffi](/quest/m1/cpp/README.md) - the plugin publishes through the generated C++
- [Bindings](/quest/m0/broadcast-epoch/bindings.md) - moq-ffi exposes epochs
