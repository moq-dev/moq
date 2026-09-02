---
title: moq
description: Connect, announce, and subscribe from Dart
---

# `moq`

## Connect

`Moq.connect` accepts the relay URL and optional TLS, bind, publish, and subscribe settings.

```dart
final moq = await Moq.connect(
  'https://localhost:4443',
  tlsVerify: false,
);
```

Call `close()` when the connection is no longer needed.

## Consume announcements

Announcements are exposed as a Dart stream. Canceling the stream releases the underlying UniFFI cursor.

```dart
await for (final announcement in moq.announcements(prefix: 'live/')) {
  print(announcement.path());
}
```

Use `requestBroadcast(path)` when the broadcast path is already known, or `announcedBroadcast(path)` when it may arrive later.

## Publish

Create a broadcast through the connected session, then use the generated producer types for tracks, groups, and frames.

```dart
final broadcast = moq.createBroadcast('live/camera');
final track = broadcast.publishTrack(name: 'video', info: null);
final group = track.appendGroup();
group.writeFrame(frame: MoqFrame(payload: Uint8List.fromList(bytes)));
```

The package re-exports `moq_ffi`, so the complete generated API remains available without a second import.
