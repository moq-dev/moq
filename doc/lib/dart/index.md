---
title: Dart and Flutter
description: Build native MoQ clients with Dart and Flutter
---

# Dart and Flutter

The [`moq`](https://pub.dev/packages/moq) package provides the recommended Dart API for Media over QUIC. It uses futures and streams over the generated [`moq_ffi`](https://pub.dev/packages/moq_ffi) package.

```yaml
dependencies:
  moq: ^0.1.0
```

```dart
import 'package:moq/moq.dart';

final moq = await Moq.connect('https://relay.example.com');
final broadcast = await moq.requestBroadcast('demo');

// Use the raw broadcast, track, group, and frame objects as needed.
print(broadcast);

moq.close();
```

The package's Native Assets hook supplies `moq-ffi` for Android, iOS, Linux, macOS, and Windows. Flutter web is not supported because it cannot load the native Rust library.

The published libraries target Android API 24 and iOS 16. Set your application's minimum at least that high; a lower deployment target links objects stamped for a newer OS.

## Codecs

Unlike the Swift and Kotlin bindings, the published Dart libraries are built without the `audio` and `video` features, so there is no `MoqAudioProducer`, `MoqVideoProducer`, or any other built-in encoder or decoder. Catalog and container types (`MoqAudio`, `MoqVideo`, `MoqMediaProducer`, `MoqMediaConsumer`) are present, so an application can carry already-encoded frames; it just has to do the encoding itself, via `package:camera`, platform channels, or another Dart codec package.

See the [Dart API guide](/lib/dart/moq) for announcements and publishing.
