# Migrating the Dart moq package

How to update your code for each breaking change, newest first.

## Unreleased

### Bandwidth reserves a track's demand (#5845)

`Bandwidth.reserve` takes the track's `demand()` instead of the producer, and no longer throws.

```dart
// Before
final reservation = bandwidth.reserve(track: track, maxBps: maxBps);
// After
final reservation = bandwidth.reserve(demand: track.demand(), maxBps: maxBps);
```

### Media subscribe and fetch take a catalog rendition (#5834)

`media.TrackConfig` and `media.GroupConfig` replace `name` and `container` with a `rendition` (`media.VideoRendition` or `media.AudioRendition`) and follow its broadcast and track themselves.

```dart
// Before
final config = media.TrackConfig(name: id, container: video.container);
// After
final config = media.TrackConfig(rendition: media.VideoRendition(id: id, config: video));
```

### Background panics are reported (#5562)

A panic in a background task throws `TaskMoqException` with the panic message as `reason`, and other objects keep running.

### Announcements are events with a restart

`Moq.announcements()` is `announced(...).updates()`, a `Stream<AnnounceEvent>`: `AnnounceEventStart`, `AnnounceEventUpdate`, `AnnounceEventEnd`, or `AnnounceEventRestart`, each carrying an `Announce`.
`AnnounceUpdate` is gone.
A restart means another publisher instance now serves the prefix: request the path again.

```dart
// Before
moq.announcements().listen((update) {
  if (update.active()) subscribe(update.prefix());
});
// After
moq.announced().updates().listen((event) {
  switch (event) {
    case AnnounceEventStart(:final announce) ||
        AnnounceEventRestart(:final announce):
      subscribe(announce.prefix);
    default:
  }
});
```

### "Epoch" means only the publisher epoch (#5146)

`Moq.epoch` is `connects`.

```dart
// Before
final n = moq.epoch;
// After
final n = moq.connects;
```

### The consume origin is `consume`

`ConnectOptions.subscribe` is `consume`.
A value the native side cannot use fails `Moq.connect` with `ConfigMoqException`, including a bad bind address.
A server request takes its origins in `accept(publish: ..., consume: ...)`, where null inherits the server's origin.

```dart
// Before
final moq = await Moq.connect(url, options: ConnectOptions(subscribe: origin));
// After
final moq = await Moq.connect(url, options: ConnectOptions(consume: origin));
```

### Data tracks wrap a track

`BroadcastProducer.publishJsonSnapshot` / `publishJsonStream`, the binary variants, and the matching consumers are gone.
Create the track, then wrap it with `package:moq/json.dart` (or `package:moq/flate.dart` for opaque bytes).

```dart
// Before
final state = broadcast.publishJsonSnapshot(name: 'state', config: JsonSnapshotConfig());
// After
final state = moq_json.SnapshotProducer(
  broadcast: broadcast,
  track: broadcast.publishTrack(name: 'state', info: null),
  config: moq_json.SnapshotConfig(),
);
```

### Media lives in `package:moq/media.dart`

`BroadcastProducer.publishAudio`, `publishVideo`, `publishContainer`, their `OnTrack` and `Stream` variants, `setVideoProperties`, and `setCatalogSection`, and the consumer's catalog and media subscriptions, are gone, along with the `MediaProducer`, `MediaConsumer`, and `MediaGroupConsumer` aliases.
Use `media.TrackProducer.audio` / `video`, `ContainerProducer`, `CatalogProducer`, `CatalogConsumer.subscribe`, `TrackConsumer.subscribe`, and `GroupConsumer.fetch`.
Producers drop `name`, `used`, and `unused`; read them through `demand()`.

```dart
// Before
final audio = broadcast.publishAudio(init: init);
// After
final audio = media.TrackProducer.audio(broadcast: broadcast, target: media.Named(null), init: init);
```

### Container byte-stream importers take an init record

`media.ContainerStreamProducer` takes a `ContainerStreamInit` holding the format instead of a bare format.
Its optional `timebase`, also on `ContainerInit`, shares one source's offset across container imports.

```dart
// Before
final producer = media.ContainerStreamProducer(broadcast: broadcast, format: media.ContainerFormat.fmp4);
// After
final producer = media.ContainerStreamProducer(broadcast: broadcast, init: media.ContainerStreamInit(format: media.ContainerFormat.fmp4));
```

### Timestamps are optional

`Frame.timestampUs` and `Datagram.timestampUs` are nullable, null on a frame read from an untimed track, and so are their `timestamp` getters.
A raw write to a timed track without a timestamp throws `TimestampMismatch` instead of going out at 0, and whole-frame media writes require one.
`media.CatalogProducer.now()` samples the broadcast clock for local-event stamps.

```dart
// Before
track.writeFrame(frame: Frame(payload: payload));
final Duration ts = frame.timestamp;
// After
track.writeFrame(frame: Frame(payload: payload, timestampUs: pts));
final Duration? ts = frame.timestamp;
```

### Subscriber staleness is `maxDelay`

`Subscription.maxAgeUs` / `maxAge` is `maxDelayUs` / `maxDelay`.
`TrackInfo.maxAge` keeps its name: it is the publisher's retention.

```dart
// Before
final sub = Subscription(maxAgeUs: 500000);
// After
final sub = Subscription(maxDelayUs: 500000);
```

### Error payloads are named `reason`

Exceptions that carried a positional string (`Transport`, `Internal`, `Connect`, ...) name it `reason`.

```dart
// Before
final reason = (error as TransportMoqException).v0;
// After
final reason = (error as TransportMoqException).reason;
```

### `Transport` gains `webTransport`

A WebTransport session reports `MoqTransport.webTransport`; `quic` now means raw QUIC only, so an exhaustive `switch` needs one more case.

```dart
// Before
case MoqTransport.quic:
// After
case MoqTransport.quic || MoqTransport.webTransport:
```

### `stalled` is `enabled` (#4915)

`media.Video.stalled` is `enabled`, defaulting to true with the meaning inverted, and `media.Audio` gains `enabled`.

```dart
// Before
final live = !video.stalled;
// After
final live = video.enabled;
```

### Routes have one cost (#4661)

`MoqRoute.cold` is removed.

```dart
// Before
final route = MoqRoute(cost: 10, cold: 20);
// After
final route = MoqRoute(cost: 10);
```

### `BroadcastProducer.finish()` is removed (#4152)

```dart
// Before
broadcast.finish();
// After
broadcast.close();
```
