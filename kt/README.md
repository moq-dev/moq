# Moq (Kotlin)

Ergonomic Kotlin wrappers around the [moq-ffi](../rs/moq-ffi) UniFFI bindings for [Media over QUIC](https://datatracker.ietf.org/doc/draft-lcurley-moq-lite/).

Two Kotlin Multiplatform artifacts (JVM + Android variants under each coordinate):

- **`dev.moq:moq-ffi`**: the raw UniFFI bindings (`uniffi.moq.*`) plus the native libs. Auto-released on every `moq-ffi-v*` tag, so its version tracks the `moq-ffi` crate.
- **`dev.moq:moq`**: the ergonomic wrapper layered on top. Versioned independently and published only when its version changes. It depends on `moq-ffi` via a floating range, so adding the wrapper transitively pulls the newest bindings patch.

Most apps want `dev.moq:moq`. Reach for `dev.moq:moq-ffi` directly only if you want the raw FFI surface without the wrapper.

## Install

```kotlin
// build.gradle.kts
dependencies {
    implementation("dev.moq:moq:0.3.0")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-core:1.9.0")
}
```

The wrapper's POM declares `dev.moq:moq-ffi:[0.2,0.3)`, so Gradle resolves the latest `0.2.x` bindings automatically. Pin `dev.moq:moq-ffi` yourself if you need a reproducible bindings version.

## Quick start

```kotlin
import dev.moq.*
import kotlinx.coroutines.flow.collect

// connect() wires up an internal origin and returns a live connection.
Moq.connect("https://relay.example.com").use { moq ->
    moq.announcements("demos/").collect { announcement ->
        println("got broadcast ${announcement.path()}")

        val catalog = announcement.broadcast().catalog()
        println("catalog: $catalog")
    }
}
```

`Moq.connect` builds the `MoqClient`, applies TLS / bind options, wires the publish + subscribe origins, and hands back a `Moq` you can `use {}`. Cancelling the surrounding coroutine scope propagates through the Flow extensions to the native consumer's `cancel()` via their `onCompletion` hook.

### What the wrapper adds

The `dev.moq` package is intentionally thin: Kotlin has extension functions, so we keep the FFI objects and decorate them rather than re-wrapping every type.

- **`Moq.connect(...)`**: a connection facade (`Moq.kt`), so you never hand-wire a `MoqClient`.
- **Typealiases** (`Aliases.kt`): re-export the `Moq*`-prefixed FFI types under clean `dev.moq` names (`OriginProducer`, `BroadcastConsumer`, `Catalog`, `Frame`, ...), so you import `dev.moq.*` only. A couple of sealed types (`Container`, `MoqException`) are not aliased because Kotlin can't resolve their subtypes through a typealias; use `uniffi.moq.*` for those.
- **Flow extensions** (`Flows.kt`): `updates()`, `groups()`, `frames()`, `announcements()`, `catalog()` turn the pull-based consumers into coroutine `Flow`s with cancellation wired through.
- **`MoqException.isShutdown`** (`Errors.kt`): true for the graceful `Cancelled`/`Closed` cases.

## Versioning

- `moqffi.version` (gradle.properties): the bindings version. CI overrides it from the `moq-ffi-v*` tag; only used for local dev otherwise.
- `moq.version` (gradle.properties): the wrapper version, the source of truth. **Bump this by hand** to ship a new wrapper. `release-kt-wrapper.yml` reads it, checks whether `dev.moq:moq:<version>` is already on Maven Central, and publishes only if it isn't. Must stay `>= 0.3.0` (the line continues from the pre-split `dev.moq:moq` releases).

## Local development

`kt/scripts/check.sh` builds `moq-ffi` for the host, regenerates the UniFFI Kotlin bindings, drops the host cdylib into the `:moq-ffi` JNA-resource layout, and runs `gradle :moq-ffi:jvmTest :moq:jvmTest`. Run via `just kt check`. Skips cleanly without a JDK or `cargo`.

The wrapper resolves `moq-ffi` from the sibling project (a Gradle `dependencySubstitution` in `kt/moq/build.gradle.kts`), so tests run against freshly-built bindings; the published metadata still carries the floating range.

The Android target is opt-in via `-Pandroid.enabled=true`. Without the flag the JVM variant builds without an Android SDK, though Gradle still resolves the AGP plugin marker against `google()` at sync time. CI sets the flag automatically when packaging.

## Layout

```
kt/
  build.gradle.kts          Root config (group only; version is per-module)
  settings.gradle.kts       include(":moq-ffi", ":moq"), pins AGP version
  gradle.properties         Defaults: moqffi.version, moq.version, ...
  moq-ffi/
    build.gradle.kts        Bindings + native libs; coordinates dev.moq:moq-ffi
    src/
      jvmAndAndroidMain/kotlin/uniffi/  UniFFI-generated kotlin (populated, gitignored)
      jvmMain/resources/                Native libs at JNA paths (populated, gitignored)
      androidMain/jniLibs/              JNI .so per ABI (populated, gitignored)
      jvmAndAndroidTest/                Binding smoke test
  moq/
    build.gradle.kts        Pure-Kotlin wrapper; coordinates dev.moq:moq
    src/
      jvmAndAndroidMain/kotlin/dev/moq/ Wrapper sources (Moq, Aliases, Flows, Errors)
      jvmAndAndroidTest/                Facade smoke test
  scripts/                  check.sh, package.sh
```

The native/UniFFI layer stays in a single `dev.moq:moq-ffi` artifact because uniffi-linked libraries can't be split across separately packaged artifacts (the Python `moq-rs` wheel is one umbrella for the same reason). That constraint is about the *native* layer; the pure-Kotlin `dev.moq:moq` wrapper sits cleanly on top of it as its own artifact because it ships no native code.

## Publishing to Maven Central

Both `release-kt.yml` (bindings) and `release-kt-wrapper.yml` (wrapper) use [`com.vanniktech.maven.publish`](https://vanniktech.github.io/gradle-maven-publish-plugin/) to upload to the [Sonatype Central Portal](https://central.sonatype.com) and trigger the release automatically. Required setup (one-time):

1. Register at https://central.sonatype.com and claim the `dev.moq` namespace (TXT record on `moq.dev` with the verification key). The auto-verified alternative `io.github.moq-dev` works without DNS setup but changes artifact coordinates.
2. Account menu -> Generate User Token. Save the username/password (one-time view).
3. Create a GPG signing key (the passphrase becomes `SIGNING_PASSWORD`):
   ```
   gpg --full-generate-key                       # RSA 4096, 4y expiry
   gpg --list-secret-keys --keyid-format=long    # find <KEYID>
   gpg --armor --export-secret-keys <KEYID> > signing-key.asc
   gpg --keyserver keys.openpgp.org --send-keys <KEYID>
   gpg --keyserver keyserver.ubuntu.com --send-keys <KEYID>
   ```
4. In `moq-dev/moq` -> Settings -> Secrets and variables -> Actions:
   - Secret `MAVEN_CENTRAL_USERNAME`
   - Secret `MAVEN_CENTRAL_PASSWORD`
   - Secret `SIGNING_KEY` (full contents of `signing-key.asc`, including the BEGIN/END lines)
   - Secret `SIGNING_PASSWORD`

Both workflows wire the four secrets as `ORG_GRADLE_PROJECT_{mavenCentralUsername,mavenCentralPassword,signingInMemoryKey,signingInMemoryKeyPassword}` so the plugin picks them up automatically. The publish tasks are `:moq-ffi:publishAndReleaseToMavenCentral` and `:moq:publishAndReleaseToMavenCentral`.
