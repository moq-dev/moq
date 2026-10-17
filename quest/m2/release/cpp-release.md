# [S] First C++ package release

## Goal

The OBS release path is dry-run nightly, then the first `cpp-v<version>` tag
is pushed and `release-cpp.yml` publishes one `moq-cpp-<version>-<target>`
archive per target to its GitHub release, plus the
matching `obs-moq-v<version>` plugin release. A maintainer cuts it by hand
once `main` reaches `release`; check with
`gh release list --repo moq-dev/moq | grep cpp-v`.

## Plan

The version is `cpp/moq/VERSION`; the plugin's is `cpp/obs/VERSION`. Delete
this quest once the release exists.

The OBS plugin now ships only with a C++ release, so `obs-moq` stays at its
last moq-c build until this tag. Decided when the C++ line landed on `main`
(#4079): the first plugin built on the C++ package must not drop the
Advanced settings or the dock's protocol and reconnect reason that the moq-c
build had, so those parity quests gate this release.

The [nightly package proof](/quest/m1/cpp-package-check.md) owns pre-cut
preparation. This quest owns tagging and verifying the published downloads.

Decided in the 2026-10-10 audit: the tag also waits for Codecs and the epoch
rename, so consumers take the binding breaks in one
release. The nightly dry-run preparation can proceed independently.

## Required

- [Breaking release cut](/quest/m2/release/cut.md) - prepared code reaches release before artifact publication and installed proof

- [Client settings parity](/quest/m1/obs-client-config.md) - the OBS Advanced settings the migration dropped come back
- [Generated C++ shape](/quest/m1/cpp-generated-shape.md) - each type has one name and `moq::expected` one type before the API ships

- [Nightly package proof](/quest/m1/cpp-package-check.md) - the release path is exercised before the tag
