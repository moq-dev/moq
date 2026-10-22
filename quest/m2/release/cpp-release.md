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

As of 2026-10-20 no `cpp-v*` release exists, and `obs-moq` still publishes
from the release line's C build (`obs-moq-v0.6.17`, 2026-10-17); the first
plugin built on the C++ package replaces it at this tag. Decided when the C++ line landed on `main`
(#4079): the first plugin built on the C++ package must not drop the
Advanced settings or the dock's protocol and reconnect reason that the moq-c
build had; that parity landed in #5726 and #6131.

The nightly C++ and OBS release checks pass: the
[2026-10-20 Nightly](https://github.com/moq-dev/moq/actions/runs/39421743227),
which failed only in unrelated lanes, checked the installed consumers of all four packaged archives and built both
OBS plugins against them. It packages this run's prebuilt moq-ffi input,
while a tag with `cpp/moq/FFI_VERSION` unset builds moq-ffi from source, a
path no nightly checks. So if the pin is still unset, first dispatch
`release-cpp.yml` on the ref to be tagged without `prebuilt-test`: it runs
the tag's source build and archive check on all four hosts, then both OBS
builds, and publishes nothing. This quest owns that dry run, tagging, and verifying the
published downloads.

## Required

- [Breaking release cut](/quest/m2/release/cut.md) - prepared code reaches release before artifact publication and installed proof
