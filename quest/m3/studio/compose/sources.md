# [M] Source tracks

## Goal

Each local source in a `moq-studio` scene (camera, display, window, image,
microphone) publishes as its own track in the studio's broadcast, listed in
its catalog, alongside the program. Each encodes while it is in the
collection, whatever the subscriber demand, and uploads only while something
subscribes to it.

## Plan

Encode like every first-party publisher (decided 2026-10-28, from
[#7101](https://github.com/moq-dev/moq/issues/7101)): an unwatched source
still encodes, so a late viewer or a [replay](/quest/m3/studio/replay.md)
finds its recent past, but costs no upload. Static sources (images, colors) publish once rather
than as a video stream. Consumer GPUs cap concurrent hardware encode
sessions, so when the hardware refuses one, that source's track fails loudly
with the reason shown rather than silently degrading.
Track names stay stable for a source's life, and an edited source that
changes what it shows gets a new name.
