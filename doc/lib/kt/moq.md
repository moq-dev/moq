---
title: dev.moq:moq (Kotlin)
description: Kotlin Multiplatform library for Media over QUIC
---

# dev.moq:moq

The Kotlin Multiplatform module for [Media over QUIC](/).

A single Maven coordinate that publishes JVM and Android variants. Gradle metadata picks the right one for your target, so there are no per-platform artifacts to track.

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
import uniffi.moq.MoqClient

val client = MoqClient()
val cs = client.connect("https://relay.example.com")
```

`MoqClient.connect(url)` returns a `MoqSession`. The accessors `cs.publisher()` and `cs.consumer()` are always populated: by whatever origin you wired via `setPublish` / `setConsume` before connect, or by a fresh auto-created one for any side you didn't set.

For development against a relay with a self-signed certificate, configure the client before connecting:

```kotlin
val client = MoqClient()
client.setTlsDisableVerify(true)
client.setBind("127.0.0.1:0")
val cs = client.connect("https://localhost:4443")
```

When you're done, signal graceful shutdown to the peer:

```kotlin
cs.shutdown()  // alias for cancel(0u)
```

## Subscribe

```kotlin
import dev.moq.*
import kotlinx.coroutines.flow.collect

cs.consumer().announcements("demos/").collect { announcement ->
    val catalog = announcement.broadcast().subscribeCatalog()
    catalog.updates().collect { update ->
        println("catalog: $update")
    }
}
```

## Publish

```kotlin
import dev.moq.*
import uniffi.moq.MoqBroadcastProducer

val broadcast = MoqBroadcastProducer()
val audio = broadcast.publishMedia("opus", opusInitBytes)

cs.publisher().announce("my-stream", broadcast)

audio.writeFrame(payload, timestampUs = 0u)
audio.writeFrame(payload, timestampUs = 20_000u)
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
