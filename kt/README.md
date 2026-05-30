# Moq (Kotlin)

Ergonomic Kotlin wrappers around the [moq-ffi](../rs/moq-ffi) UniFFI bindings for [Media over QUIC](https://datatracker.ietf.org/doc/draft-lcurley-moq-lite/).

Single Kotlin Multiplatform module. Publishes `dev.moq:moq` with both JVM and Android variants under one coordinate. Consumers add `dev.moq:moq:VERSION` and Gradle metadata resolution picks the right artifact for their target.

## Install

Once Maven Central publishing is enabled (see below):

```kotlin
// build.gradle.kts
dependencies {
    implementation("dev.moq:moq:0.2.0")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-core:1.9.0")
}
```

## Quick start

```kotlin
import dev.moq.*
import kotlinx.coroutines.flow.collect
import uniffi.moq.MoqOriginProducer

val session = Moq.connect("https://relay.example.com")

MoqOriginProducer().use { origin ->
    val consumer = origin.consume()
    consumer.announcements("demos/").collect { announcement ->
        println("got broadcast ${announcement.path()}")

        announcement.broadcast().subscribeCatalog().updates().collect { catalog ->
            println("catalog: $catalog")
        }
    }
}
```

Cancelling the surrounding coroutine scope propagates through to the native consumer's `cancel()` via the wrapper's `onCompletion` hook.

## Local development

`kt/scripts/check.sh` builds `moq-ffi` for the host, regenerates the UniFFI Kotlin bindings, drops the host cdylib into the JNA-resource layout, and runs `gradle :moq:jvmTest`. Run via `just check-ffi`. Skips cleanly without a JDK or `cargo`.

The Android target is opt-in via `-Pandroid.enabled=true`. Without the flag the JVM variant builds without an Android SDK, though Gradle still resolves the AGP plugin marker against `google()` at sync time. CI sets the flag automatically when packaging.

## Layout

```
kt/
  build.gradle.kts          Root config (group, version)
  settings.gradle.kts       include(":moq"), pins AGP version
  gradle.properties         Defaults: version, android.useAndroidX, etc.
  moq/
    build.gradle.kts        KMP plugin, jvm() always, androidTarget() guarded by -Pandroid.enabled
    src/
      commonMain/           (reserved for future K/N targets)
      jvmAndAndroidMain/
        kotlin/dev/moq/     Wrapper sources (Moq.kt, Flows.kt, Errors.kt)
        kotlin/uniffi/      UniFFI-generated kotlin (populated, gitignored)
      jvmMain/resources/    Native libs at JNA paths (populated, gitignored)
      androidMain/jniLibs/  JNI .so per ABI (populated, gitignored)
      jvmAndAndroidTest/    Smoke tests (run as :moq:jvmTest or androidUnitTest)
  scripts/                  check.sh, package.sh, publish.sh
```

The Kotlin module stays as a single `moq-ffi` artifact because uniffi-linked libraries can't be split across separately packaged wheels/artifacts. The Python wheel (`moq-rs`) follows the same umbrella shape for the same reason.

## Publishing to Maven Central

The `release-kt.yml` workflow uses [`com.vanniktech.maven.publish`](https://vanniktech.github.io/gradle-maven-publish-plugin/) to upload artifacts to the [Sonatype Central Portal](https://central.sonatype.com) and trigger the release automatically on every `moq-ffi-v*` tag. Required setup (one-time):

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

The publish task is `:moq:publishAndReleaseToMavenCentral`; CI wires the four secrets as `ORG_GRADLE_PROJECT_{mavenCentralUsername,mavenCentralPassword,signingInMemoryKey,signingInMemoryKeyPassword}` so the plugin picks them up automatically.
