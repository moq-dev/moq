# moq

Idiomatic Dart and Flutter bindings for Media over QUIC.

```dart
import 'package:moq/moq.dart';

final connection = await Moq.connect('https://relay.example.com');
await for (final announcement in connection.announcements(
  options: const AnnounceOptions(prefix: 'live/', filter: '*/camera'),
)) {
  // Prefix stays origin-relative; captures reports wildcard matches.
  print(announcement.prefix());
  print(announcement.captures());
}
```

The package uses `Future` for asynchronous operations and `Stream` for
announcements. The lower-level generated API remains available through the
re-exported `moq_ffi` package.

Flutter Web is not supported. Browser applications should use the TypeScript
packages under `@moq/*`.
