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

```bash
dart pub add moq        # or: flutter pub add moq
```

```dart
import 'package:moq/moq.dart';

final moq = await Moq.connect('https://relay.example.com');

// Subscribe. The stream is live, so listen to it rather than awaiting its end.
moq.announcements(
  options: const AnnounceOptions(prefix: 'live/', filter: '*/camera'),
).listen((event) {
  if (event is AnnounceEventStart) {
    print(event.announce.prefix);
    print(event.announce.captures);
  }
});
final broadcast = await moq.requestBroadcast('live/camera');
final catalog = await broadcast.subscribeCatalog();
print(await catalog.next());
```

```dart
// Publish. bytes comes from your encoder or application source.
final mine = moq.createBroadcast('live/camera');
final track = mine.publishTrack(name: 'video', info: null);
final group = track.appendGroup();
group.writeFrame(frame: Frame(payload: bytes));
group.finish();
mine.announce(route: MoqRoute());
track.finish();
mine.close();

await moq.close();
```

## Things to know

The rest of the [shared feature list](/lib/#what-every-binding-can-do) maps
one to one; the API reference has the names.

- **No codecs.** Unlike the other bindings, the published Dart binaries carry no encoder or decoder. Already-encoded frames flow through `MediaProducer` and `MediaConsumer`; encoding is up to `package:camera`, platform channels, or another codec package.
- **Names.** Types drop the `Moq` prefix (`Session`, `BroadcastProducer`) as aliases, so the generated names still work. `Container`, `Route`, and the exceptions keep it, since Flutter owns those names. `dynamic` is reserved, so the origin method is `dynamic_`.
- **Audio needs cuts.** Video groups at its keyframes, but audio forms a group only where you call `cut()`: after every frame, or at a segment cadence to align with video.
- **Live encoder timing.** After writing a frame you encoded yourself, call `flush(timestampUs: ...)` with the same timestamp so the catalog advertises your jitter. Skip it for file and network imports. On a seek or pause, call `discontinuity()`, then keep timestamps moving forward and resume video on a keyframe.
- **Closing.** `await moq.close()` or `session.shutdown()` gives finished tracks up to one second to deliver and fails if they did not. `session.cancel(code: 0)` closes at once. Finish or abort live tracks first. Cancelling a stream subscription releases its native cursor.
- **Stats.** `session.stats()` reports `rttUs`, `estimatedSendRateBps`, `estimatedRecvRateBps`, and the byte and packet counters (`bytesSent`, `bytesReceived`, `bytesLost`, `packetsSent`, `packetsReceived`, `packetsLost`). A field is `null` when the transport does not report it, which is not the same as zero.

## Reference

- API reference: [pub.dev/documentation/moq](https://pub.dev/documentation/moq/latest/)
- Source: [`dart/`](https://github.com/moq-dev/moq/tree/main/dart)
- Packages: [moq](https://pub.dev/packages/moq), [moq\_ffi](https://pub.dev/packages/moq_ffi)
