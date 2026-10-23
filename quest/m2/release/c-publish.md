# [S] Published generated C runs the samples

## Goal

The generated moq-c release archive installs, and every doc/lib/c sample and
the C interop client build and run against it from a clean consumer checkout.

## Plan

The m1 C line proves the same cases with the built archive before the cut.
This quest proves registry/release delivery after publication and records the
artifact versions and results. It cannot block the cut that creates them.
The hand-written crate never published, so there is no old package to
supersede; its source stays until
[c-retire](/quest/m2/release/c-retire.md).

## Required

- [Breaking release cut](/quest/m2/release/cut.md) - prepared code reaches release before artifact publication and installed proof

- [Generated C preparation](/quest/m1/c/README.md) - package and consumers pass before publication
