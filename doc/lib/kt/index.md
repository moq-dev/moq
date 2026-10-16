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

`dev.moq.media` owns catalogs, encoded imports, and container consumers. `TrackProducer.audio`
/ `.video` take a broadcast, a `Named` or `Requested` target, and an init record.
`CatalogProducer(broadcast)` holds its broadcast weakly and fails with `Closed` after it closes.
`dev.moq.flate.SnapshotProducer` / `StreamProducer` publish opaque bytes with an optional `timestampUs`.
A missing data timestamp stays untimed; the library never substitutes zero or now.
Raw tracks with a `null` timescale are untimed on both publish and receive.
Whole-frame media writes require caller time; container imports keep their own timeline handling.
Timed raw/data tracks require caller-supplied timestamps. Use `CatalogProducer.now()` to stamp a locally
created event on the broadcast clock; reading it fixes that clock for later media producers.
Share `Timebase(broadcast)` through container init records when several imports use
one source timeline. `ContainerStreamProducer` takes `ContainerStreamInit`; omitting
its timebase keeps an independent offset. A timebase belongs to its broadcast and
does not keep it open.
Construct readers with `CatalogConsumer.subscribe`, `TrackConsumer.subscribe`, or
`GroupConsumer.fetch`. For arrival-order reads, decode each group from
`dev.moq.TrackConsumer.recvGroup()` with `dev.moq.media.GroupConsumer(group, container)`; frames
carry container media time as they land. Like `TrackProducer`, these names also exist at the
root for moq-net handles, so qualify them when both packages are star-imported.

```kotlin ignore
dependencies {
    implementation("dev.moq:moq:<version>")   // latest: see the badge above
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-core:1.9.0")
}
```

```kotlin
import dev.moq.*
import dev.moq.media.*

// Subscribe. The Flow is live, so run it in its own coroutine.
Moq.connect("https://relay.example.com", ClientConfig(tls = ClientTls(roots = listOf("ca.pem")))).use { moq ->
    moq.announced(AnnounceConfig(prefix = "live/", filter = "*/camera")).updates().collect { event ->
        // A restart is a new publisher run at the same path: request it again.
        val announce = when (event) {
            is AnnounceEventStart -> event.announce
            is AnnounceEventRestart -> event.announce
            else -> return@collect // Update or End
        }
        // Prefixes stay origin-relative; captures reports what each wildcard matched.
        println(announce.captures)
        val broadcast = moq.requestBroadcast(announce.prefix)
        println(catalog(broadcast))
    }
}
```

```kotlin
// Publish encoded frames, or raw pixels with the codec inside the binding.
// opusInit, packet, pts, and rgba come from your encoder or capture source.
Moq.connect("https://relay.example.com").use { moq ->
    val broadcast = moq.createBroadcast("my-stream.hang")
    val audio = dev.moq.media.TrackProducer.audio(broadcast, Named(null), AudioInit(format = AudioFormat.OPUS, data = opusInit))
    audio.writeFrame(Frame(payload = packet, timestampUs = 20_000u))
    audio.cut() // audio has no keyframes, so this is what gives it groups

    val video = broadcast.encodeVideo(
        VideoEncoderInput(format = VideoPixelFormat.RGBA, width = 1280u, height = 720u, framerate = 30u),
        VideoEncoderOutput(codec = VideoCodec.H264, track = "camera", bitrate = null, gop = null, kind = autoEncoder),
    )
    video.write(VideoFrame(timestampUs = pts, data = rgba))
    broadcast.announce(Route(epoch = mintEpoch())) // a fresh epoch per run
    audio.finish()
    video.finish()
    broadcast.end()
    moq.shutdown()
}
```

## Things to know

The rest of the [shared feature list](/lib/#what-every-binding-can-do) maps
one to one; the API reference has the names.

- **`end`, not `close`.** `broadcast.end()` ends a broadcast for good. `close()`, or leaving `use { }`, releases the handle, which ends the broadcast only once no `dynamic()` handle remains.
- **Audio needs cuts.** Video groups at its keyframes, but audio forms a group only where you call `cut()`: after every frame, or at a segment cadence to align with video.
- **Live encoder timing.** After writing a frame you encoded yourself, call `flush(timestampUs)` with the same timestamp so the catalog advertises your jitter. Skip it for file and network imports. On a seek or pause, call `discontinuity()`, then keep timestamps moving forward and resume video on a keyframe.
- **Decoded frames hold decoder buffers.** `close()` each frame from `decodeVideo` promptly (or `use { }` it), or the decoder stalls. `resize` is best effort, so read each frame's `width()` and `height()`.
- **Closing.** Suspending `moq.shutdown()` drains for up to one second and throws on failure. `Moq.close()`, and so `use { }`, cancels at once. Finish or abort live tracks first. Application delivery requires explicitly selecting `moq-lite-07-wip` on both peers; older Lite versions (including default lite-06) and IETF wait only for transport ACKs.
- **Stats.** `session.stats()` reports `rttUs`, `estimatedSendRateBps`, `estimatedRecvRateBps`, and the byte and packet counters (`bytesSent`, `bytesReceived`, `bytesLost`, `packetsSent`, `packetsReceived`, `packetsLost`). A field is `null` when the transport does not report it, which is not the same as zero.
- **Errors.** Read diagnostic payloads from `reason` and structured protocol failures from `details`.
- **Disabled renditions.** Audio and video convenience decoders reject a catalog configuration with `enabled: false`. Select an enabled rendition from the latest catalog before opening a decoder.
- **Track demand.** Obtain a dynamic track request’s `demand()` before accepting it to watch subscribers through the handoff to its producer. The watch does not keep the track alive and ends when the track closes or the request is rejected.

## Runtime

Native I/O and background tasks run on a Tokio worker pool. Calls awaited by the
host still poll in place; independent native wake callbacks can run concurrently.
The generated bindings resume Kotlin coroutines through their dispatcher. Use
the UI dispatcher when updating UI state.
A panic in a native background task reaches the pending and later fallible calls
of every object that depends on it as a task error; other objects keep running.

A session closed for exceeding the relay's request caps reports the protocol
kind `ProtocolKind.TOO_MANY_REQUESTS`. Reduce
the simultaneous announcements or subscriptions, or ask the relay operator
to adjust the [session limits](/bin/relay/config#session-and-peer), before
reconnecting with the same workload.
A session closed because its credential expired reports `ProtocolKind.EXPIRED`, and a request refused for an expired token raises `MoqException.Expired`; refresh the token before retrying.

## Reference

- API reference: [javadoc.io/doc/dev.moq/moq](https://javadoc.io/doc/dev.moq/moq)
- Source: [`kt/`](https://github.com/moq-dev/moq/tree/main/kt); `just kt check` builds and tests locally
- Artifacts: [dev.moq:moq](https://central.sonatype.com/artifact/dev.moq/moq), [dev.moq:moq-ffi](https://central.sonatype.com/artifact/dev.moq/moq-ffi)
