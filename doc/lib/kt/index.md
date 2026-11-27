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
Publish JSON tracks with `TrackInfo(maxAge = Duration.ZERO, compression = Compression.deflate())`; a `dev.moq.json` consumer reads one with no option of its own.
Every write takes a timestamp, and the library never substitutes zero or now.
`SnapshotProducer.update`, `StreamProducer.append`, and `WindowProducer.push` and `pop` require a timestamp on the broadcast clock (`CatalogProducer.now()`), the epoch every track in the broadcast shares, so their track needs a timescale.
`dev.moq.json` consumers' `values()` emits `Value` (`value`, `timestamp`) and `valuesAs<T>()` emits `Timed<T>`;
a snapshot consumer yields each state in order, and `latest()` skips to the newest.
A `Timestamp(value, scale)` is exact and reads back at the track's timescale, so a 90 kHz tick republishes unchanged.
`Duration.toTimestamp()` is exact at nanosecond scale; `Timestamp.toDuration()` rounds down to a whole nanosecond.
A raw track with a `null` timescale is untimed: each frame carries `Timestamp.unscaled(key)`, its group's key, whose `scale` is `null`.
`TrackInfo` names `maxAge` (a `java.time.Duration`), how long a superseded group stays fresh: `Duration.ZERO` serves only the live edge, and `null` sets no publisher limit, as `null` does for `Delay.max`.
`TrackInfo.compression` set to `Compression.deflate()` on a raw track sends each group DEFLATE-compressed to opt-in `moq-lite-07-wip` peers; other peers, and every reader here, get the original frames.
Whole-frame media writes require caller time; container imports keep their own timeline handling.
Timed raw/data tracks require caller-supplied timestamps. Use `CatalogProducer.now()` to stamp a locally
created event on the broadcast clock; reading it fixes that clock for later media producers.
Share `Timebase(broadcast)` through `ContainerInit` when several imports use one source
timeline; omitting it keeps an independent offset. A timebase belongs to its broadcast
and does not keep it open. `ContainerProducer` takes chunks split anywhere, so a raw
byte stream needs no other importer.
`ContainerFormat.fmp4()` splits each fragment into one frame per sample.
Construct readers with `CatalogConsumer.subscribe`, `TrackConsumer.subscribe`, or
`GroupConsumer.fetch`; the last two take a catalog `Rendition` and follow its broadcast and
track themselves. For arrival-order reads, decode each group from
`dev.moq.TrackConsumer.recvGroup()` with `dev.moq.media.GroupConsumer(group, rendition)`; frames
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
        // A path stays present across publisher restarts: hold moq.follow(path) to be handed
        // each new run, or null once it goes offline.
        val announce = when (event) {
            is AnnounceEventPresent -> event.announce
            is AnnounceEventAbsent -> return@collect
        }
        // Prefixes stay origin-relative; captures is a list of wildcard matches, empty without wildcards.
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
    // Hidden until the catalog is filled; hold the route while it should stay announced.
    val route = moq.route("my-stream.hang", RouteOptions(hidden = true))
    val broadcast = route.broadcast()
    val audio = dev.moq.media.TrackProducer.audio(broadcast, Named(null), AudioInit(format = AudioFormat.opus(), data = opusInit))
    audio.writeFrame(Frame(payload = packet, timestamp = Timestamp(960u, 48_000u)))
    audio.cut() // audio has no keyframes, so this is what gives it groups

    val video = dev.moq.video.Producer(
        CatalogProducer(broadcast),
        dev.moq.video.ProducerOptions(input = VideoEncoderInput(format = VideoPixelFormat.rgba(), width = 1280u, height = 720u, framerate = dev.moq.video.FrameRate(30000u, 1001u)),
        output = VideoEncoderOutput(codec = VideoCodec.h264(), bitrate = null, join = null, kind = autoEncoder), track = "camera"),
    )
    video.write(VideoFrame(timestamp = pts, data = rgba))
    route.announce() // the route mints its own identity: route.identity()
    audio.finish()
    video.finish()
    broadcast.end()
    moq.shutdown()
    moq.session.closed()
}
```

Raw video encoders take an exact numerator/denominator frame rate, such as 30000/1001. Producer options hold input, codec settings, track naming, and optional session bandwidth ownership; standalone encoders take only codec settings. For simulcast, give each rendition the next one down as `lower`: it stops encoding, advertised as disabled, while its bandwidth grant cannot fund it, and resumes once the grant recovers. A rung only disables while the session probes for bandwidth, which on native transports takes moq-quic with BBR3. A pre-encoded video track takes `lower` in its `VideoInit` too, as a `uniffi.moq.MoqMediaProducer.Track` or `.Stream`, which only places it one priority below; follow its reservation with `RateControl`, and while it reports disabled, drop the frames, call `setEnabled(false)` after a `discontinuity()`, and `setEnabled(true)` on the keyframe that resumes it. A `TrackStreamProducer` finds its own frames: `setEnabled(false)` drops what is written to it, and after `setEnabled(true)` it resumes at the stream's next keyframe.

## Things to know

The rest of the [shared feature list](/lib/#what-every-binding-can-do) maps
one to one; the API reference has the names, and [Layers](/lib/layers)
shows which module holds each.

- **`end`, not `close`.** `broadcast.end()` ends a broadcast for good, retracting its route. `close()`, or leaving `use { }`, releases the handle, which ends the broadcast only once no other handle to it remains, such as its route or a `requests()` handle. A route is the opposite: closing it retracts it at once, so hold it while the broadcast should stay announced.
- **Audio needs cuts.** Video groups at its keyframes, but audio forms a group only where you call `cut()`: after every frame, or less often to batch frames into fewer groups.
- **Timeline breaks.** On a seek or pause, call `discontinuity()`, then keep timestamps moving forward and resume video on a keyframe.
- **Media reads.** `dev.moq.media.TrackConsumer`, `dev.moq.audio.Consumer`, and `dev.moq.video.Consumer` return a `Read` in order: `ReadFrame`, a `ReadGap` where media was lost, or a `ReadEnd` where the publisher stopped its encoder. A gap breaks continuity. An encoder end is not stream EOF: following frames before its endpoint are the old encoder's drain; the first at or past it starts fresh. A decoder reads everything the track still holds unless a live player sets `start = DecodeStart.LATEST` in its decoder output, which joins at the newest group and skips the cached backlog.
- **Decoded frames hold decoder buffers.** `close()` each frame from a `dev.moq.video.Consumer` promptly (or `use { }` it), or the decoder stalls. `resize` is best effort, so read each frame's `width()` and `height()`.
- **Closing.** `moq.shutdown()` requests a drain immediately; suspending `moq.session.closed()` waits up to one second and reports its result. `Moq.close()`, and so `use { }`, cancels at once. Finish or abort live tracks first. moq-lite-05 and newer wait for the peer to receive finished tails; older Lite versions and IETF wait only for transport ACKs.
- **Ending cleanly.** When the input ends, finish every track, then call `finish()` on its `CatalogProducer` to finish the catalog, which ends the content. The broadcast stays announced and keeps serving the finished tracks, so a subscriber still catching up can drain them, until `broadcast.end()` takes it offline. How long to keep it up is yours to decide. A track still open at the end ends with `Unroutable` (`Internal` over moq-transport, which has no such code), and one released without a finish ends cleanly while nobody reads it; a reader still reading is served by the next request for the track while a request handler is live. That handler must accept it with the info the track was published with: another info ends the track with `MalformedTrack`. Serve an encoder's track with `dev.moq.video.Producer.requested()` or `dev.moq.audio.Producer.requested()`, given the request and the options the track was published with: `MalformedTrack` catches only a changed track info, not changed codec settings, which its readers decode with. With no handler, or once the producer serving that request is released too before another reader joins, the reader reads `Dropped`, seen as `Internal`. A media container producer, and a JSON stream or window that published a record, are the exceptions: finish them, since releasing one unfinished ends its tracks with `Dropped`. Readers follow a publisher's runs by announcement, not by that error: see [identity and recovery](/concept/identity).
- **Stats.** `session.stats()` reports `rtt`, `estimatedSendRateBps`, and the byte and packet counters (`bytesSent`, `bytesReceived`, `bytesLost`, `packetsSent`, `packetsReceived`, `packetsLost`), and `datagramsUnrouted`, the datagrams received for a subscription with no datagram track, reported only on moq-lite 05 and later, which carry datagrams. A field is `null` when the transport does not report it, which is not the same as zero. `version` names the negotiated protocol. Connect returns a connecting session; read its coherent `state()` snapshot and wait with that snapshot's revision to observe reconnects. Until the peer has accepted a session's credential or drained a session with a GOAWAY (or, on moq-lite 03/04, which never answer it, a session stayed up 1s), a failed dial or a closed session, such as a refused credential, ends the session with that error rather than retrying. Each watcher keeps its own revision. Hold the owned `BandwidthConsumer` returned by `session.recvBandwidth()` to receive bandwidth estimates. `rate()` reads the current estimate in bits per second and `rateChanged()` waits for a new one. Release the consumer or call `cancel()` when finished; releasing the last consumer stops PROBE; the session and stats snapshots do not request it. Client consumers survive reconnects and report no estimate while disconnected.
- **Viewers.** `moq.viewers()` divides the connection's receive estimate among the broadcasts you watch, max-min fair by each one's ceiling. `watch(maxBps)` returns a `Viewer` claiming up to what the largest rendition the player would switch to needs; `rate()` reads its slice in bits per second, `null` while there is no estimate, so hold the current selection. `update(maxBps)` changes the ceiling, such as when the catalog enables or disables a rendition. Holding a viewer requests PROBE, and the session sends the viewers' summed ceilings as the PROBE target; `close()` it, or leave `use { }`, once the broadcast stops being watched. `setTarget(bps)` sends another target, `0` sends none, and `null` follows the viewers again. Ceilings or a target past 2^53 - 1 bps, the most a PROBE target carries, throw `MoqException.OutOfRange`. Client viewers survive reconnects and read `null` while disconnected.
- **Authorization.** On `moq-lite-07-wip` (opt in with `ClientConfig(versions = ...)`), collect `moq.grants().updates()` for the `Grant` the relay sent back and each change, `null` until it answers and while a client session reconnects. Ending the collection cancels the consumer.
- **Tokens.** Pass `ClientConfig(tokens = listOf(...))` instead of putting the credential in the URL. The first is the connection credential, sent in SETUP and, while an offered version can't carry it there (lite before 07), as the URL's `?jwt=`; each other token widens the grant over AUTH. `moq.tokens().set(listOf(...))` replaces them before they expire: the next dial presents them, a live AUTH session at once. Suspending `tokens().answered()` returns once the relay accepts them and throws its refusal with the relay's code. A `?jwt=` already in the URL stays the connection credential, which is how to reach a relay older than this release. For a per-request token, `moq.credential(token)` returns a `Credential` that `route.setAuth(credential)` or `claim.setAuth(credential)` names (`null` for none); setting a new one renews the announcement in place. Only the session that minted it carries it, a client session across its reconnects; another session announcing the same route uses its own scope. On moq-transport `answered` resolves at once, since only the request carries the token; on moq-lite the token rides AUTH instead (07 only, unsupported before), so `answered` is the peer's verdict and the grant joins the whole session's, not only the named route's. Suspending `credential.answered()` throws the peer's refusal, and `credential.cancel()` withdraws it unless a route or claim still names it.
- **Errors.** Read diagnostic payloads from `reason` and structured protocol failures from `details`.
- **Catalog timing.** Each catalog a `CatalogConsumer` yields carries the `timestamp` it takes effect at on the broadcast's media timeline. `CatalogConsumer.subscribe(broadcast, Duration.ZERO)` reads only the newest catalog; a player that holds each update until its media reaches that time passes its playout delay instead, and reads in order every update it may still need.
- **Disabled renditions.** Audio and video convenience decoders reject a catalog configuration with `enabled: false`. Select an enabled rendition from the latest catalog before opening a decoder. Among the video renditions you can decode, prefer the highest `preference`; `0` is the default and `-1` marks a compatibility transcode. Audio and video subscribe setup returns `MoqException.UnsupportedConfig` when the decoder refuses the codec, layout, or description; a player can then try its next ranked rendition. Decoder faults and later peer or decode failures do not report that setup refusal.
- **Subscription defaults.** `Subscription.default()` returns moq-net's defaults, whose zero `delay` starts at the newest group and skips a superseded one at once; passing no subscription means the same. `copy` it to change a field; building a `Subscription(...)` must name its `Delay(min, max)`, where a null `max` sets no subscriber limit and a null `min` leaves the target latency unstated. Decoders take the same `Delay`.
- **Track demand.** Obtain a dynamic track request’s `demand()` before accepting it to watch subscribers through the handoff to its producer. The watch does not keep the track alive and ends when the track closes or the request is rejected. A producer publishes everything you write, subscribed or not, so a late subscriber can fetch the recent past. Capturing only while a producer's `demand()` reports a subscriber is an opt-in that saves the encode but leaves nothing from before the first one. To end a track nobody wants, release its producer once `wantedChanged()` returns false; a subscriber that joined since is served by the next request for the track while a request handler is live, and reads `Dropped` without one.
- **Group refresh.** `demand().refresh()` returns once a subscriber asks for a new group: the first group of a track with none, or a join judged by `TrackInfo(refresh = TrackRefresh(ratio = ..., interval = ...))`. Write a group starting with a keyframe or snapshot, which answers it, then wait again. The ratio asks on a join once the newest group has grown past that many times its first frame; the interval has a request wait until the newest group is that old, which caps how often you make groups however many subscribers ask. A subscriber asks with `consumer.refresh()`, which returns once the request is sent (forwarded upstream, or read by a local publisher); cancelling the coroutine first withdraws it.

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
