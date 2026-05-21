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
    val announced = consumer.announced("demos/")
    announced.announcements().collect { announcement ->
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

The Android target is opt-in via `-Pandroid.enabled=true`. Local dev without the Android SDK (or without Google maven access) still builds the JVM variant fine. CI sets the flag automatically when packaging.

## Layout

```
kt/
  build.gradle.kts          Root config (group, version)
  settings.gradle.kts       include(":moq"), pins AGP version
  gradle.properties         Defaults: version, android.useAndroidX, etc.
  moq/
    build.gradle.kts        KMP plugin, jvm() always, androidTarget() conditional
    android.gradle.kts      Applied only when -Pandroid.enabled=true
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

When `moq-ffi` splits into `moq-mux-ffi` + `moq-net-ffi`, add sibling modules `kt/moq-mux/` and `kt/moq-net/` with identical structure.

## Publishing to Maven Central

Wired but disabled. To enable:

1. Register at https://central.sonatype.com and claim the `dev.moq` namespace (requires a TXT record at `moq.dev` with the verification key). The auto-verified alternative `io.github.moq-dev` works without DNS setup but changes artifact coordinates.
2. Account menu -> Generate User Token. Save the username/password (one-time view).
3. Create a GPG signing key:
   ```
   gpg --batch --generate-key gpg.conf
   gpg --armor --export-secret-keys $KEYID > signing-key.asc
   gpg --keyserver keys.openpgp.org --send-keys $KEYID
   gpg --keyserver keyserver.ubuntu.com --send-keys $KEYID
   ```
4. In `moq-dev/moq` -> Settings -> Secrets and variables -> Actions:
   - Secret `MAVEN_CENTRAL_USERNAME`
   - Secret `MAVEN_CENTRAL_PASSWORD`
   - Secret `SIGNING_KEY` (contents of `signing-key.asc`)
   - Secret `SIGNING_PASSWORD`
   - Variable `PUBLISH_MAVEN=true`
5. Cut the next `moq-ffi-v*` tag. The `kotlin.yml` workflow's `publish` job calls `kt/scripts/publish.sh`, which uploads to the Central Portal.
