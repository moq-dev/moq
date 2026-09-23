# [S] GStreamer and OBS publish under epochs

## Goal

`moqsink` and the OBS plugin publish each run under a fresh epoch, so a
restarted pipeline or a stop and start in OBS is a clean takeover for viewers.

## Plan

`moqsink` takes the origin default per session. When
[#3115](/quest/m2/3115-moqsink-the-publication-has-no-generation-so-a-flush.md)
lands, each of its publication generations is a new epoch. OBS gets it
through libmoq. Update `doc/bin/gstreamer.md` and `doc/bin/obs.md` if they
show paths.

## Required

- [Bindings](/quest/m1/broadcast-epoch/bindings.md) - OBS publishes through libmoq
