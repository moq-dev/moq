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
Publish JSON tracks with `TrackInfo(compress = true)`; a `dev.moq.json` consumer reads one with no option of its own.
A missing data timestamp stays untimed; the library never substitutes zero or now.
`dev.moq.json` consumers' `values()` emits `Value` (`value`, `timestamp`) and `valuesAs<T>()` emits `Timed<T>`;
a snapshot consumer yields each state in order, and `latest()` skips to the newest. Read timestamps are rounded down to a whole microsecond.
Raw tracks with a `null` timescale are untimed on both publish and receive.
`MoqTrackInfo.compress` on a raw track sends each group DEFLATE-compressed to opt-in `moq-lite-07-wip` peers; other peers, and every reader here, get the original frames.
Whole-frame media writes require caller time; container imports keep their own timeline handling.
Timed raw/data tracks require caller-supplied timestamps. Use `CatalogProducer.now()` to stamp a locally
created event on the broadcast clock; reading it fixes that clock for later media producers.
Share `Timebase(broadcast)` through `ContainerInit` when several imports use one source
timeline; omitting it keeps an independent offset. A timebase belongs to its broadcast
and does not keep it open. `ContainerProducer` takes chunks split anywhere, so a raw
byte stream needs no other importer.
`ContainerFormat.FMP4` splits each fragment into one frame per sample.
Construct readers with `CatalogConsumer.subscribe`, `TrackConsumer.subscribe`, or
`GroupConsumer.fetch`; the last two take a catalog `Rendition` and follow its broadcast and
track themselves. For arrival-order reads, decode each group from
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
        // A restart is a new publisher run at the same path: request it again. A takeover
        // continues what an earlier route served, like a start.
        val announce = when (event) {
            is AnnounceEventStart -> event.announce
            is AnnounceEventRestart -> event.announce
            is AnnounceEventTakeover -> event.announce
            else -> return@collect // Update or End
        }
        // Prefixes stay origin-relative; captures reports what each wildcard matched.
        println(announce.captures)
        val broadcast = moq.request(announce.prefix)
        println(catalog(broadcast))
    }
}
```

```kotlin
// Publish encoded frames, or raw pixels with the codec inside the binding.
// opusInit, packet, pts, and rgba come from your encoder or capture source.
Moq.connect("https://relay.example.com").use { moq ->
    val publication = moq.createBroadcast("my-stream.hang")
    val broadcast = publication.broadcast()
    val audio = dev.moq.media.TrackProducer.audio(broadcast, Named(null), AudioInit(format = AudioFormat.OPUS, data = opusInit))
    audio.writeFrame(Frame(payload = packet, timestampUs = 20_000u))
    audio.cut() // audio has no keyframes, so this is what gives it groups

    val video = dev.moq.video.Producer(
        CatalogProducer(broadcast),
        dev.moq.video.ProducerOptions(input = VideoEncoderInput(format = VideoPixelFormat.RGBA, width = 1280u, height = 720u, framerate = dev.moq.video.FrameRate(30000u, 1001u)),
        output = VideoEncoderOutput(codec = VideoCodec.H264, bitrate = null, gop = null, kind = autoEncoder), track = "camera"),
    )
    video.write(VideoFrame(timestampUs = pts, data = rgba))
    publication.announce(AnnounceRoute(epoch = mintEpoch())) // a fresh epoch per run
    audio.finish()
    video.finish()
    broadcast.end()
    moq.shutdown()
    moq.session.closed()
}
```

Raw video encoders take an exact numerator/denominator frame rate, such as 30000/1001. Producer options hold input, codec settings, track naming, and optional session bandwidth ownership; standalone encoders take only codec settings. For simulcast, give each rendition the next one down as `lower`: it stops encoding, advertised as disabled, while its bandwidth grant cannot fund it, and resumes once the grant recovers. A rung only disables while the session probes for bandwidth, which on native transports takes moq-quic with BBR3.

## Things to know

The rest of the [shared feature list](/lib/#what-every-binding-can-do) maps
one to one; the API reference has the names, and [Layers](/lib/layers)
shows which module holds each.

- **`end`, not `close`.** `broadcast.end()` ends a broadcast for good. `close()`, or leaving `use { }`, releases the handle, which ends the broadcast only once no other handle to it remains, such as its publication or a `requests()` handle.
- **Audio needs cuts.** Video groups at its keyframes, but audio forms a group only where you call `cut()`: after every frame, or at a segment cadence to align with video.
- **Timeline breaks.** On a seek or pause, call `discontinuity()`, then keep timestamps moving forward and resume video on a keyframe.
- **Media reads.** `dev.moq.media.TrackConsumer`, `dev.moq.audio.Consumer`, and `dev.moq.video.Consumer` return a `Read` in order: `ReadFrame`, a `ReadGap` where media was lost, or a `ReadEnd` where the publisher stopped its encoder. A gap breaks continuity. An encoder end is not stream EOF: following frames before its endpoint are the old encoder's drain; the first at or past it starts fresh.
- **Decoded frames hold decoder buffers.** `close()` each frame from a `dev.moq.video.Consumer` promptly (or `use { }` it), or the decoder stalls. `resize` is best effort, so read each frame's `width()` and `height()`.
- **Closing.** `moq.shutdown()` requests a drain immediately; suspending `moq.session.closed()` waits up to one second and reports its result. `Moq.close()`, and so `use { }`, cancels at once. Finish or abort live tracks first. Application delivery requires explicitly selecting `moq-lite-07-wip` on both peers; older Lite versions (including default lite-06) and IETF wait only for transport ACKs.
- **Ending cleanly.** When the input ends, finish every track, then call `finish()` on its `CatalogProducer` to finish the catalog, which ends the content. The broadcast stays announced and keeps serving the finished tracks, so a subscriber still catching up can drain them, until `broadcast.end()` takes it offline. How long to keep it up is yours to decide. A track still open at the end, or released without a finish, ends with `Unroutable`, which readers take as the publisher leaving.
- **Stats.** `session.stats()` reports `rtt`, `estimatedSendRateBps`, and the byte and packet counters (`bytesSent`, `bytesReceived`, `bytesLost`, `packetsSent`, `packetsReceived`, `packetsLost`). A field is `null` when the transport does not report it, which is not the same as zero. `version` names the negotiated protocol. Connect returns a connecting session; read its coherent `state()` snapshot and wait with that snapshot's revision to observe first-dial failures and reconnects. Each watcher keeps its own revision. Hold the owned `BandwidthConsumer` returned by `session.recvBandwidth()` to receive bandwidth estimates. `rate()` reads the current estimate in bits per second and `rateChanged()` waits for a new one. Release the consumer or call `cancel()` when finished; releasing the last consumer stops PROBE; the session and stats snapshots do not request it. Client consumers survive reconnects and report no estimate while disconnected.
- **Authorization.** On `moq-lite-07-wip` (opt in with `ClientConfig(versions = ...)`) and on moq-transport draft-18+ with the [MoQ Auth extension](/draft/moq-auth), collect `moq.grants().updates()` for the `Grant` the relay sent back and each change, `null` until it answers and while a client session reconnects. Ending the collection cancels the consumer.
- **Tokens.** Pass `ClientConfig(tokens = listOf(...))` instead of putting the credential in the URL. The first is the connection credential, sent in SETUP and, while an offered version can't carry it there (lite before 07), as the URL's `?jwt=`; each other token widens the grant over AUTH. `moq.tokens().set(listOf(...))` replaces them before they expire: the next dial presents them, a live AUTH session at once, and after an authorization refusal the session redials only if they changed. Suspending `tokens().answered()` returns once the relay accepts them and throws its refusal with the relay's code. A `?jwt=` already in the URL stays the connection credential, which is how to reach a relay older than this release.
- **Errors.** Read diagnostic payloads from `reason` and structured protocol failures from `details`.
- **Disabled renditions.** A rendition with `enabled: false` has no frames coming until its publisher re-enables it. Select an enabled rendition from the latest catalog before opening a decoder. Keep one already playing while every rendition of its kind is disabled: the convenience decoders accept a disabled configuration, and a re-enable, such as an unmute, then plays without a resubscribe. Among the video renditions you can decode, prefer the highest `preference`; `0` is the default and `-1` marks a compatibility transcode.
- **Subscription defaults.** `Subscription.default()` returns moq-net's defaults, whose zero `maxDelay` skips a superseded group at once; passing no subscription means the same. `copy` it rather than starting from `Subscription()`, whose null `maxDelay` sets no subscriber limit.
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
A session closed because its credential expired reports `ProtocolKind.EXPIRED`, and a request refused for an expired token raises `MoqException.Expired`; refresh the token before retrying. Malformed credentials report `ProtocolKind.MALFORMED_AUTH_TOKEN`; correct the token before reconnecting with it.
A catalog that needs a newer moq-hang draft than this build reads raises `MoqException.UnsupportedCatalog`; upgrade the library to read it.

## Reference

- API reference: [javadoc.io/doc/dev.moq/moq](https://javadoc.io/doc/dev.moq/moq)
- Source: [`kt/`](https://github.com/moq-dev/moq/tree/main/kt); `just kt check` builds and tests locally
- Artifacts: [dev.moq:moq](https://central.sonatype.com/artifact/dev.moq/moq), [dev.moq:moq-ffi](https://central.sonatype.com/artifact/dev.moq/moq-ffi)
