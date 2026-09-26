---
title: Kotlin
description: Coroutines and Flow for Android and the JVM via dev.moq:moq
---

# Kotlin

[![Maven Central](https://img.shields.io/maven-central/v/dev.moq/moq)](https://central.sonatype.com/artifact/dev.moq/moq)

`dev.moq:moq` on Maven Central: a Kotlin Multiplatform wrapper with a
`Moq.connect(...)` facade, `Flow`s for every live sequence, and structured
cancellation that reaches the native consumer. It pulls in `dev.moq:moq-ffi`,
which carries the native binaries for Android (arm64-v8a, armeabi-v7a,
x86\_64) and desktop JVM (Linux x86\_64/aarch64, macOS arm64, Windows x64).

```kotlin ignore
dependencies {
    implementation("dev.moq:moq:<version>")   // latest: see the badge above
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-core:1.9.0")
}
```

```kotlin
import dev.moq.*

// Subscribe. The Flow is live, so run it in its own coroutine.
Moq.connect("https://relay.example.com", tlsRoots = listOf("ca.pem")).use { moq ->
    moq.announcements(AnnounceConfig(prefix = "live/", filter = "*/camera")).collect { announcement ->
        // Updates stay origin-relative; captures reports what each wildcard matched.
        println(announcement.captures())
        val broadcast = moq.requestBroadcast(announcement.prefix())
        println(broadcast.catalog())
    }
}
```

```kotlin
// Publish encoded frames, or raw pixels with the codec inside the binding.
// opusInit, packet, pts, and rgba come from your encoder or capture source.
Moq.connect("https://relay.example.com").use { moq ->
    val broadcast = moq.createBroadcast("my-stream.hang")
    val audio = broadcast.publishAudio(AudioInit(format = AudioFormat.OPUS, data = opusInit))
    audio.writeFrame(Frame(payload = packet, timestampUs = 20_000u))

    val video = broadcast.encodeVideo(
        VideoEncoderInput(format = VideoPixelFormat.RGBA, width = 1280u, height = 720u, framerate = 30u),
        VideoEncoderOutput(codec = VideoCodec.H264, track = "camera", bitrate = null, gop = null, kind = autoEncoder),
    )
    video.write(VideoFrame(timestampUs = pts, data = rgba))
    broadcast.announce(Route())
}
```

`MediaProducer.flush(timestampUs)` records a locally encoded frame's transport handoff on the broadcast media clock. Call it after `writeFrame` for live encoder output; omit it for file, pipe, and network imports. `MediaProducer` is a typealias, so the generated method is available directly.

Call `media.discontinuity()` when the source seeks, pauses, or changes its time base. It publishes a timeline marker and restarts handoff measurement without lowering advertised jitter. Resume with timestamps that continue forward on the broadcast media clock; this does not permit timestamp rewinds.

The three advertising operations: `moq.createBroadcast(path)` (or
`origin.createBroadcast`) returns an unannounced producer, invisible to everyone;
`broadcast.announce(route)` / `broadcast.unannounce()` own that exact-path
advertisement, and `broadcast.close()` (or `use { }`) releases the producer,
ending the broadcast once no `dynamic()` handle remains; `origin.dynamic(prefix, route)` claims `prefix` and every
path beneath it (`""` for everything). Hold the returned `OriginDynamic`
while the claim should stay advertised, and reject the requests you will not
serve. A route is a capability, not an inventory. `announcements(config)` takes
a literal prefix plus an optional relative pattern; `announcement.prefix()`
stays origin-relative and `captures()` reports the wildcard matches. Paths with
a `.`-prefixed segment below the prefix are [hidden](/concept/moq-lite#hidden-broadcasts) unless `hidden = true`.

Sessions reconnect with backoff when the transport drops and re-announce local
broadcasts. `moq.epoch()` counts the connections, 1 on the first, pairing with
`MoqSession.status` to log each reconnect; the `backoff` argument tunes the
pacing (`timeoutUs = 0` retries forever); and `maxStreams` raises the peer's
inbound stream cap.

The [WebSocket fallback](/concept/transport#websocket-fallback) races QUIC after
a 200 ms head start. `Moq.connect(websocketEnabled = false)` turns it off for a
QUIC-only relay, and a `websocketDelay` `Duration` changes the head start.

`Server.listen(bind, tlsGenerate = ...)` accepts sessions with per-request
`accept()`/`reject()`. Generated configuration setters, including
`MoqRequest.setPublish`/`setConsume`, throw if a connect, listen, or accept is
in flight, or after cancel. `MoqRequest.transport()` returns a `Transport` enum.
JSON tracks take `@Serializable` types
(`publishJsonSnapshot`, `publishJsonStream`, `valuesAs<T>()`), and the rest of
the [shared feature list](/lib/#what-every-binding-can-do) maps one to one:
`fetchGroup`/`fetchMediaGroup`, `dynamic()` for tracks and `dynamic(prefix)` for broadcasts, `appendDatagram`/`datagrams()`,
`setCatalogSection`, `demand()` for `used()`/`unused()`. `session.bandwidth()` divides the
connection's send estimate; pass it to `encodeVideo` / `encodeAudio` or
`reserve` a share for an app-owned track. `MoqException.isAuth` and
`isShutdown` classify errors. Microsecond fields read back as a
`kotlin.time.Duration`: `stats.rtt`, `backoff.initial`, `frame.timestamp`. `protocolError` is the structured protocol failure
(scope, verbatim code, kind) when the peer sent one. Cancelling the collecting coroutine cancels the
native side.

`encodeAudio` encodes raw PCM inside the binding. Its codec is an object,
`AudioCodec.opus()`, and `AudioEncoderOutput.frameDurationUs` sets the Opus
frame length: 2500, 5000, 10000, 20000 (the default), 40000, or 60000.

Each frame from `decodeVideo` owns its decoded picture until `close()` (or
`use {}`), including after the consumer is cancelled. `frame.pixels(format)`
converts it on demand: `VideoPixelFormat.I420`, or `VideoPixelFormat.RGBA` for
four bytes a pixel. Close frames promptly, since held frames hold decoder
buffers. `resize` is best effort: only NVDEC has a built-in scaler, and
MediaCodec is not it, so read each frame's own `width()` and `height()` rather
than assuming it took.

## Connection stats

`session.stats()` returns a `ConnectionStats` snapshot. Each field is `null`
when the transport backend does not report it (native QUIC reports all of them;
browser WebTransport reports few or none) or before it is available, which is
not the same as zero. `rttUs` is microseconds; the `rtt` extension property
reads it as a `kotlin.time.Duration`.

| Field | Unit | Meaning |
| --- | --- | --- |
| `rttUs` | microseconds | Smoothed round-trip time. |
| `estimatedSendRateBps` | bits per second | Send bandwidth from the congestion controller. |
| `estimatedRecvRateBps` | bits per second | Receive bandwidth from MoQ PROBE. |
| `bytesSent` | bytes | Total sent, including retransmissions and overhead. |
| `bytesReceived` | bytes | Total received, including duplicates and overhead. |
| `bytesLost` | bytes | Total lost, detected via retransmission or acknowledgement. |
| `packetsSent` | datagrams | Total datagrams sent. |
| `packetsReceived` | datagrams | Total datagrams received. |
| `packetsLost` | datagrams | Total datagrams detected as lost. |

- API reference: [javadoc.io/doc/dev.moq/moq](https://javadoc.io/doc/dev.moq/moq)
- Source: [`kt/`](https://github.com/moq-dev/moq/tree/main/kt); `just kt check` builds and tests locally
- Artifacts: [dev.moq:moq](https://central.sonatype.com/artifact/dev.moq/moq), [dev.moq:moq-ffi](https://central.sonatype.com/artifact/dev.moq/moq-ffi)

Raw track publisher metadata has an optional maximum age. Omitting it imposes no publisher age limit; zero keeps the live edge. Local cache limits still apply, and media imports explicitly retain 30 seconds. See [publisher retention](/concept/moq-lite).
