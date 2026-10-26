# [M] Source tracks

## Goal

Each local source in a `moq-studio` scene (camera, display, window, image,
microphone) publishes as its own track in the studio's broadcast, listed in
its catalog, alongside the program. Each is encoded only while something
subscribes to it.

## Plan

Follow the pull model the program already uses: an unwatched source costs
no encode and no upload. Static sources (images, colors) publish once rather
than as a video stream. Consumer GPUs cap concurrent hardware encode
sessions, so when the hardware refuses one, that source's track fails loudly
with the reason shown rather than silently degrading.
Track names stay stable for a source's life, and an edited source that
changes what it shows gets a new name.
