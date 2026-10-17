# [S] GStreamer sinks linger at end of stream

## Goal

A `moq-gst` sink that receives EOS finishes its broadcast instead of closing
it, so subscribers still catching up drain the tail until the media is
stale. Stopping the pipeline (a state change to NULL, or Ctrl-C under
`gst-launch`) ends the wait early.

## Plan

Decided (2026-10-16): publishers call `broadcast::Producer::finish()`, which
returns a `broadcast::Finish` handle, and await `Finish::wait()`; dropping
the handle is the interrupt and closes the broadcast. Tracks without a
max-age add nothing to the wait.

Check when the sink posts EOS to the bus. `gst-launch` exits on it, so
posting it before the wait ends would cut the linger short; posting it after
could hold up pipelines that expect EOS promptly. Recommend one and
document the behavior in `doc/bin/gstreamer.md`.

Test with a mocked clock where the sink's tests allow it.
