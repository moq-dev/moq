---
title: Dart and Flutter
description: Futures and streams for Flutter via the moq package
---

# Dart and Flutter

[![pub.dev](https://img.shields.io/pub/v/moq)](https://pub.dev/packages/moq)

The [`moq`](https://pub.dev/packages/moq) package on pub.dev wraps the
generated [`moq_ffi`](https://pub.dev/packages/moq_ffi) bindings in Dart
futures and streams. A Native Assets hook supplies the Rust core for Android
(API 24+), iOS (16+), Linux, macOS, and Windows. Flutter web is not supported,
since it can't load a native library.

Media frames use `keyframe` to mark a group start or a video keyframe. For audio,
it is true only on the first frame of each group, even when every sample can be
decoded independently.

```bash
dart pub add moq        # or: flutter pub add moq
```

```dart
import 'package:moq/moq.dart';

final moq = await Moq.connect('https://relay.example.com');

// Subscribe. The stream is live, so listen to it rather than awaiting its end.
moq.announcements(
  options: const AnnounceOptions(prefix: 'live/', filter: '*/camera'),
).listen((announcement) {
  print(announcement.prefix());
  print(announcement.captures());
});
final broadcast = await moq.requestBroadcast('live/camera');
```

```dart
// Publish. bytes comes from your encoder or application source.
final mine = moq.createBroadcast('live/camera');
final track = mine.publishTrack(name: 'video', info: null);
track.appendGroup().writeFrame(frame: Frame(payload: bytes));
mine.announce(route: MoqRoute());

moq.close();
```

```dart
// Serve. Server.listen binds the socket and streams the sessions that arrive.
final server = await Server.listen(
  options: const ListenOptions(
    bind: '127.0.0.1:4443',
    tlsGenerate: ['localhost'],
  ),
);
final live = server.createBroadcast('live/camera');
live.announce(route: MoqRoute()); // unannounced broadcasts are invisible
await for (final request in server.requests()) {
  final session = await request.accept();
  print(session.epoch());
}
```

The three advertising operations: `moq.createBroadcast(path)` (or
`origin.createBroadcast`) returns an unannounced producer, invisible to everyone;
`broadcast.announce(route:)` / `broadcast.unannounce()` own that exact-path
advertisement; `origin.dynamic_(prefix:, route:)` claims `prefix` and
every path beneath it (`''` for everything; Dart spells the origin method
`dynamic_` because `dynamic` is reserved). Hold the returned handle while the
claim should stay advertised, and reject the requests you will not serve. A
route is a capability, not an inventory. `announcements(options:)` takes a
literal prefix plus an optional relative pattern; `announcement.prefix()`
stays origin-relative and `captures()` reports the wildcard matches. Paths with
a `.`-prefixed segment below the prefix are [hidden](/concept/moq-lite#hidden-broadcasts) unless `hidden: true`.

Sessions reconnect with backoff when the transport drops and re-announce local
broadcasts. `Moq.connect` and `Server.listen` take a `ConnectOptions` /
`ListenOptions` struct, like Rust: `reconnect: false` makes the dial one-shot
and `backoff:` re-paces the retries. `moq.epoch` counts the connections, 1 on the first, pairing with
`session.status()` to log each reconnect; `maxStreams` raises the peer's
inbound stream cap for a subscriber to many tracks.

The [WebSocket fallback](/concept/transport#websocket-fallback) races QUIC after
a 200 ms head start. `websocketEnabled: false` turns it off for a QUIC-only
relay, and a `websocketDelay` `Duration` changes the head start.

Types are spelled without the `Moq` prefix (`Session`, `BroadcastProducer`,
`Backoff`); the generated names stay valid, since these are aliases rather than
wrappers. `Container`, `Route`, and the exceptions keep theirs, because
`Container` and `Route` are Flutter's. Microsecond fields read back as a
`Duration`: `stats.rtt`, `backoff.initial`, `frame.timestamp`.

Cancelling a stream releases the native cursor. The package re-exports
`moq_ffi`, so the full generated API is available without a second import.
Generated configuration setters throw if a connect, listen, or accept is in
flight, or after `cancel()`. Incoming requests report a `MoqTransport` enum.
`ProtocolMoqException` carries a `MoqProtocolException` as `details` (scope, verbatim
code, kind) when the peer sent a session or stream code.

`moq.bandwidth()` divides the connection's send estimate; `reserve` a share
for an app-owned encoder so several publishers on one session split the
uplink instead of each targeting the whole thing.

Unlike the other bindings, the published Dart binaries carry **no codecs**:
catalog and container types are there, so already-encoded frames flow through
`MoqMediaProducer`/`MoqMediaConsumer`, but encoding is up to
`package:camera`, platform channels, or another codec package.

`MediaProducer.flush(timestampUs: ...)` records the handoff of a locally encoded frame on the broadcast media clock. Call it after `writeFrame` only for live encoder output; file, pipe, and network imports stay clock-free. `MediaProducer` aliases the generated FFI object, so its method is available directly.

## Connection stats

`session.stats()` returns a `ConnectionStats` snapshot. Each field is `null`
when the transport backend does not report it (native QUIC reports all of them;
browser WebTransport reports few or none) or before it is available, which is
not the same as zero. `rttUs` is microseconds; the `rtt` extension reads it as a
`Duration`.

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

- Source: [`dart/`](https://github.com/moq-dev/moq/tree/main/dart)
- Packages: [moq](https://pub.dev/packages/moq), [moq\_ffi](https://pub.dev/packages/moq_ffi)
