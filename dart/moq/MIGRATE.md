# Migrating the Dart moq package

How to update your code for each breaking change, newest first.

## Unreleased

### Compression moves to the track

`package:moq/flate.dart` is removed: publish opaque bytes on a `TrackProducer`.
`SnapshotConfig` and `WindowConfig` lose `compression`, and `StreamConfig` is removed. Publish the track with `MoqTrackInfo(compress: true)` instead.
`StreamProducer` drops its `config`, and `SnapshotConsumer`, `StreamConsumer`, and `WindowConsumer` take only the track.

### Paths with an empty segment are refused ([#6648](https://github.com/moq-dev/moq/pull/6648))

Publishing, serving, or requesting a broadcast at a path with a leading, trailing, or doubled `/` throws instead of collapsing it. Pass the canonical path, such as `room/cam`.

### Request handlers are `requests()` ([#6752](https://github.com/moq-dev/moq/pull/6752))

Rename `dynamic_()` on `OriginProducer`, `BroadcastProducer`, `TrackProducer`, and `TrackRequest` to `requests()`.
Their handles `OriginDynamic`, `BroadcastDynamic`, and `TrackDynamic` are `OriginRequests`, `BroadcastRequests`, and `TrackRequests`; their methods are unchanged.

### `Allocator` ([#6751](https://github.com/moq-dev/moq/pull/6751))

`Bandwidth` is `Allocator`, `bandwidth()` is `allocator()`, and `Reservation.grant()` is `rate()`. The producers' `bandwidth` option keeps its name.

### Decoders read gaps and ends

`audio.Consumer.next()` and `video.Consumer.next()` return a `Read` instead of a frame. Take `read.frame` from a `ReadFrame` where you used the frame; `ReadGap` marks lost media and `ReadEnd` where the publisher stopped its encoder, and what follows either does not continue what came before.

### Durations are `Duration` fields

The records carry durations as `Duration` fields instead of `*Us` microsecond integers, so the duration extension getters are gone: read `backoff.initial`, `stats.rtt`, `grant.expires`, `info.maxAge`, `fill.maxDelay()`, and `consumer.delay()` directly.
`audio.EncoderOutput.frameDuration` is nullable: pass null where you passed `frameDurationUs: 0` for the codec's own frame.
`Subscription.maxDelay` has no default, so a `MoqSubscription()` that omits it gets no staleness limit instead of moq-net's 0: pass `Duration.zero` for the old default.
A negative `Duration` throws `ArgumentError` wherever it crosses into the native library.

### Connection state

`Moq.connect` returns a connecting native session. Replace status, connect-count, and failure polls with `session.state()` and `session.stateChanged(after: state.revision)`. `ConnectionState` carries `phase`, `connects`, `failure`, and `version`; `ConnectionPhase` replaces `ConnectionStatus`.

### Exact encoder rates and publication options

Raw video input framerates are exact numerator/denominator records, not integers.
Use `video.FrameRate(numerator: 30000, denominator: 1001)` and pass `options: ProducerOptions(input: input, output: output, track: track, bandwidth: bandwidth)` to the matching audio/video producer.
The codec-only output records no longer contain track naming. Rebuild against
the matching generated bindings and native library.

### Video rendition preference

`media.Video` gains `preference`, defaulting to `0`. Upgrade the matching moq\_ffi package too. Higher values are preferred; compatibility transcodes use `-1`.

### `announce` takes an `AnnounceRoute`

`BroadcastProducer.announce(route:)` takes `AnnounceRoute(cost:, epoch:)` instead of a `MoqRoute`: the origin builds the hop chain, so `hops` is no longer an input.
`MoqRoute` is only what announcements report. To re-announce an observed route, copy its `cost` and `epoch`.

### Cancelling a stream cancels its pending read

`await subscription.cancel()` on announcements, authorization grants, and server requests now waits for the pending native operation to release, even when no event arrives.
Announcement and grant subscriptions own and release their consumer; cancelling a request subscription keeps its shared `Server` usable.
The generated async methods now return `UniffiRustFuture<T>`, a `Future<T>` with `cancel()`; existing `await` and `Future<T>` assignments work unchanged.
Custom implementations of generated interfaces must return `UniffiRustFuture<T>` instead of a plain `Future<T>`.

### A broadcast reference waits for its sibling's announcement (#6254)

Resolving a rendition's `broadcast` reference in the rendition-based subscribes and fetches now waits for a sibling nothing announces yet instead of failing as unroutable, so a late sibling plays once it is announced.
Where you skipped such a rendition on that error, bound the call yourself: add a `.timeout` to the future.

