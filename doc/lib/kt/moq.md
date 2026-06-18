---
title: dev.moq:moq (Kotlin)
description: Kotlin Multiplatform library for Media over QUIC
---

# dev.moq:moq

The ergonomic Kotlin wrapper for [Media over QUIC](/), layered on the [`dev.moq:moq-ffi`](https://central.sonatype.com/artifact/dev.moq/moq-ffi) bindings. Both publish JVM and Android variants under one coordinate; Gradle metadata picks the right one for your target.

## Install

```kotlin
// build.gradle.kts
dependencies {
    implementation("dev.moq:moq:0.3.0")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-core:1.9.0")
}
```

The wrapper depends on `dev.moq:moq-ffi:[0.2,0.3)`, so Gradle resolves the latest bindings patch automatically. The bindings carry the native binaries:

- Android: arm64-v8a, armeabi-v7a, x86\_64
- JVM: Linux x86\_64 + aarch64, macOS x86\_64 + aarch64, Windows x86\_64

Android uses JNI (`jniLibs/`), desktop JVM uses JNA (resource-classpath layout).

## Connect

```kotlin
import dev.moq.*

val moq = Moq.connect("https://relay.example.com")
```

`Moq.connect(url)` builds the client, wires an internal origin for both publishing and subscribing, and returns a `Moq` connection. It is `AutoCloseable`, so prefer `use {}`:

```kotlin
Moq.connect("https://localhost:4443", tlsVerify = false, bind = "127.0.0.1:0").use { moq ->
    // ... moq.session is the underlying MoqSession ...
}  // close() cancels the client + session
```

Advanced callers can pass their own `publish` / `subscribe` origins, or skip the facade entirely and drive `uniffi.moq.MoqClient` directly.

A server can reject the connection on auth grounds: `MoqException.Unauthorized` (HTTP 401) or `MoqException.Forbidden` (HTTP 403). These are terminal: retrying without new credentials won't help, so handle them separately from a transient transport failure. Use the `isAuth` helper to catch both:

```kotlin
import dev.moq.isAuth

try {
    val session = client.connect("https://relay.example.com")
} catch (e: MoqException) {
    if (e.isAuth) {
        // Prompt for credentials; don't reconnect.
    }
}
```

## Subscribe

```kotlin
import dev.moq.*
import kotlinx.coroutines.flow.collect

Moq.connect("https://relay.example.com").use { moq ->
    moq.announcements("demos/").collect { announcement ->
        // Convenience: subscribe and grab the current catalog.
        val catalog = announcement.broadcast().catalog()
        println("catalog: $catalog")
    }
}
```

## Publish

```kotlin
import dev.moq.*

Moq.connect("https://relay.example.com").use { moq ->
    val broadcast = BroadcastProducer()
    val audio = broadcast.publishMedia("opus", opusInitBytes)

    moq.publish("my-stream", broadcast)

    audio.writeFrame(payload, timestampUs = 0u)
    audio.writeFrame(payload, timestampUs = 20_000u)
    audio.finish()
    broadcast.finish()
}
```

### On-demand raw tracks

Use a dynamic broadcast when subscribers should be able to request raw tracks that are not published yet:

```kotlin
import dev.moq.*
import uniffi.moq.MoqBroadcastProducer

val broadcast = MoqBroadcastProducer()
val dynamic = broadcast.dynamic()

origin.publish("events", broadcast)

dynamic.requestedTracks().collect { track ->
    if (track.name() == "alerts") {
        track.writeFrame("ready".encodeToByteArray())
        track.finish()
    } else {
        track.abort(404)
    }
}
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
just kt check
```

This builds `moq-ffi` for the host arch, regenerates the UniFFI Kotlin bindings, drops the host cdylib into the `:moq-ffi` JNA resource layout, and runs `gradle :moq-ffi:jvmTest :moq:jvmTest`. The wrapper resolves `:moq-ffi` from the sibling project, so it builds against the freshly generated bindings.

Android targets are opt-in via `-Pandroid.enabled=true`. Local builds without the Android SDK still produce a working JVM variant.

## See also

- Source: [kt/](https://github.com/moq-dev/moq/tree/main/kt)
- README: [kt/README.md](https://github.com/moq-dev/moq/blob/main/kt/README.md)
- Maven Central: [dev.moq:moq](https://central.sonatype.com/artifact/dev.moq/moq)
- The Rust crate this wraps: [moq-net](/lib/rs/crate/moq-net)
