# Migrating the Dart moq package

How to update your code for each breaking change, newest first.

## Unreleased

### JSON consumers return `Value` (#5677)

`SnapshotConsumer.next()` and `StreamConsumer.next()` return `Value` (`value`, `timestampUs`) instead of a string; `timestampUs` is null when untimed.
A snapshot consumer's `next()` now yields every state in order; call `latest()` for the newest.
Timestamps are rounded down to a whole microsecond.

### Ranges filter reads (#5929)

A subscription now reads only the groups its own `ranges` cover.

### Track demand reports the wanted group (#5930)

`TrackDemand.isUsed()`, `used()`, and `unused()` are gone.
`wanted()` returns null while nobody subscribes, otherwise the group subscribers want next, and `wantedChanged()` waits for it to change (the first call returns at once).
Loop on `wantedChanged()` until it returns non-null where you awaited `used()`, and null where you awaited `unused()`.
`TrackInfo` gains `refresh`. `GroupDemand` is unchanged.

### `CongestionControl` gained `none` (#5926)

`CongestionControl.none` runs no congestion control and no pacing, for tests and controlled links.
An exhaustive `switch` needs one more case.

### Key logging is opt-in (#5907)

`SSLKEYLOGFILE` alone no longer writes TLS session secrets.
Set `ConnectOptions.keylog` or `ListenOptions.keylog` as well to capture them; without `SSLKEYLOGFILE` the connect or listen fails.

### Subscriptions carry group ranges (#5732)

`Subscription.groupStart` and `groupEnd` are replaced by `ranges`, a list of `Range` whose ends are an `Anchor` (an absolute group and frame, or a count of groups before the latest), and an optional `order`.
To start at group `n`, pass one range starting at the absolute anchor for group `n`, frame 0.

### Receive estimates come from `Session.recvBandwidth` (#5777)

The connection stats no longer carry a receive estimate.
Hold the consumer `Session.recvBandwidth` returns to request PROBE, read it with `peek()` or wait with `changed()`, and `cancel()` it to stop.
Replace `stats().estimatedRecvRateBps` with `recvBandwidth()?.peek()`.

### `AnnouncedBroadcast.available()` waits for the next publisher instance (#5700)

The first call returns the broadcast serving the path, as before.
Each later call waits for the next instance to take over the path, instead of returning whatever serves it now.

### A timed track refuses a group that rewinds (#5803)

Writing a frame fails with `TimestampMismatch` when a group's first frame starts before a lower-sequence group's, or after a higher one's.
A publisher whose timeline restarts publishes a new broadcast instead.

### Bandwidth reserves a track's demand (#5845)

`Bandwidth.reserve` takes the track's `demand()` instead of the producer, and no longer throws.
Pass `demand: track.demand()` instead of `track: track`.

### Media subscribe and fetch take a catalog rendition (#5834)

`media.TrackConfig` and `media.GroupConfig` replace `name` and `container` with a `rendition` (`media.VideoRendition` or `media.AudioRendition`) and follow its broadcast and track themselves.

```dart
final config = media.TrackConfig(rendition: media.VideoRendition(id: id, config: video));
```

### Background panics are reported (#5562)

A panic in a background task throws `TaskMoqException` with the panic message as `reason`, and other objects keep running.

### Announcements are events with a restart

`Moq.announcements()` is `announced(...).updates()`, a `Stream<AnnounceEvent>`: `AnnounceEventStart`, `AnnounceEventUpdate`, `AnnounceEventEnd`, or `AnnounceEventRestart`, each carrying an `Announce`.
`AnnounceUpdate` is gone.
A restart means another publisher instance now serves the prefix: request the path again.

```dart
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

### The consume origin is `consume`

`ConnectOptions.subscribe` is `consume`.
A value the native side cannot use fails `Moq.connect` with `ConfigMoqException`, including a bad bind address.
A server request takes its origins in `accept(publish: ..., consume: ...)`, where null inherits the server's origin.

### Data tracks wrap a track

`BroadcastProducer.publishJsonSnapshot` / `publishJsonStream`, the binary variants, and the matching consumers are gone.
Create the track, then wrap it with `package:moq/json.dart` (or `package:moq/flate.dart` for opaque bytes).

```dart
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
final audio = media.TrackProducer.audio(broadcast: broadcast, target: media.Named(null), init: init);
```

### Container byte streams use `media.ContainerProducer` (#5919)

`publishContainerStream` has no separate importer: a container recovers its own framing, so `media.ContainerProducer` takes chunks split anywhere.
Pass empty `data` and write the stream through it.
`ContainerInit`'s optional `timebase` shares one source's offset across container imports.

```dart
final stream = media.ContainerProducer(broadcast: broadcast, init: media.ContainerInit(format: media.ContainerFormat.fmp4, data: Uint8List(0)));
```

### Timestamps are optional

`Frame.timestampUs` and `Datagram.timestampUs` are nullable, null on a frame read from an untimed track, and so are their `timestamp` getters.
A raw write to a timed track without a timestamp throws `TimestampMismatch` instead of going out at 0, and whole-frame media writes require one.
`media.CatalogProducer.now()` samples the broadcast clock for local-event stamps.
Pass `timestampUs` when writing to a timed track, and handle a null `timestamp` when reading.

### Subscriber staleness is `maxDelay`

`Subscription.maxAgeUs` / `maxAge` is `maxDelayUs` / `maxDelay`.
`TrackInfo.maxAge` keeps its name: it is the publisher's retention.

### Error payloads are named `reason`

Exceptions that carried a positional string (`Transport`, `Internal`, `Connect`, ...) name it `reason`.
Read `reason` instead of `v0`.

### `Transport` gains `webTransport`

A WebTransport session reports `MoqTransport.webTransport`; `quic` now means raw QUIC only, so an exhaustive `switch` needs one more case.
Match `MoqTransport.webTransport` wherever you matched `MoqTransport.quic`.

### `stalled` is `enabled` (#4915)

`media.Video.stalled` is `enabled`, defaulting to true with the meaning inverted, and `media.Audio` gains `enabled`.
Replace `!video.stalled` with `video.enabled`.

### Routes have one cost (#4661)

`MoqRoute.cold` is removed; drop the `cold` argument.

### `BroadcastProducer.finish()` is removed (#4152)

Call `close()` instead.
