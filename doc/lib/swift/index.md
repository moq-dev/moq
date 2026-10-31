---
title: Swift
description: Async sequences for iOS and macOS via the Moq package
---

# Swift

[![Swift Package Index](https://img.shields.io/github/v/release/moq-dev/moq-swift?label=moq-swift)](https://github.com/moq-dev/moq-swift/releases)

The `Moq` Swift package: de-prefixed types, `AsyncSequence` on every
consumer, `Sendable` handles, and `Task` cancellation that reaches the native
side. It depends on `MoqFFI`, which ships a prebuilt XCFramework with arm64
slices for iOS 16+, the iOS Simulator, and macOS 12.3+.

`Media` owns catalogs, encoded-media importers, and container consumers.
`Media.TrackProducer.audio` / `.video` take a broadcast, an init record, and a
`.named(name:)` or `.requested(request)` target. `Media.CatalogProducer(broadcast:)`
updates catalog properties and sections without keeping the broadcast open.
Its writes fail after closing or releasing the broadcast.
Publish JSON tracks with `TrackInfo(compress: true)`; a `Json` consumer reads one with no option of its own.
A missing data timestamp stays untimed; the library never substitutes zero or now.
`Json` consumers yield `Json.Timed` (`value`, `timestampUs`); a snapshot consumer yields each state in order,
and `latest()` skips to the newest. Read timestamps are rounded down to a whole microsecond.
Raw tracks with a `nil` timescale are untimed on both publish and receive.
`MoqTrackInfo.compress` on a raw track sends each group DEFLATE-compressed to opt-in `moq-lite-07-wip` peers; other peers, and every reader here, get the original frames.
Whole-frame media writes require caller time; container imports keep their own timeline handling.
Timed raw/data tracks require caller-supplied timestamps. Use `Media.CatalogProducer.now()` to stamp a locally
created event on the broadcast clock; reading it fixes that clock for later media producers.
Share `Media.Timebase(broadcast:)` through `ContainerInit` when several imports use one
source timeline; omitting it keeps an independent offset. A timebase belongs to its
broadcast and does not keep it open. `ContainerProducer` takes chunks split anywhere,
so a raw byte stream needs no other importer.
`.fmp4` splits each fragment into one frame per sample.
For arrival-order reads, decode each group from `TrackConsumer.recvGroup()` with
`Media.GroupConsumer(group:container:)`; frames carry container media time as they land.

```swift ignore
dependencies: [
    .package(url: "https://github.com/moq-dev/moq-swift", from: "<version>"),   // latest: see the badge above
],
targets: [
    .target(name: "MyApp", dependencies: [.product(name: "Moq", package: "moq-swift")]),
]
```

```swift
import Moq

// Subscribe. The sequence is live, so run it in its own Task.
let client = try Client()
let session = try await client.connect(to: "https://relay.example.com")

for try await event in try session.consume.announced(prefix: "live/", filter: "*/camera") {
    // A restart is a new publisher run at the same path: request it again. A takeover
    // continues what an earlier route served, like a start.
    let announcement: Announce
    switch event {
    case .start(let started), .restart(let started), .takeover(let started, _): announcement = started
    default: continue // .update or .end
    }
    // Prefixes stay origin-relative; captures reports what each wildcard matched.
    print(announcement.captures ?? [])
    let broadcast = try await session.consume.request(path: announcement.prefix)
    for try await catalog in try await Media.CatalogConsumer.subscribe(broadcast: broadcast) {
        print(catalog)
    }
}
```

```swift
// Publish encoded frames, or raw pixels with the codec inside the binding (VideoToolbox).
// opusInit, packet, pts, and rgba come from your encoder or capture source.
let publication = try session.publish.createBroadcast(path: "my-stream.hang")
let broadcast = publication.broadcast
let audio = try Media.TrackProducer.audio(broadcast: broadcast, initData: Media.AudioInit(format: .opus, data: opusInit))
try audio.writeFrame(packet, timestampUs: 20_000)
try audio.cut() // audio has no keyframes, so this is what gives it groups

let video = try Video.Producer(
    catalog: Media.CatalogProducer(broadcast: broadcast),
    input: VideoEncoderInput(format: .rgba, width: 1280, height: 720, framerate: Video.FrameRate(numerator: 30000, denominator: 1001)),
    output: VideoEncoderOutput(codec: .h264, kind: .auto),
    track: "camera"
)
try video.write(VideoFrame(timestampUs: pts, data: rgba))
try publication.announce(route: AnnounceRoute(epoch: mintEpoch())) // a fresh epoch per run
try audio.finish()
try video.finish()
try broadcast.close()

session.shutdown()
try await session.closed()
```

Raw video encoders take an exact numerator/denominator frame rate, such as 30000/1001. Pass the publication track name to the producer, alongside its input and codec settings. For simulcast, give each rendition the next one down as `lower`: it stops encoding, advertised as disabled, while its bandwidth grant cannot fund it, and resumes once the grant recovers. A rung only disables while the session probes for bandwidth, which on native transports takes moq-quic with BBR3.

## Things to know

The rest of the [shared feature list](/lib/#what-every-binding-can-do) maps
one to one; the API reference has the names, and [Layers](/lib/layers)
shows which module holds each.

- **Audio needs cuts.** Video groups at its keyframes, but audio forms a group only where you call `cut()`: after every frame, or at a segment cadence to align with video.
- **Timeline breaks.** On a seek or pause, call `discontinuity()`, then keep timestamps moving forward and resume video on a keyframe.
- **Media reads.** `Media.TrackConsumer`, `Audio.Consumer`, and `Video.Consumer` yield `Read` cases in order: `.frame`, a `.gap` where media was lost, or an `.end` where the publisher stopped its encoder. A gap breaks continuity. An encoder end is not stream EOF: following frames before its endpoint are the old encoder's drain; the first at or past it starts fresh. A decoder reads everything the track still holds unless a live player sets `start: .latest` in its decoder output, which joins at the newest group and skips the cached backlog.
- **Decoded frames hold decoder buffers.** Release each frame from a `Video.Consumer` promptly, or the decoder stalls. `resize` is best effort, so read each frame's `width()` and `height()`.
- **Closing.** `session.shutdown()` requests a drain immediately; `try await session.closed()` waits up to one second and reports its result. `session.cancel(code:)` closes at once. Finish or abort live tracks first. Application delivery requires explicitly selecting `moq-lite-07-wip` on both peers; older Lite versions (including default lite-06) and IETF wait only for transport ACKs.
- **Ending cleanly.** When the input ends, finish every track, then call `try finish()` on its `Media.CatalogProducer` to finish the catalog, which ends the content. The broadcast stays announced and keeps serving the finished tracks, so a subscriber still catching up can drain them, until `broadcast.close()` takes it offline. How long to keep it up is yours to decide. A track still open at the close, or released without a finish, ends with `Unroutable`, which readers take as the publisher leaving.
- **Stats.** `session.stats()` reports `rtt`, `estimatedSendRateBps`, and the byte and packet counters (`bytesSent`, `bytesReceived`, `bytesLost`, `packetsSent`, `packetsReceived`, `packetsLost`). A field is `nil` when the transport does not report it, which is not the same as zero. `version` names the negotiated protocol. Connect returns a connecting session; read its coherent `state()` snapshot and wait with that snapshot's revision to observe first-dial failures and reconnects. Each watcher keeps its own revision. Hold the owned `BandwidthConsumer` returned by `session.recvBandwidth()` to receive bandwidth estimates. `rate()` reads the current estimate in bits per second and `rateChanged()` waits for a new one. Release the consumer or call `cancel()` when finished; releasing the last consumer stops PROBE; the session and stats snapshots do not request it. Client consumers survive reconnects and report no estimate while disconnected.
- **Authorization.** On `moq-lite-07-wip` (opt in with `Client(versions:)`) and on moq-transport draft-18+ with the [MoQ Auth extension](/draft/moq-auth), `for try await grant in session.grants()` yields the `Grant` the relay sent back and each change, `nil` until it answers and while a client session reconnects. Cancelling the iterating task cancels the native wait.
- **Tokens.** Pass `Client(tokens: [...])` instead of putting the credential in the URL. The first is the connection credential, sent in SETUP and, while an offered version can't carry it there (lite before 07), as the URL's `?jwt=`; each other token widens the grant over AUTH. `try session.tokens()?.set([...])` replaces them before they expire: the next dial presents them, a live AUTH session at once, and after an authorization refusal the session redials only if they changed. `try await tokens.answered()` returns once the relay accepts them and throws its refusal with the relay's code. A `?jwt=` already in the URL stays the connection credential, which is how to reach a relay older than this release.
- **Errors.** Diagnostic cases use the `reason:` argument label; match their associated value to inspect it. Protocol errors carry `details`.
- **Disabled renditions.** A rendition with `enabled: false` has no frames coming until its publisher re-enables it. Select an enabled rendition from the latest catalog before opening a decoder. Keep one already playing while every rendition of its kind is disabled: the convenience decoders accept a disabled configuration, and a re-enable, such as an unmute, then plays without a resubscribe. Among the video renditions you can decode, prefer the highest `preference`; `0` is the default and `-1` marks a compatibility transcode.
- **Subscription defaults.** `Subscription.default` holds moq-net's defaults, whose `maxDelay` of 0 skips a superseded group at once; passing no subscription means the same. Copy it and change a field rather than starting from `Subscription()`, whose `nil` `maxDelay` sets no subscriber limit.
- **Track demand.** Obtain a dynamic track request’s `demand()` before accepting it to watch subscribers through the handoff to its producer. The watch does not keep the track alive and ends when the track closes or the request is rejected.

## Runtime

Native I/O and background tasks run on a Tokio worker pool. Calls awaited by the
host still poll in place; independent native wake callbacks can run concurrently.
The generated bindings resume Swift continuations. Use the appropriate actor
when updating UI state.
A panic in a native background task reaches the pending and later fallible calls
of every object that depends on it as a task error; other objects keep running.

A session closed for exceeding the relay's request caps reports the protocol
kind `ProtocolKind.tooManyRequests`. Reduce
the simultaneous announcements or subscriptions, or ask the relay operator
to adjust the [session limits](/bin/relay/config#session-and-peer), before
reconnecting with the same workload.
A session closed because its credential expired reports `ProtocolKind.expired`, and a request refused for an expired token raises `MoqError.Expired`; refresh the token before retrying. Malformed credentials report `ProtocolKind.malformedAuthToken`; correct the token before reconnecting with it.
A catalog that needs a newer moq-hang draft than this build reads raises `MoqError.UnsupportedCatalog`; upgrade the library to read it.

## Reference

- API source: [swift/Sources/Moq](https://github.com/moq-dev/moq/tree/main/swift/Sources/Moq)
- Source: [`swift/`](https://github.com/moq-dev/moq/tree/main/swift); `just swift check` builds and tests on a Mac
- Packages SPM resolves: [moq-dev/moq-swift](https://github.com/moq-dev/moq-swift), [moq-dev/moq-swift-ffi](https://github.com/moq-dev/moq-swift-ffi)
