---
title: Swift
description: Async sequences for iOS and macOS via the Moq package
---

# Swift

[![Swift Package Index](https://img.shields.io/github/v/release/moq-dev/moq-swift?label=moq-swift)](https://github.com/moq-dev/moq-swift/releases)

The `Moq` Swift package: de-prefixed types, `AsyncSequence` on every
consumer, `Sendable` handles, and `Task` cancellation that reaches the native
side. It depends on `MoqFFI`, which ships a prebuilt XCFramework with arm64
slices for iOS 15+, the iOS Simulator, and macOS 12.3+.

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
let client = Client()
let session = try await client.connect(to: "https://relay.example.com")

for try await announcement in try session.consume.announced(prefix: "live/", filter: "*/camera") {
    // Updates stay origin-relative; captures reports what each wildcard matched.
    print(announcement.captures ?? [])
    let broadcast = try await session.consume.requestBroadcast(path: announcement.prefix)
    for try await catalog in try await broadcast.subscribeCatalog() {
        print(catalog)
    }
}
```

```swift
// Publish encoded frames, or raw pixels with the codec inside the binding (VideoToolbox).
// opusInit, packet, pts, and rgba come from your encoder or capture source.
let broadcast = try session.publish.createBroadcast(path: "my-stream.hang")
let audio = try broadcast.publishAudio(format: .opus, initData: opusInit)
try audio.writeFrame(packet, timestampUs: 20_000)

let video = try broadcast.encodeVideo(
    input: VideoEncoderInput(format: .rgba, width: 1280, height: 720, framerate: 30),
    output: VideoEncoderOutput(codec: .h264, track: "camera", kind: .auto)
)
try video.write(VideoFrame(timestampUs: pts, data: rgba))
try broadcast.announce()

session.shutdown()
```

For already-encoded live output, call `audio.flush(timestampUs:)` after `writeFrame` with the same broadcast-clock PTS. It measures catalog jitter at the transport handoff. File, pipe, and network imports should omit `flush`; built-in encoders observe their own output.

Call `audio.discontinuity()` when the source seeks, pauses, or changes its time base. It publishes a timeline marker and restarts handoff measurement without lowering advertised jitter. Resume with timestamps that continue forward on the broadcast media clock; this does not permit timestamp rewinds.

The three advertising operations: `session.publish.createBroadcast(path:)`
returns an unannounced producer, invisible to everyone; `broadcast.announce(route:)` /
`broadcast.unannounce()` own that exact-path advertisement;
`session.publish.dynamic(prefix:route:)` claims `prefix` and every path
beneath it (`""` for everything). Hold the returned `OriginDynamic` while the
claim should stay advertised, and reject the requests you will not serve. A
route is a capability, not an inventory. `announced(prefix:filter:)` combines a
literal root with an optional relative pattern; `announcement.prefix` stays
relative to the origin and `captures` reports what the wildcards matched.
Paths with a `.`-prefixed segment below the prefix are [hidden](/concept/moq-lite#hidden-broadcasts) unless
`hidden: true`.

For a self-signed relay on your own test network, `try client.setTlsVerify(false)`
accepts any certificate; prefer `setTlsRoots` or a fingerprint anywhere else.
Setters throw if a connect is in flight or after `cancel()`.

Sessions reconnect with backoff when the transport drops and re-announce local
broadcasts. `session.epoch()` counts the connections, 1 on the first, pairing
with `session.status()` to log each reconnect; `client.setBackoff` tunes the
pacing; and `client.setQuicMaxStreams` raises the peer's inbound stream cap.

The [WebSocket fallback](/concept/transport#websocket-fallback) races QUIC after
a 200 ms head start. `client.setWebsocketEnabled(false)` turns it off for a
QUIC-only relay, and `client.setWebsocketDelay(_:)` changes the head start, in
microseconds.

`Server` binds, generates or loads TLS, and hands you each request to
`accept()` or `reject(code:)`; `request.transport` is a `Transport` enum. JSON tracks take `Codable` types
(`publishJsonSnapshot(name:of:)`, `subscribeJsonStream(name:as:)`), and the
rest of the [shared feature list](/lib/#what-every-binding-can-do) maps one
to one: `fetchGroup`/`fetchMediaGroup`, `dynamic()` for tracks and `dynamic(prefix:)` for broadcasts, `appendDatagram`/
`datagrams`, `setCatalogSection`, `demand()` for `used()`/`unused()`. `session.bandwidth()`
divides the connection's send estimate; pass it to `encodeVideo` /
`encodeAudio` or `reserve` a share for an app-owned track. `MoqError.isAuth` and
`isShutdown` classify errors. `protocolError` is the structured protocol failure
(scope, verbatim code, kind) when the peer sent one.

`encodeAudio` encodes raw PCM inside the binding. Its codec is an object,
`AudioCodec.opus()`, and `AudioEncoderOutput.frameDurationUs` sets the Opus
frame length: 2500, 5000, 10000, 20000 (the default), 40000, or 60000.

`decodeVideo` picks the decoded CPU pixel layout: `VideoDecoderOutput.format`
is `.i420` when unset, or `.rgba` for four bytes a pixel, and every frame
repeats the layout it was decoded to. `resize` is best effort: only NVDEC has a
built-in scaler, and VideoToolbox is not it, so read each frame's own `width`
and `height` rather than assuming it took.

## Connection stats

`session.stats()` returns a `ConnectionStats` snapshot. Each field is `nil`
when the transport backend does not report it (native QUIC reports all of them;
browser WebTransport reports few or none) or before it is available, which is
not the same as zero.

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

- API reference: [Swift Package Index (DocC)](https://swiftpackageindex.com/moq-dev/moq-swift/documentation/moq)
- Source: [`swift/`](https://github.com/moq-dev/moq/tree/main/swift); `just swift check` builds and tests on a Mac
- Packages SPM resolves: [moq-dev/moq-swift](https://github.com/moq-dev/moq-swift), [moq-dev/moq-swift-ffi](https://github.com/moq-dev/moq-swift-ffi)
