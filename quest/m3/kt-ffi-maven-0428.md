# [S] Kotlin moq-ffi 0.4.28 missing from Maven Central

## Goal

`dev.moq:moq-ffi` publishes to Maven Central for every `moq-ffi-v*` tag.
`moq-ffi-v0.4.28` (2026-11-23) did not: its Release FFI run's
`Release Kotlin FFI / Package and publish` job
([45138049265](https://github.com/moq-dev/moq/actions/runs/45138049265))
failed with `Failed to stop service 'maven-central-build-service'`, and
`https://repo1.maven.org/maven2/dev/moq/moq-ffi/maven-metadata.xml` still
lists 0.4.27 as the latest release on 2026-11-28.

## Plan

Find why the Maven Central build service failed to stop (a failed upload or
deployment it reports on shutdown, or a Gradle teardown bug), fix that at its
root on `main` and `release`, then publish 0.4.28. Check whether the upload
reached Maven Central's deployment portal before re-running anything: a
version published twice is refused.
