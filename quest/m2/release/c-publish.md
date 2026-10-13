# [S] Published generated C runs the samples

## Goal

The generated moq-c release archive installs, and every doc/lib/c sample and
the C interop client build and run against it from a clean consumer checkout.

## Plan

The m1 C line proves the same cases with the built archive before the cut.
This quest proves registry/release delivery after publication and records the
artifact versions and results. It cannot block the cut that creates them.
Coordinate the final hand-written package notice before replacing its
publication; retain the source until its retirement condition is satisfied.

## Required

- [Breaking release cut](/quest/m2/release/cut.md) - prepared code reaches release before artifact publication and installed proof

- [Generated C preparation](/quest/m1/c/README.md) - package and consumers pass before publication
