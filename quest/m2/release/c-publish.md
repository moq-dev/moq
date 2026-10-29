# [S] Published generated C runs the samples

## Goal

The generated moq-c release archive installs, and every doc/lib/c sample and
the C interop client build and run against it from a clean consumer checkout.

## Plan

The generated C line proves the same cases with the built archive before
publication. This quest proves registry/release delivery after publication
and records the artifact versions and results. The hand-written `moq-c` 0.7
ships at the m1 cut (decided 2026-10-25), so the generated 0.8 supersedes a
published package; its source stays until
[c-retire](/quest/m2/release/c-retire.md).

## Required

- [Breaking release cut](/quest/m2/release/cut.md) - prepared code reaches release before artifact publication and installed proof

- [Generated C bindings](/quest/m2/c/README.md) - package and consumers pass before publication
