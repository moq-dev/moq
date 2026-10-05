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
- First check whether the plugin already inherits it: today it announces
  through moq-c's `moq_publish_announce`, which calls
  `broadcast::Producer::announce`. If Origin mints the epoch on that path,
  the plugin needs no code, only a test.
- Show the full epoch path in the dock, and update `doc/bin/obs.md` if it
  shows paths.

## Required

- [C++ through moq-ffi](/quest/m1/cpp/README.md) - the plugin publishes through the generated C++
- [Bindings](/quest/m0/broadcast-epoch/bindings.md) - moq-ffi exposes epochs
