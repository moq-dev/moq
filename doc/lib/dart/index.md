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

See the [Dart API guide](/lib/dart/moq) for announcements and publishing.