### Shutdown drains and `Moq.close()` is async

`session.shutdown()` still returns at once, but it now requests a draining close instead of cancelling.
`await session.closed()` waits for the drain and reports delivery failures; call `session.cancel(code: 0)` to close at once, as `shutdown()` did.
`Moq.close()` returns `Future<void>` and drains before stopping the client, so await it.

### Media track names belong to the target

Audio and video init records no longer carry a track name. Pass that name on the media producer's named target instead; init records describe only the codec and media.

### Broadcast `finish` is removed

`BroadcastProducer.finish()`, a deprecated alias for `close()`, is removed.
Call `BroadcastProducer.close()` where you called `finish()`.
To end the content first, call `CatalogProducer.finish()`; the broadcast stays up until `close()`.

### JSON snapshot `deltaRatio` is `deltas`

`SnapshotConfig.deltaRatio` is replaced by `bool deltas` (default `true`).
Pass `deltas: false` where you passed `deltaRatio: 0`, and drop the argument otherwise.
Publish the track with a `refresh` ratio so a joiner gets the value restated in a fresh group.

### Priorities are signed (#6142)

`TrackInfo.priority`, `Subscription.priority`, and `FetchGroupOptions.priority` take `-128..=127`, higher first, and 0 is the unset midpoint.
`TrackInfo.priority` defaulted to 127 and now defaults to 0.
To keep a value's moq-lite byte, subtract 128 from it: 255 becomes 127, 128 becomes 0, and 0 becomes -128.

### A client asks only about what it watches (#6165)

On moq-lite, a client session asks its relay for announcements only under paths something watches, through `announced()` or `announcedBroadcast()`, instead of under its whole scope at connect.
`requestBroadcast()` watches nothing, so on a client it fails with an unroutable error for a path nothing watches, even when the relay announces it.
Resolve the path with `announcedBroadcast()`, or hold `announced()` over its prefix while requesting.
moq-transport and moq-lite-03/04 sessions are unchanged.

### `ContainerFormat.fmp4` unwraps to Legacy (#6250)

A container import with `ContainerFormat.fmp4` now publishes one Legacy frame per sample instead of one CMAF frame per fragment.
Pass the new `ContainerFormat.cmaf` to keep publishing CMAF.

### Catalog references carry their epoch

`Video` and `Audio` renditions gain `epoch`, the publisher instance their `broadcast` reference names.
`BroadcastConsumer.resolve(reference:)` becomes `resolve(rendition:)`, taking the catalog `Rendition` and reading its `broadcast` and `epoch` from it.
Resolving, and the rendition-based consumers, throw for a rendition whose broadcast is proven to serve another epoch.

### Timestamps stop at 2^61 - 1 microseconds (#6168)

Writing a frame, or any other call that takes a timestamp, fails with `TimeOverflow` past `2^61 - 1` microseconds instead of `2^62 - 1`, the most moq-lite-05/06 can carry.
A received timestamp past it fails the group as a malformed track.
Clamp timestamps you build from unbounded input to `2^61 - 1`.

### Finished tracks stay until the broadcast ends (#6157)

A finished track now stays listed, with its catalog rendition and demand, until its broadcast closes.
Publish a replacement under a new name, and expect a finished track's demand watch to end with the broadcast.

### A claim answers by publishing (#5678)

`origin.dynamic_(prefix:)` takes no route, and `update` is gone.
Replace `request.accept(broadcast:)` with `request.prepare(epoch: mintEpoch())`, which returns a `BroadcastPrepared`: fill its `broadcast()`, then `publish()` announces it at the exact path under that epoch and returns its producer.

### Anchors can name a time (#6074)

`Anchor` gains a time variant, so exhaustive switches need a case for it.
`TrackConsumer.update` now throws on a time anchor an untimed track can't serve.

### Rebuild the matching raw bindings

The documentation update changes UniFFI's audio-producer method metadata checksums.
Update `moq_ffi` and its native library together; older generated guards reject the rebuilt library.
For a custom native library, regenerate its matching Dart bindings with `just dart generate`.
Application calls are unchanged.

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
`TrackInfo` gains `refresh`.

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
Hold the consumer `Session.recvBandwidth` returns to request PROBE, read it with `rate()` or wait with `rateChanged()`, and `cancel()` it to stop.
Replace `stats().estimatedRecvRateBps` with `recvBandwidth()?.rate()`.

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

An omitted timescale now publishes a raw track untimed instead of selecting microseconds. Explicitly set a microsecond timescale to preserve the released default.

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
