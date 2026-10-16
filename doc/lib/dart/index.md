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

Import `package:moq/media.dart` with a prefix for catalogs, encoded-media imports, and
container consumers. `TrackProducer.audio` / `.video` take a broadcast, an init record,
and a `Named` or `Requested` target. `CatalogProducer(broadcast: ...)` owns catalog properties
and sections; it holds the broadcast weakly and fails after the broadcast closes.
`package:moq/flate.dart` exposes opaque `SnapshotProducer` / `StreamProducer` with an optional `timestampUs`.
A missing data timestamp stays untimed; the library never substitutes zero or now.
Raw tracks with a `null` timescale are untimed on both publish and receive.
Whole-frame media writes require caller time; container imports keep their own timeline handling.
Timed raw/data tracks require caller-supplied timestamps. Use `CatalogProducer.now()` to stamp a locally
created event on the broadcast clock; reading it fixes that clock for later media producers.
Share `media.Timebase(broadcast: ...)` through container init records when several
imports use one source timeline. `ContainerStreamProducer` takes `ContainerStreamInit`;
omitting its timebase keeps an independent offset. A timebase belongs to its broadcast
and does not keep it open.
`media.CatalogConsumer.subscribe`, `media.TrackConsumer.subscribe`, and `media.GroupConsumer.fetch`
construct the read side from a broadcast. For arrival-order reads, decode each group from
`TrackConsumer.recvGroup()` with `media.GroupConsumer(group: ..., container: ...)`; frames
carry container media time as they land.

```bash
dart pub add moq        # or: flutter pub add moq
```

```dart
import 'package:moq/media.dart' as media;
import 'package:moq/moq.dart';

final moq = await Moq.connect('https://relay.example.com');

// Subscribe. The stream is live, so listen to it rather than awaiting its end.
moq
    .announced(
      options: const AnnounceOptions(prefix: 'live/', filter: '*/camera'),
    )
    .updates()
    .listen((event) {
  // A restart is a new publisher run at the same path: request it again.
  switch (event) {
    case AnnounceEventStart(:final announce) ||
        AnnounceEventRestart(:final announce):
      print(announce.prefix);
      print(announce.captures);
    default:
  }
});
final broadcast = await moq.requestBroadcast('live/camera');
final catalog = await media.CatalogConsumer.subscribe(broadcast: broadcast);
print(await catalog.next());
```

```dart
// Publish. bytes comes from your encoder or application source.
final mine = moq.createBroadcast('live/camera');
final track = mine.publishTrack(name: 'video', info: null);
final group = track.appendGroup();
group.writeFrame(frame: Frame(payload: bytes, timestampUs: null));
group.finish();
mine.announce(route: MoqRoute(epoch: mintEpoch())); // a fresh epoch per run
track.finish();
mine.close();

await moq.close();
```

## Things to know

The rest of the [shared feature list](/lib/#what-every-binding-can-do) maps
one to one; the API reference has the names.

- **No codecs.** Unlike the other bindings, the published Dart binaries carry no encoder or decoder. Already-encoded frames flow through `media.TrackProducer` and `media.TrackConsumer`; encoding is up to `package:camera`, platform channels, or another codec package.
- **Names.** Types drop the `Moq` prefix (`Session`, `BroadcastProducer`) as aliases, so the generated names still work. `Container`, `Route`, and the exceptions keep it, since Flutter owns those names. `dynamic` is reserved, so the origin method is `dynamic_`.
- **Audio needs cuts.** Video groups at its keyframes, but audio forms a group only where you call `cut()`: after every frame, or at a segment cadence to align with video.
- **Live encoder timing.** After writing a frame you encoded yourself, call `flush(timestampUs: ...)` with the same timestamp so the catalog advertises your jitter. Skip it for file and network imports. On a seek or pause, call `discontinuity()`, then keep timestamps moving forward and resume video on a keyframe.
- **Closing.** `await moq.close()` or `session.shutdown()` drains for up to one second and reports any error. `session.cancel(code: 0)` closes at once. Finish or abort live tracks first. Cancelling a stream subscription releases its native cursor. Application delivery requires explicitly selecting `moq-lite-07-wip` on both peers; older Lite versions (including default lite-06) and IETF wait only for transport ACKs.
- **Stats.** `session.stats()` reports `rttUs`, `estimatedSendRateBps`, `estimatedRecvRateBps`, and the byte and packet counters (`bytesSent`, `bytesReceived`, `bytesLost`, `packetsSent`, `packetsReceived`, `packetsLost`). A field is `null` when the transport does not report it, which is not the same as zero.
- **Errors.** Read diagnostic payloads from `reason` and structured protocol failures from `details`.
- **Disabled renditions.** Audio and video convenience decoders reject a catalog configuration with `enabled: false`. Select an enabled rendition from the latest catalog before opening a decoder.
- **Track demand.** Obtain a dynamic track request’s `demand()` before accepting it to watch subscribers through the handoff to its producer. The watch does not keep the track alive and ends when the track closes or the request is rejected.

## Runtime

Native I/O and background tasks run on a Tokio worker pool. Calls awaited by the
host still poll in place; independent native wake callbacks can run concurrently.
The generated bindings deliver native wakeups to the Dart isolate. Keep UI work
on its owning isolate.
A panic in a native background task reaches the pending and later fallible calls
of every object that depends on it as a task error; other objects keep running.

A session closed for exceeding the relay's request caps reports the protocol
kind `ProtocolKind.tooManyRequests`. Reduce
the simultaneous announcements or subscriptions, or ask the relay operator
to adjust the [session limits](/bin/relay/config#session-and-peer), before
reconnecting with the same workload.
A session closed because its credential expired reports `ProtocolKind.expired`, and a request refused for an expired token raises `ExpiredMoqException`; refresh the token before retrying.

## Reference

- API reference: [pub.dev/documentation/moq](https://pub.dev/documentation/moq/latest/)
- Source: [`dart/`](https://github.com/moq-dev/moq/tree/main/dart)
- Packages: [moq](https://pub.dev/packages/moq), [moq\_ffi](https://pub.dev/packages/moq_ffi)
