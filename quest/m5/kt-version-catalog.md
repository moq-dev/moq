# [XS] Kotlin dependencies live in a Gradle version catalog

## Goal

`kt/` declares its library and plugin versions once, in a Gradle version
catalog (`kt/gradle/libs.versions.toml`), with one entry per library. A
Dependabot bump of one `org.jetbrains.kotlinx` library then can't rewrite
another that happens to share its version string. Done when no
`kt/**/*.gradle.kts` file spells a dependency version inline, and
`just kt check` passes.

## Plan

Planned 2026-11-03 (maintainer, `/quest-plan`), from the follow-ups of
[moq-dev/moq#7562](https://github.com/moq-dev/moq/pull/7562). Dependabot's
Gradle updater rewrites every `org.jetbrains.kotlinx` line in a build script
that shares the old version string, so a serialization bump can drag
coroutines along once both sit on the same version.
`kt/moq/build.gradle.kts` and `kt/moq-ffi/build.gradle.kts` spell
`kotlinx-coroutines-*` and `kotlinx-serialization-json` versions inline.

- Move the libraries into `[versions]` and `[libraries]`, with a separate
  version key per library family (coroutines, serialization). Move the
  plugin versions into `[plugins]` too, requested with `alias(...)`: Kotlin,
  maven-publish and Dokka in `kt/build.gradle.kts`, and the Android plugin
  pinned in `kt/settings.gradle.kts`'s `pluginManagement`.
- Keep `kt/`'s Dependabot entry (`directory: "/kt"`, group `gradle`).
  Dependabot reads `gradle/libs.versions.toml` under that directory.
- Leave `moqffi.version` and `moq.version` in `gradle.properties`: they are
  this repository's own published versions, not dependencies.

Public API: none. Wire: none.
