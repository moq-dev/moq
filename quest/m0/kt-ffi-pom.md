# [XS] Release the Kotlin wrapper POM fix as 0.5.2

## Goal

Cherry-pick #5219 onto `release` with `moq.version=0.5.2` in
`kt/gradle.properties`, so `release-kt-lib.yml` publishes `dev.moq:moq` 0.5.2.
Done when the `moq-jvm` and `moq-android` 0.5.2 POMs on Maven Central name
`moq-ffi-jvm` and `moq-ffi-android` at `[0.4.3,0.5)`.

## Plan

Settled in #5219, which landed the fix on `main`:

- `moq-jvm` 0.5.0/0.5.1 and `moq-android` 0.5.1 depend on the never-published
  `0.0.0-dev` bindings; Central is immutable, so they stay broken.
- The wrapper keeps compiling against the sibling `:moq-ffi` project.
  `release-kt-lib.yml` publishes only when `rs/moq-ffi` matches a released
  `moq-ffi-v*` tag, so those local bindings are the published ones.
- A `GenerateMavenPom` `doLast` restores `MOQ_FFI_RANGE` in the target POMs,
  and `just kt check-poms` refuses `0.0.0-dev` in `just kt check` and the
  publication dry-run.
- `release` builds against `moq-ffi` 0.4.15, which is on Maven Central
  (checked 2026-10-15). Bumping on `main` cannot publish while `main` carries
  unreleased FFI changes.

Verify: fetch
`https://repo1.maven.org/maven2/dev/moq/moq-jvm/0.5.2/moq-jvm-0.5.2.pom` and the
`moq-android` equivalent and check the bindings dependency version.

Public API: none. Wire: none.
