# moq

Idiomatic Dart and Flutter bindings for Media over QUIC.

```dart
import 'package:moq/moq.dart';

final connection = await Moq.connect('https://relay.example.com');
final announced = connection.announced(
  options: const AnnounceOptions(prefix: 'live/', filter: '*/camera'),
);
await for (final event in announced.updates()) {
  // The listing reports each path once while it stays present, even as its
  // publisher restarts; `connection.follow(path)` hands out each new run, or
  // null once it goes offline.
  switch (event) {
    case AnnounceEventPresent(:final announce):
      // Prefix stays origin-relative; captures reports wildcard matches.
      print(announce.prefix);
      print(announce.captures);
    case AnnounceEventAbsent():
  }
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
final publication = server.createBroadcast('live');
publication.announce(route: AnnounceRoute()); // unannounced broadcasts are invisible
await for (final request in server.requests()) {
  final session = await request.accept();
  print(session.state().connects);
}
```

The package uses `Future` for asynchronous operations and `Stream` for
announcements, and `connect` / `listen` take an options struct like Rust does.
Types are spelled without the `Moq` prefix (`Session`, `BroadcastProducer`,
`Backoff`). Spans cross as a `Duration` (`stats.rtt`), and media times as an
exact `Timestamp` with `toDuration()`. The lower-level generated API
remains available through the re-exported `moq_ffi` package.

Flutter Web is not supported. Browser applications should use the TypeScript
packages under `@moq/*`.
