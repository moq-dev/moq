---
title: dev.moq:moq (Kotlin)
description: Kotlin Multiplatform library for Media over QUIC
---

# dev.moq:moq

The Kotlin Multiplatform module for [Media over QUIC](/).

A single Maven coordinate that publishes JVM and Android variants. Gradle metadata picks the right one for your target — no per-platform artifacts to track.

## Install

```kotlin
// build.gradle.kts
dependencies {
    implementation("dev.moq:moq:0.2.0")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-core:1.9.0")
}
```

Native binaries are bundled for:

- Android: arm64-v8a, armeabi-v7a, x86_64
- JVM: Linux x86_64 + aarch64, macOS x86_64 + aarch64, Windows x86_64

Android uses JNI (`jniLibs/`), desktop JVM uses JNA (resource-classpath layout). Both are bundled in the same AAR/JAR.

## Connect

```kotlin
import dev.moq.Moq

val session = Moq.connect("https://relay.example.com")
```

For development against a relay using a self-signed certificate, pass `tlsVerify = false`.

## Subscribe

```kotlin
import dev.moq.*
import kotlinx.coroutines.flow.collect
import uniffi.moq.MoqOriginProducer

MoqOriginProducer().use { origin ->
    val consumer = origin.consume()
    val announced = consumer.announced("demos/")

    announced.announcements().collect { announcement ->
        val catalog = announcement.broadcast().subscribeCatalog()
        catalog.updates().collect { update ->
            println("catalog: $update")
        }
    }
}
```

## Publish

```kotlin
import dev.moq.*
import uniffi.moq.MoqBroadcastProducer

val broadcast = MoqBroadcastProducer()
val audio = broadcast.publishMedia("opus", opusInitBytes)

session.publish("my-stream", broadcast)

audio.writeFrame(payload, timestampUs = 0)
audio.writeFrame(payload, timestampUs = 20_000)
audio.finish()
broadcast.finish()
```

## Cancellation

The wrapper exposes consumers as Kotlin `Flow`s. Cancelling the collector's coroutine scope calls `cancel()` on the native side via the wrapper's `onCompletion` hook, releasing resources promptly:

```kotlin
val job = launch {
    mediaConsumer.frames().collect { frame ->
        process(frame)
    }
}

// Later:
job.cancel()  // releases native resources
```

## Local development

To build and run the JVM tests locally:

```bash
just check-ffi
```

This builds `moq-ffi` for the host arch, regenerates the UniFFI Kotlin bindings, drops the host cdylib into the JNA resource layout, and runs `gradle :moq:jvmTest`.

Android targets are opt-in via `-Pandroid.enabled=true`. Local builds without the Android SDK still produce a working JVM variant.

## See also

- Source: [kt/](https://github.com/moq-dev/moq/tree/main/kt)
- README: [kt/README.md](https://github.com/moq-dev/moq/blob/main/kt/README.md)
- Maven Central: [dev.moq:moq](https://central.sonatype.com/artifact/dev.moq/moq)
- The Rust crate this wraps: [moq-net](/lib/rs/crate/moq-net)
