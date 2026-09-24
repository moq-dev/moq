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

To serve instead of connect, `Server.listen` binds a socket and streams the
sessions that arrive:

```dart
final server = await Server.listen(
  options: const ListenOptions(
    bind: '127.0.0.1:4443',
    tlsGenerate: ['localhost'],
  ),
);
final broadcast = server.createBroadcast('live');
broadcast.announce(route: MoqRoute()); // unannounced broadcasts are invisible
await for (final request in server.requests()) {
  final session = await request.accept();
  print(session.epoch());
}
```

The package uses `Future` for asynchronous operations and `Stream` for
announcements, and `connect` / `listen` take an options struct like Rust does.
Types are spelled without the `Moq` prefix (`Session`, `BroadcastProducer`,
`Backoff`), and microsecond fields read back as a `Duration` (`stats.rtt`,
`frame.timestamp`). The lower-level generated API
remains available through the re-exported `moq_ffi` package.

Flutter Web is not supported. Browser applications should use the TypeScript
packages under `@moq/*`.
