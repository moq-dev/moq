# [S] The dev shell builds Kotlin with Gradle 9

## Goal

The dev shell's Kotlin toolchain (`ktDeps` in `flake.nix`) ships `gradle_9`
instead of `gradle_8`, so the Android target can be built from the dev shell.
Done when `just kt check` passes on the dev shell's Gradle 9, and
`gradle -Pandroid.enabled=true :moq-ffi:assembleRelease :moq:assembleRelease`
builds both AARs from the dev shell on a host with an Android SDK.

## Plan

Planned 2026-11-03 (maintainer, `/quest-plan`), from the follow-ups of
[moq-dev/moq#7562](https://github.com/moq-dev/moq/pull/7562). Gradle 8
can't configure the Android target on any AGP 9.x, including the 9.4.1 that
main had before #7562. AGP 9.5.0 needs Gradle 9.6.0 or later. #7562 built
the AARs with Gradle 9.8.0 outside the dev shell. The release runner's
Gradle 9.8.2 already publishes them.

The `flake.nix` comment pins Gradle 8 because "Kotlin 2.0.21's Gradle plugin
predates Gradle 9", but `kt/build.gradle.kts` is on Kotlin 2.4.21, so that
reason no longer holds. The pinned nixpkgs has `gradle_9` at 9.8.0.

- Swap `gradle_8` for `gradle_9` in `ktDeps` and rewrite the comment with the
  current constraint (AGP 9.x needs Gradle 9).
- Run `just kt check` on Linux and macOS (kixtop). Fix any Gradle 9
  deprecation that is now an error.
- The dev shell has no Android SDK, so the Android build still needs
  `ANDROID_HOME`. Adding an SDK to the default shell is out of scope. Ask
  the maintainer before adding one.
- Check that `.github/workflows/android.yml` and the release workflows still
  agree with the dev shell's Gradle major.

Public API: none. Wire: none.
