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

```kotlin
dependencies {
    implementation("dev.moq:moq:<version>")   // latest: see the badge above
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-core:1.9.0")
}
```

```kotlin
import dev.moq.*

// Subscribe. The Flow is live, so run it in its own coroutine.
Moq.connect("https://relay.example.com", tlsRoots = listOf("ca.pem")).use { moq ->
    moq.announcements("live/").collect { announcement ->
        val catalog = announcement.broadcast().catalog()
        println(catalog)
    }
}
```

```kotlin
// Publish encoded frames, or raw pixels with the codec inside the binding.
// opusInit, packet, pts, and rgba come from your encoder or capture source.
Moq.connect("https://relay.example.com").use { moq ->
    val broadcast = moq.createBroadcast("my-stream.hang")
    val audio = broadcast.publishMedia(Init(format = "opus", data = opusInit, video = null))
    audio.writeFrame(Frame(payload = packet, timestampUs = 20_000u))

    val video = broadcast.publishVideo(
        VideoEncoderInput(format = VideoPixelFormat.RGBA, width = 1280u, height = 720u, framerate = 30u),
        VideoEncoderOutput(codec = VideoCodec.H264, track = "camera", bitrate = null, gop = null, kind = autoEncoder),
    )
    video.write(VideoFrame(timestampUs = pts, data = rgba))
}
```

`Server.listen(bind, tlsGenerate = ...)` accepts sessions with per-request
`accept()`/`reject()`. JSON tracks take `@Serializable` types
(`publishJsonSnapshot`, `publishJsonStream`, `valuesAs<T>()`), and the rest of
the [shared feature list](/lib/#what-every-binding-can-do) maps one to one:
`fetchGroup`/`fetchMediaGroup`, `dynamic()`, `appendDatagram`/`datagrams()`,
`setCatalogSection`, `used()`/`unused()`. `MoqException.isAuth` and
`isShutdown` classify errors. Cancelling the collecting coroutine cancels the
native side.

- API reference: [javadoc.io/doc/dev.moq/moq](https://javadoc.io/doc/dev.moq/moq)
- Source: [`kt/`](https://github.com/moq-dev/moq/tree/main/kt); `just kt check` builds and tests locally
- Artifacts: [dev.moq:moq](https://central.sonatype.com/artifact/dev.moq/moq), [dev.moq:moq-ffi](https://central.sonatype.com/artifact/dev.moq/moq-ffi)
