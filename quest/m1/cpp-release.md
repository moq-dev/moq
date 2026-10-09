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

Before the tag, give the OBS release path a nightly dry run. `obs-build` in
`release-cpp.yml` needs the tag-only `release` job, so the nightly
`workflow_call` builds the C++ archives but never runs
`just obs package --moq-release` against them. Add a local-archive override
to `cpp/obs/CMakeLists.txt` beside the release-download branch, so the dry run
links the build job's `moq-cpp-<version>-<target>` artifacts instead of a
published release. Keep publication tag-only. Without it, the first tag is the
first run of that path.

## Required

- [Client settings parity](/quest/m1/obs-client-config.md) - the OBS Advanced settings the migration dropped come back
- [Session report parity](/quest/m1/obs-session-report.md) - the dock shows the negotiated protocol and the reconnect failure again
- [Generated C++ shape](/quest/m1/cpp-generated-shape.md) - each type has one name and `moq::expected` one type before the API ships
