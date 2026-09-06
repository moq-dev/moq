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
moq.announcements(prefix: 'live/').listen((announcement) {
  print(announcement.path());
});
final broadcast = await moq.requestBroadcast('live/camera');
```

```dart
// Publish. bytes comes from your encoder or application source.
final mine = moq.createBroadcast('live/camera');
final track = mine.publishTrack(name: 'video', info: null);
track.appendGroup().writeFrame(frame: MoqFrame(payload: bytes));

moq.close();
```

Cancelling a stream releases the native cursor. The package re-exports
`moq_ffi`, so the full generated API is available without a second import.

Unlike the other bindings, the published Dart binaries carry **no codecs**:
catalog and container types are there, so already-encoded frames flow through
`MoqMediaProducer`/`MoqMediaConsumer`, but encoding is up to
`package:camera`, platform channels, or another codec package.

- Source: [`dart/`](https://github.com/moq-dev/moq/tree/main/dart)
- Packages: [moq](https://pub.dev/packages/moq), [moq\_ffi](https://pub.dev/packages/moq_ffi)
