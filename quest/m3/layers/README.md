# Layered broadcasts

## Goal

hang specifies a layer layout track: a broadcast's main picture and sound plus
other tracks layered over it, each placed, timed, and mixed on the media
clock. `moq play` and `@moq/watch` composite those layers over the main
picture and mix their audio, so a moq-dev/studio broadcast plays as its
creator composed it, while sources published on their own tracks stay
separately fetchable and toggleable.

This README's own work is the end-to-end proof: one fixture broadcast with a
picture layer and an audio layer plays in `moq play` and `@moq/watch` with the
same placement and timing, within a stated tolerance, and with the layer's
sound mixed in.

## Plan

Decided 2026-11-08 (maintainer, moq-dev/studio /quest-plan):

- The studio publishes the program plus each independently published source
  as its own track, and players composite those sources over the program.
  Rejected: viewers rebuilding the whole scene from every source's track,
  which would take more encoder sessions than consumer GPUs have.
- hang specifies the layout and each player implements it with its own code.
  The studio's compositor (moq-scene) stays in moq-dev/studio, since moq
  never depends on that repository, so parity comes from the shared fixture,
  not shared code. Rejected: moving moq-scene into moq.
- The studio prototypes the format first (`moq-studio --watch`, reading a
  provisional `dev.moq.studio.layers` track), and the spec starts from what
  that prototype settles (rejected: drafting the spec in parallel).
- Players mix layers' audio in, at the gain, mute, pan, and delay the layout
  carries (pan and delay added 2026-11-09, as the spec quest records).
- Each layer carries a priority and whether a viewer may toggle it. A player
  maps priority onto moq-lite's subscription priorities and unsubscribes a
  toggled-off layer.
- m3: moq-dev/studio is the named consumer, and its side sits in its m2.
