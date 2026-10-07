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
    moq.announcements(AnnounceConfig(prefix = "live/", filter = "*/camera")).collect { event ->
        if (event !is AnnounceEventStart) return@collect // Update or End
        // Prefixes stay origin-relative; captures reports what each wildcard matched.
        println(event.announce.captures)
        val broadcast = moq.requestBroadcast(event.announce.prefix)
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
    audio.cut() // audio has no keyframes, so this is what gives it groups

    val video = broadcast.encodeVideo(
        VideoEncoderInput(format = VideoPixelFormat.RGBA, width = 1280u, height = 720u, framerate = 30u),
        VideoEncoderOutput(codec = VideoCodec.H264, track = "camera", bitrate = null, gop = null, kind = autoEncoder),
    )
    video.write(VideoFrame(timestampUs = pts, data = rgba))
    broadcast.announce(Route())
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
- **Closing.** Suspending `moq.shutdown()` gives finished tracks up to one second to deliver and throws if they did not. `Moq.close()`, and so `use { }`, cancels at once. Finish or abort live tracks first.
- **Stats.** `session.stats()` reports `rttUs`, `estimatedSendRateBps`, `estimatedRecvRateBps`, and the byte and packet counters (`bytesSent`, `bytesReceived`, `bytesLost`, `packetsSent`, `packetsReceived`, `packetsLost`). A field is `null` when the transport does not report it, which is not the same as zero.

## Reference

- API reference: [javadoc.io/doc/dev.moq/moq](https://javadoc.io/doc/dev.moq/moq)
- Source: [`kt/`](https://github.com/moq-dev/moq/tree/main/kt); `just kt check` builds and tests locally
- Artifacts: [dev.moq:moq](https://central.sonatype.com/artifact/dev.moq/moq), [dev.moq:moq-ffi](https://central.sonatype.com/artifact/dev.moq/moq-ffi)
