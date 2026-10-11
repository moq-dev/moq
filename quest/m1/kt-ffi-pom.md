# [S] Kotlin wrapper POMs name a published moq-ffi

## Goal

The `dev.moq:moq-jvm` and `dev.moq:moq-android` POMs on Maven Central depend
on a published `moq-ffi` variant, so a Maven build resolves the wrapper.
Today 0.5.0 and 0.5.1 depend on `moq-ffi-jvm` and `moq-ffi-android` version
`0.0.0-dev`, which was never published, so every Maven consumer fails to
resolve. A PR check refuses a generated POM naming that version.

## Plan

Checked against Maven Central on 2026-10-08:

- `moq-jvm` 0.5.0 and 0.5.1 depend on `moq-ffi-jvm:0.0.0-dev`, and
  `moq-android` 0.5.1 on `moq-ffi-android:0.0.0-dev`. `moq-jvm` 0.4.5 depended
  on `moq-ffi` `[0.3,0.4)`.
- Gradle consumers resolve: the `.module` metadata and the root `moq` POM
  keep `MOQ_FFI_RANGE` (`[0.4.3,0.5)`).

Cause: `kt/moq/build.gradle.kts` substitutes `dev.moq:moq-ffi` with the
sibling `:moq-ffi` project in every configuration, and the target POMs take
that project's coordinates and version, `moqffi.version=0.0.0-dev` in
`kt/gradle.properties`. Its comment that the published POM keeps the range
holds only for the root POM. The 0.4 line had the same substitution, so a
Kotlin or Gradle upgrade since changed the POM rewrite.

Options:

- Keep the substitution out of the publication's configurations, so target
  POMs carry `MOQ_FFI_RANGE` on the target artifacts (recommended: Maven
  consumers float to new bindings patches as Gradle consumers do).
- Publish with `moqffi.version` set to the current FFI release, which pins
  Maven consumers to one bindings patch.

Test: the `release-kt-lib.yml` PR dry-run inspects the POMs that
`publishToMavenLocal` writes and fails when a `dev.moq` dependency names
`0.0.0-dev`. Ship it as a wrapper patch: that workflow publishes only when
`moq.version` changes. Maven Central is immutable, so 0.5.0 and 0.5.1 stay
broken.

Public API: none. Wire: none.

### Publication blocker

PR #5219 removes publication-time project substitution and validates generated
POMs, but publication is not complete. Maven Central's latest bindings are
`0.4.12` (checked 2026-10-10). Compiling the wrapper against that artifact
fails because it lacks current client configuration, announcement, group demand,
and media APIs. Generating valid POMs alone does not prove a usable publication.

Recommended: publish matching bindings through the normal release process, set
the wrapper's supported bindings range to the actual compatible release, then
validate the JVM and Android publication dry-run before publishing a wrapper
patch. Alternatively, backport the POM correction onto a compatible release
wrapper. Do not compile against unpublished local bindings while claiming that
an older Maven artifact supplies those APIs. Existing broken Central artifacts
are immutable; this work remains open until a usable replacement ships.
