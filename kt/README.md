# Moq (Kotlin)

Ergonomic Kotlin wrappers around the [moq-ffi](../rs/moq-ffi) UniFFI bindings for [Media over QUIC](https://datatracker.ietf.org/doc/draft-lcurley-moq-lite/).

Two artifacts ship from this directory:

- `dev.moq:moq-jvm` for desktop and server JVM applications.
- `dev.moq:moq-android` for Android applications.

Both share their wrapper source from [`common/src/`](common/src) and depend on the same set of UniFFI-generated Kotlin code populated at build time.

## Install

Once Maven Central publishing is enabled (see below), consumers add:

```kotlin
// build.gradle.kts
dependencies {
    implementation("dev.moq:moq-android:0.2.0")    // or moq-jvm
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-core:1.9.0")
}
```

Until then, every `moq-ffi-v*` GitHub Release attaches a `moq-ffi-${VERSION}-kotlin.tar.gz` archive containing a Maven-local layout that can be added with `repositories { maven { url = uri("./path/to/maven-local") } }`.

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

`kt/scripts/check.sh` builds `moq-ffi` for the host, regenerates the UniFFI Kotlin bindings, drops the host cdylib into the JNA-resource layout, and runs `gradle :moq-jvm:test`. It is invoked automatically by `just check-ffi`. The Android variant is built only on machines that have `ANDROID_HOME` or a `local.properties` with `sdk.dir=`.

## Layout

```
kt/
  build.gradle.kts        Root config (group, version)
  settings.gradle.kts     Conditionally includes :moq-android when ANDROID_HOME is set
  gradle.properties       Default version + publishing toggle
  common/
    src/
      dev/moq/            Wrapper sources (Moq.kt, Flows.kt, Errors.kt)
      uniffi/             UniFFI-generated kotlin (populated, gitignored)
  moq-jvm/
    build.gradle.kts      Kotlin/JVM library
    src/main/resources/   Native libs at JNA paths (populated, gitignored)
    src/test/             Smoke tests
  moq-android/
    build.gradle.kts      Android library
    src/main/jniLibs/     JNI .so per ABI (populated, gitignored)
  scripts/                check.sh, package.sh, publish.sh
```

## Publishing to Maven Central

The pipeline is wired but disabled. To enable:

1. Pick a namespace. `io.github.moq-dev` is auto-verified via the GitHub org; `dev.moq` requires DNS TXT verification at `_sonatype-central-verification.moq.dev`.
2. Register at https://central.sonatype.com/ and claim the namespace.
3. Generate a portal user token: account menu -> Generate User Token. Save the username/password (one-time view).
4. Create a GPG signing key:
   ```
   gpg --batch --generate-key gpg.conf      # 4096 bit RSA, expiry 4y
   gpg --armor --export-secret-keys $KEYID > signing-key.asc
   gpg --keyserver keys.openpgp.org --send-keys $KEYID
   gpg --keyserver keyserver.ubuntu.com --send-keys $KEYID
   ```
5. In `moq-dev/moq` repo settings -> Secrets and variables -> Actions:
   - Secret `MAVEN_CENTRAL_USERNAME`
   - Secret `MAVEN_CENTRAL_PASSWORD`
   - Secret `SIGNING_KEY` (contents of `signing-key.asc`)
   - Secret `SIGNING_PASSWORD` (passphrase used at keygen)
   - Variable `PUBLISH_MAVEN=true`
6. Cut the next `moq-ffi-v*` tag. The `publish-maven` job runs `kt/scripts/publish.sh`, which uploads the bundle to the Central Portal.
