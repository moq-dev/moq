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

Raw track subscribers can query the publisher's track properties and change their own delivery preferences without resubscribing:

```kotlin
val track = announcement.broadcast().subscribeTrack(
    "events",
    Subscription(priority = 10u.toUByte()),
)
val info = track.info()
track.update(Subscription(priority = 20u.toUByte(), ordered = false))
```

`ordered` controls prioritization only. When true, groups are prioritized in sequence order. Groups may always arrive out-of-order (or not at all) over the network.

## Publish

```kotlin
import dev.moq.*

Moq.connect("https://relay.example.com").use { moq ->
    val broadcast = BroadcastProducer()
    val audio = broadcast.publishMedia(MoqInit(format = "opus", data = opusInitBytes, video = null))

    moq.announce("my-stream", broadcast)

    audio.writeFrame(payload, timestampUs = 0u)
    audio.writeFrame(payload, timestampUs = 20_000u)
    audio.finish()
    broadcast.finish()
}
```

### Fetching raw groups

Fetch retrieves one group by track name and group sequence without keeping a live subscription:

```kotlin
val group = consumer.fetchGroup(
    "events",
    42uL,
    FetchGroupOptions(priority = 10u),
)
group.frames().collect { frame ->
    println("${frame.timestampUs}: ${frame.payload.decodeToString()}")
}
```

A retained group resolves immediately. To serve a group that is not retained, keep a dynamic handler alive on its producer:

```kotlin
val dynamic = track.dynamic()

dynamic.requestedGroups().collect { request ->
    val group = request.accept()
    group.writeFrame(loadArchivedFrame(request.sequence()), timestampUs = request.sequence() * 20_000uL)
    group.finish()
}
```

Call `request.abort(code)` when the requested group cannot be produced. Fetch is currently a single-group operation and is supported by the moq-lite 05+ FETCH wire path.

### On-demand raw tracks

Use a dynamic broadcast when subscribers should be able to request raw tracks that are not published yet:

```kotlin
import dev.moq.*

Moq.connect("https://relay.example.com").use { moq ->
    val broadcast = BroadcastProducer()
    val dynamic = broadcast.dynamic()

    moq.announce("events", broadcast)

    dynamic.requestedTracks().collect { request ->
        if (request.name() == "alerts") {
            val track = request.accept(null)
            track.writeFrame(payload = "ready".encodeToByteArray(), timestampUs = 20_000u)
            track.finish()
        } else {
            request.abort(404)
        }
    }
}
```

Each requested track arrives as a `TrackRequest`; call `accept(info)` to turn it into a `TrackProducer` (pass `null` for defaults), or `abort(code)` to reject the subscriber. Use `writeFrame(payload, timestampUs)` with a presentation timestamp in microseconds. Raw tracks default to a microsecond timescale. Raw consumers receive `MoqFrame` values from `readFrame()` or the `frames()` Flow extension.

### Raw datagrams

Raw tracks can send a single best-effort payload without opening a group stream:

```kotlin
val sequence = track.appendDatagram(timestampUs = 42_000u, payload = "meter update".encodeToByteArray())
val datagram = consumer.recvDatagram()

consumer.datagrams().collect { datagram ->
    println("${datagram.sequence}: ${datagram.timestampUs}")
}
```

Datagrams are delivered as `Datagram(sequence, timestampUs, payload)`. Payloads are capped at 1200 bytes. Delivery requires a datagram-capable transport and lite-05 or newer moq-lite; IETF moq-transport, pre-lite-05, WebSocket, and TCP paths do not deliver them, and there is no stream fallback.

### On-demand broadcasts

Use a dynamic origin when consumers should be able to request whole broadcasts that are not announced:

```kotlin
import dev.moq.*

val origin = OriginProducer(OriginOptions(cacheCapacityBytes = 256UL * 1024UL * 1024UL))
val dynamic = origin.dynamic()

dynamic.requestedBroadcasts().collect { request ->
    if (request.path() == "events") {
        val broadcast = BroadcastProducer()
        val track = broadcast.publishTrack("status", null)
        request.accept(broadcast)
        track.writeFrame("ready".encodeToByteArray(), timestampUs = 0u)
    } else {
        request.abort(404)
    }
}
```

The served broadcast is not announced. It only resolves consumers that call `requestBroadcast(path)`. Each request arrives as a `BroadcastRequest`; call `accept(broadcast)` to serve it, or `abort(code)` to fail the requester.

### JSON tracks

For JSON payloads, publish and subscribe with the framing handled for you, in one of two modes. Snapshot (lossy) carries one value updated over time; a subscriber only sees the latest. Stream (lossless) is an ordered append-log where every record is preserved. Values cross as JSON strings; serialize with your JSON library of choice.

```kotlin
import dev.moq.*
import uniffi.moq.MoqBroadcastProducer
import uniffi.moq.MoqJsonConfig
import uniffi.moq.MoqJsonStreamConfig

// Snapshot: each update supersedes the last.
val config = MoqJsonConfig(deltaRatio = 8u, compression = true)
val status = broadcast.publishJson("status", config)
status.update("""{"state":"live"}""")

val broadcastConsumer = broadcast.consume()
val consumer = broadcastConsumer.subscribeJson("status", config)
consumer.values().collect { value -> println(value) }

// Stream: every record is delivered in order.
val events = broadcast.publishJsonStream("events", MoqJsonStreamConfig(compression = false))
events.append("""{"event":"started"}""")
```

`compression` must match on the producer and subscriber. In snapshot mode, `deltaRatio` of `0` disables merge-patch deltas (every change is a fresh snapshot).

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
