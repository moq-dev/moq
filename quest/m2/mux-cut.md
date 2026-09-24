# [M] One cut in moq-mux

## Goal

`moq_mux::container::Producer::cut(end)` closes the open group and writes the
discontinuity marker group, exactly as JS `Container.Legacy.Producer.cut`
does, and `discontinuity()` is gone from the producer and every codec
importer. One call declares a break everywhere, and moq-ffi and libmoq
encoders that already call `cut(None)` mark their pauses for free.

## Plan

Fold `discontinuity()` (`rs/moq-mux/src/container/producer.rs`) into `cut`,
delete it from the h264, h265, aac, opus, and legacy importers, and move its
callers (moq-audio's encode producer, moq-boy) to `cut`. Every existing
`cut(None)` caller (moq-ffi, libmoq, moq-gst, moq-cli) now emits a marker at
end of stream too, which subscribers already tolerate. Suggested by #3982.

Public API: breaking in published moq-mux, so it targets `dev`. Wire: none;
the marker group already exists.
