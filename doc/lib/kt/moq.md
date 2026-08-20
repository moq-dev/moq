---
title: dev.moq:moq (Kotlin)
description: Kotlin Multiplatform library for Media over QUIC
---

# dev.moq:moq

The ergonomic Kotlin wrapper for [Media over QUIC](/), layered on the [`dev.moq:moq-ffi`](https://central.sonatype.com/artifact/dev.moq/moq-ffi) bindings. Both publish JVM and Android variants under one coordinate; Gradle metadata picks the right one for your target.

[![javadoc](https://javadoc.io/badge2/dev.moq/moq/javadoc.svg)](https://javadoc.io/doc/dev.moq/moq)

Full API reference: [javadoc.io/doc/dev.moq/moq](https://javadoc.io/doc/dev.moq/moq), the KDoc rendered from each release's Dokka javadoc jar.

## Install

```kotlin
// build.gradle.kts
dependencies {
    implementation("dev.moq:moq:0.4.3")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-core:1.9.0")
}
```

The wrapper depends on `dev.moq:moq-ffi:[0.3,0.4)`, so Gradle resolves the latest bindings patch automatically. The bindings carry the native binaries:

- Android: arm64-v8a, armeabi-v7a, x86\_64
- JVM: Linux x86\_64 + aarch64, macOS aarch64, Windows x86\_64

Android uses JNI (`jniLibs/`), desktop JVM uses JNA (resource-classpath layout).

## Connect

```kotlin
import dev.moq.*

val moq = Moq.connect("https://relay.example.com")
```

`Moq.connect(url)` builds the client, wires an internal origin for both publishing and subscribing, and returns a `Moq` connection. It is `AutoCloseable`, so prefer `use {}`:

```kotlin
Moq.connect(
    "https://localhost:4443",
    tlsVerify = false,
    tlsRoots = listOf("local-ca.pem"),
    tlsSystemRoots = true,
    bind = "127.0.0.1:0",
).use { moq ->
    // ... moq.session is the underlying MoqSession ...
}  // close() cancels the client + session
```

Advanced callers can pass their own `publish` / `subscribe` origins, or skip the facade entirely and drive `uniffi.moq.MoqClient` directly.

To resolve a single broadcast rather than iterate announcements:

```kotlin
// Waits for the announcement, however long that takes.
val broadcast = moq.announcedBroadcast("demos/clock").available()

// Resolves as soon as it can be served (announced or dynamic), else throws.
val broadcast = moq.requestBroadcast("demos/clock")
```

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

### Reconnecting

The session automatically redials with backoff when the transport drops (a relay
restart, a laptop waking from sleep), and broadcasts consumed through it ride out
the gap. Pass `reconnect = false` to `Moq.connect` for a one-shot dial, or a
`Backoff` to tune the pacing. `moq.session.status()` reports the current state
(`CONNECTED`, `DISCONNECTED`, `MIGRATING`) whenever it differs from the last one
you saw, rather than a queue of every edge, so a drop that reconnects before you
ask again is coalesced away. `moq.session.closed()` resolves only once the
connection stops for good.

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

A catalog rendition may name a *different* broadcast: `MoqVideo.broadcast` / `MoqAudio.broadcast` is a path relative to the broadcast the catalog came from, so a transcode output at `live/hd` can describe a track that actually lives in `live/source`. `decodeAudio` and `decodeVideo` follow it for you. `subscribeMedia`, `subscribeTrack`, `fetchGroup`, and `fetchMediaGroup` take a track name rather than a rendition, so resolve first:

```kotlin
val source = announcement.broadcast().resolve(rendition.broadcast)
val consumer = source.subscribeMedia(name, rendition.container)
```

`resolve(null)` (or an empty reference) returns the same broadcast, so it is safe to call unconditionally. It needs an origin to fetch a sibling from, so it throws on a broadcast consumed straight from a local producer. `resolve` reports a sibling that exists but has not been announced yet as unroutable rather than waiting for it, so await the referenced broadcast's announcement first if you may be racing it.

## Publish

```kotlin
import dev.moq.*

Moq.connect("https://relay.example.com").use { moq ->
    val broadcast = moq.createBroadcast("my-stream")
    val audio = broadcast.publishAudio(
        AudioInit(format = AudioFormat.OPUS, data = opusInitBytes, label = "English")
    )

    // Audio has no keyframes, so `cut` is what gives it group boundaries. Once
    // per frame is the lowest latency; a segment cadence suits HLS/DASH.
    audio.writeFrame(Frame(payload = payload))
    audio.cut()
    audio.writeFrame(Frame(payload = payload, timestampUs = 20_000u))
    audio.cut()
    audio.finish()
    broadcast.finish()
}
```

`label` is presentation metadata for a track picker and does not change the
generated transport track name. Labels do not need to be unique.
`publishContainer` takes neither a label nor a hint: a container describes each
track it publishes from its own metadata.

Each catalog `Video` has a `stalled` boolean. A true value recommends temporarily avoiding that rendition, but the track remains directly usable. Existing catalogs default it to false.

Properties that apply to every video rendition are updated together. `null` fields clear the corresponding catalog property, and rotation is normalized to the nearest clockwise quarter turn:

```kotlin
broadcast.setVideoProperties(
    VideoProperties(
        display = Dimensions(width = 1080u, height = 1920u),
        rotation = 90.0,
        flip = false,
    )
)
```

### Raw media

`publishAudio` / `publishVideo` take frames you already encoded. To hand over raw pixels or PCM instead and let the codec run inside the bindings, use `encodeVideo` / `encodeAudio`. Pixel format, resolution, and framerate are fixed at publish time, so each frame carries only its pixels and a timestamp:

```kotlin
val video = broadcast.encodeVideo(
    VideoEncoderInput(
        format = VideoPixelFormat.RGBA,
        width = 1280u,
        height = 720u,
        framerate = 30u,
    ),
    VideoEncoderOutput(
        codec = VideoCodec.H264,
        bitrate = null,
        gop = null,
        kind = autoEncoder,
    ),
)

video.write(VideoFrame(timestampUs = ptsUs, data = rgba))
video.finish()
```

`decodeVideo` and `decodeAudio` are the mirrors: they run the codec inside the bindings on the way in, so a subscriber gets pixels and PCM without linking one. Video frames arrive as tightly-packed I420 and carry the size they actually decoded to, since `resize` is only best effort:

```kotlin
val catalog = broadcast.catalog()
val (name, rendition) = catalog.video.entries.first()

val video = broadcast.decodeVideo(name, rendition, VideoDecoderOutput())
while (true) {
    val frame = video.next() ?: break
    render(frame.data, frame.width, frame.height)
}
```

An unrecognized codec throws from `decodeVideo` itself; a recognized one no native backend handles throws when the decoder opens. Either way you find out before the first frame.

`autoEncoder` prefers a hardware encoder and falls back to software; `softwareEncoder`, `hardwareEncoder`, and `namedEncoder("videotoolbox")` pin the choice. The bindings compile VideoToolbox (macOS), Media Foundation (Windows), and openh264 (software, everywhere); the Linux hardware codecs are a libmoq-only build option. `setBitrate` retunes the live encoder without forcing a keyframe, cheap enough to drive from a congestion controller.

The track is named after the codec (`.avc3` / `.hev1`) and its catalog rendition is published immediately, read out of the encoder itself, so subscribers discover it through the catalog rather than a name you pick, and can find it before the first frame exists. `cut()` starts a new group at the next frame, which is optional: the encoder keyframes every `gop` frames on its own, and each of those cuts a group.

## Serve

`Server.listen(bind)` binds a listener, wires an internal origin for both directions, and returns an `AutoCloseable` `Server`. `serve()` accepts every session and holds it alive until it closes:

```kotlin
import dev.moq.*

Server.listen("127.0.0.1:4443", tlsGenerate = listOf("localhost")).use { server ->
    val broadcast = server.createBroadcast("live")

    server.serve()
}
```

Collect `requests()` instead when you need to inspect or reject a session before accepting it. Each `Request` must be answered with `ok()` or `close(code)`, and the returned session held to keep the connection alive:

```kotlin
Server.listen("127.0.0.1:4443", tlsGenerate = listOf("localhost")).use { server ->
    server.requests().collect { request ->
        if (request.path() == "/admin") {
            request.reject(403u)
            return@collect
        }
        launch {
            val session = request.accept()
            session.closed()
        }
    }
}
```

`request.path()` returns the query-free request path consistently across transports. The root or missing path is an empty string. `request.query()` returns the encoded query and may contain credentials.

`server.certFingerprints()` returns the hex SHA-256 fingerprints of the configured certificates, for pinning a generated self-signed certificate in a browser via `serverCertificateHashes`. Advanced callers can pass their own `publish` / `subscribe` origins to `listen`, or drive `uniffi.moq.MoqServer` directly.

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
    group.writeFrame(Frame(payload = loadArchivedFrame(request.sequence()), timestampUs = request.sequence() * 20_000uL))
    group.finish()
}
```

Call `request.abort(code)` when the requested group cannot be produced. Fetch is currently a single-group operation and is supported by the moq-lite 05+ FETCH wire path.

### Fetching media groups

`fetchGroup` hands back raw payloads. `fetchMediaGroup` decodes the same group through the rendition's advertised container, so you get timestamped frames without opening a live subscription:

```kotlin
val (name, audio) = consumer.catalog().audio.entries.first()

consumer.fetchMediaGroup(
    name,
    42uL,
    audio.container,
    FetchGroupOptions(priority = 10u),
).use { group ->
    group.frames().collect { frame ->
        println("${frame.timestampUs}: ${frame.payload.size} bytes")
    }
}
```

`frames()` is a cancellation-aware `Flow`. A fetched media group is finite: it completes after the group's last decoded frame, unlike the live `subscribeMedia` stream. Latency-based group skipping does not apply, so you always get every frame in the group.

### On-demand raw tracks

Use a dynamic broadcast when subscribers should be able to request raw tracks that are not published yet:

```kotlin
import dev.moq.*

Moq.connect("https://relay.example.com").use { moq ->
    val broadcast = moq.createBroadcast("events")
    val dynamic = broadcast.dynamic()

    dynamic.requestedTracks().collect { request ->
        if (request.name() == "alerts") {
            val track = request.accept(null)
            track.writeFrame(Frame(payload = "ready".encodeToByteArray(), timestampUs = 20_000u))
            track.finish()
        } else {
            request.abort(404u)
        }
    }
}
```

Each requested track arrives as a `TrackRequest`; call `accept(info)` to turn it into a `TrackProducer` (pass `null` for defaults), or `abort(code)` to reject the subscriber. Use `writeFrame(Frame(payload, timestampUs))` with a presentation timestamp in microseconds. Raw tracks default to a microsecond timescale. Raw consumers receive `Frame` values (payload plus timestamp) from `readFrame()` or the `frames()` Flow extension; media subscriptions yield `MediaFrame`, which adds the codec-derived `keyframe` flag.

### Raw datagrams

Raw tracks can send a single best-effort payload without opening a group stream:

```kotlin
val sequence = track.appendDatagram(Frame(payload = "meter update".encodeToByteArray(), timestampUs = 42_000u))
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
        track.writeFrame(Frame(payload = "ready".encodeToByteArray()))
    } else {
        request.abort(404u)
    }
}
```

The served broadcast is not announced. It only resolves consumers that call `requestBroadcast(path)`. Each request arrives as a `BroadcastRequest`; call `accept(broadcast)` to serve it, or `abort(code)` to fail the requester.

### JSON tracks

For JSON payloads, publish and subscribe with the framing handled for you, in one of two modes. Snapshot (lossy) carries one value updated over time; a subscriber only sees the latest. Stream (lossless) is an ordered append-log where every record is preserved.

Pass a `@Serializable` type and the wrapper encodes and decodes it with `kotlinx.serialization`:

```kotlin
import dev.moq.*
import kotlinx.serialization.Serializable

@Serializable
data class Status(val state: String)

// Snapshot: each update supersedes the last.
val config = JsonSnapshotConfig(deltaRatio = 8u, compression = true)
val status = broadcast.publishJsonSnapshot("status", config)
status.update(Status(state = "live"))

val consumer = broadcast.consume().subscribeJsonSnapshot("status", config)
consumer.valuesAs<Status>().collect { value -> println(value.state) }

// Stream: every record is delivered in order.
val events = broadcast.publishJsonStream("events", JsonStreamConfig(compression = false))
events.append(Status(state = "started"))
```

The raw string form stays available for other JSON libraries: `update("""{"state":"live"}""")` passes the payload straight through, and `values()` yields the undecoded strings. The same split applies to `setCatalogSection(name, value)`, which encodes a `@Serializable` value, or forwards a `String` unchanged.

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

This builds `moq-ffi` for the host arch, regenerates the UniFFI Kotlin bindings, drops the host cdylib into the `:moq-ffi` JNA resource layout, and runs `gradle :moq-ffi:jvmTest :moq:jvmTest`. The wrapper resolves `:moq-ffi` from the sibling project, so it builds against the freshly generated bindings. It needs `cargo`, a JDK, and Gradle, all provided by the `nix develop` shell. To regenerate the checked-in bindings without compiling or testing, use `just kt generate`.

Android targets are opt-in via `-Pandroid.enabled=true`. Local builds without the Android SDK still produce a working JVM variant.

## See also

- API reference: [javadoc.io/doc/dev.moq/moq](https://javadoc.io/doc/dev.moq/moq)
- Source: [kt/](https://github.com/moq-dev/moq/tree/main/kt)
- README: [kt/README.md](https://github.com/moq-dev/moq/blob/main/kt/README.md)
- Maven Central: [dev.moq:moq](https://central.sonatype.com/artifact/dev.moq/moq)
- The Rust crate this wraps: [moq-net](/lib/rs/crate/moq-net)
