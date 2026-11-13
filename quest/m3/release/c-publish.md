# [S] Published generated C runs the samples

## Goal

The generated moq-c release archive installs, and every doc/lib/c sample and
the C interop client build and run against it from a clean consumer checkout.
The smoke client (`test/smoke/clients/c`) checks the published generated
archive instead of the hand-written one.

## Plan

The generated C line proves the same cases with the built archive before
publication. This quest proves registry/release delivery after publication
and records the artifact versions and results. The hand-written `moq-c`
ships at the next cut (decided 2026-10-25), so the generated package
supersedes a published one; its source stays until
[c-retire](/quest/m3/release/c-retire.md). On `main` the two read 0.7.0
(`rs/moq-c/Cargo.toml`) and 0.8.0 (`c/moq/VERSION`), but the cut's
`just version` sets both to the shared minor (`sh/version.py`), so neither
number survives it and both `moq-c` lines carry the same version.

The smoke client moves here rather than with
[C consumers](/quest/m3/c/consumers.md) because it checks a published archive,
and the generated one publishes only after that quest (decided 2026-10-31).
The move also takes the client off `moq_origin_announced_broadcast` onto the
generated `request`/`follow`, so
[Smoke clients use request and follow](/quest/m3/release/smoke-bindings-rename.md)
leaves the C client to this quest.

## Required

- [Breaking release cut](/quest/m3/release/cut.md) - prepared code reaches release before artifact publication and installed proof

- [Generated C bindings](/quest/m3/c/README.md) - package and consumers pass before publication
