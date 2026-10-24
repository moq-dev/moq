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

Import `package:moq/media.dart` with a prefix for catalogs, encoded-media imports, and
container consumers. `TrackProducer.audio` / `.video` take a broadcast, an init record,
and a `Named` or `Requested` target. `CatalogProducer(broadcast: ...)` owns catalog properties
and sections; it holds the broadcast weakly and fails after the broadcast closes.
`package:moq/flate.dart` exposes opaque `SnapshotProducer` / `StreamProducer` with an optional `timestampUs`.
A missing data timestamp stays untimed; the library never substitutes zero or now.
`package:moq/json.dart` consumers return `Value` (`value`, `timestampUs`) from `next()`; a snapshot consumer
yields each state in order, and `latest()` skips to the newest. Read timestamps are rounded down to a whole microsecond.
Raw tracks with a `null` timescale are untimed on both publish and receive.
`MoqTrackInfo.compress` on a raw track sends each group DEFLATE-compressed to opt-in `moq-lite-07-wip` peers; other peers, and every reader here, get the original frames. `@moq/net` cannot read a compressed track yet.
Whole-frame media writes require caller time; container imports keep their own timeline handling.
Timed raw/data tracks require caller-supplied timestamps. Use `CatalogProducer.now()` to stamp a locally
created event on the broadcast clock; reading it fixes that clock for later media producers.
Share `media.Timebase(broadcast: ...)` through `ContainerInit` when several imports use
one source timeline; omitting it keeps an independent offset. A timebase belongs to its
broadcast and does not keep it open. `ContainerProducer` takes chunks split anywhere,
so a raw byte stream needs no other importer.
`ContainerFormat.fmp4` splits each fragment into one frame per sample; `ContainerFormat.cmaf` republishes it whole.
`media.CatalogConsumer.subscribe`, `media.TrackConsumer.subscribe`, and `media.GroupConsumer.fetch`
construct the read side from a broadcast; the last two take a catalog `media.Rendition` and
follow its broadcast and track themselves. For arrival-order reads, decode each group from
`TrackConsumer.recvGroup()` with `media.GroupConsumer(group: ..., container: ...)`; frames
carry container media time as they land.
`package:moq/video.dart` and `package:moq/audio.dart` encode raw pixels and PCM inside the
binding: `video.Producer` / `audio.Producer` take a `CatalogProducer`, and
`video.Consumer.subscribe` / `audio.Consumer.subscribe` decode a catalog rendition.

```bash
dart pub add moq        # or: flutter pub add moq
```

```dart
import 'package:moq/media.dart' as media;
import 'package:moq/moq.dart';

final moq = await Moq.connect('https://relay.example.com');

// Subscribe. The stream is live, so listen to it rather than awaiting its end.
moq
    .announced(
      options: const AnnounceOptions(prefix: 'live/', filter: '*/camera'),
    )
    .updates()
    .listen((event) {
  // A restart is a new publisher run at the same path: request it again.
  switch (event) {
    case AnnounceEventStart(:final announce) ||
        AnnounceEventRestart(:final announce):
      print(announce.prefix);
      print(announce.captures);
    default:
  }
});
// Wait for a publisher at one path; call available() again for the next one.
final broadcast = await moq.announcedBroadcast('live/camera').available();
final catalog = await media.CatalogConsumer.subscribe(broadcast: broadcast);
print(await catalog.next());
```

```dart
// Publish raw pixels; the binding encodes them. rgba and pts come from your capture source.
import 'package:moq/video.dart' as video;

final camera = moq.createBroadcast('live/camera');
final encoder = video.Producer(
  catalog: media.CatalogProducer(broadcast: camera),
  options: video.ProducerOptions(
    input: video.EncoderInput(
      format: video.PixelFormat.rgba,
      width: 1280,
      height: 720,
      framerate: video.FrameRate(numerator: 30000, denominator: 1001),
    ),
    output: video.EncoderOutput(
      codec: video.Codec.h264,
      kind: video.AutoEncoder(),
    ),
    track: 'camera',
  ),
);
encoder.write(frame: video.Frame(timestampUs: pts, data: rgba));
camera.announce(route: AnnounceRoute(epoch: mintEpoch()));
encoder.finish();
```

```dart
// Decode a subscribed rendition. Dispose each frame promptly.
import 'package:moq/video.dart' as video;

final remote = await moq.announcedBroadcast('live/camera').available();
final renditions = await media.CatalogConsumer.subscribe(broadcast: remote);
final decoder = await video.Consumer.subscribe(
  broadcast: remote,
  id: 'camera',
  catalogVideo: (await renditions.next())!.video['camera']!,
  output: video.DecoderOutput(),
);
final frame = (await decoder.next())!;
print(frame.pixels(format: video.PixelFormat.rgba).length);
frame.dispose();
```

```dart
// Publish already-encoded frames. bytes comes from your encoder or application source.
final mine = moq.createBroadcast('live/camera');
final track = mine.publishTrack(name: 'video', info: null);
final group = track.appendGroup();
group.writeFrame(frame: Frame(payload: bytes, timestampUs: null));
group.finish();
mine.announce(route: AnnounceRoute(epoch: mintEpoch())); // a fresh epoch per run
track.finish();
mine.close();

await moq.close();
```

Raw video encoders take an exact numerator/denominator frame rate, such as 30000/1001. Producer options hold input, codec settings, track naming, and optional session bandwidth ownership; standalone encoders take only codec settings.

## Things to know

The rest of the [shared feature list](/lib/#what-every-binding-can-do) maps
one to one; the API reference has the names, and [Layers](/lib/layers)
shows which module holds each.

- **Names.** Types drop the `Moq` prefix (`Session`, `BroadcastProducer`) as aliases, so the generated names still work. `Container`, `Route`, and the exceptions keep it, since Flutter owns those names. `dynamic` is reserved, so the origin method is `dynamic_`.
- **Audio needs cuts.** Video groups at its keyframes, but audio forms a group only where you call `cut()`: after every frame, or at a segment cadence to align with video.
- **Timeline breaks.** On a seek or pause, call `discontinuity()`, then keep timestamps moving forward and resume video on a keyframe.
- **Decoded frames hold decoder buffers.** `dispose()` each frame from a `video.Consumer` promptly rather than waiting for the garbage collector, or the decoder stalls. `resize` is best effort, so read each frame's `width()` and `height()`.
- **Closing.** `await moq.close()` or `session.shutdown()` drains for up to one second and reports any error. `session.cancel(code: 0)` closes at once. Finish or abort live tracks first. Awaiting cancellation of an announcement, grant, or request subscription releases its pending native read. Announcements also release their cursor; authorization and server listeners leave their shared owner usable. Application delivery requires explicitly selecting `moq-lite-07-wip` on both peers; older Lite versions (including default lite-06) and IETF wait only for transport ACKs.
- **Ending cleanly.** When the input ends, finish every track, then call `broadcast.finish()`. The returned `BroadcastFinish` keeps the broadcast announced and serving the finished tracks for `linger`, their longest max age, so a subscriber still catching up can drain them. `await finish.wait()` completes once the linger passes, and `finish.close()` closes the broadcast at once, completing the wait. `broadcast.close()` stays the abrupt end. An unreferenced `BroadcastFinish` closes the broadcast only when the garbage collector reclaims it, possibly long after, so wait on or close it rather than dropping it.
- **Stats.** `session.stats()` reports `rttUs`, `estimatedSendRateBps`, and the byte and packet counters (`bytesSent`, `bytesReceived`, `bytesLost`, `packetsSent`, `packetsReceived`, `packetsLost`). A field is `null` when the transport does not report it, which is not the same as zero. `version` names the negotiated protocol, and while a client session reconnects `session.failure()` says why. Before the first connect, `client.failure()` says why the dial is retrying. Hold the owned `BandwidthConsumer` returned by `session.recvBandwidth()` to receive bandwidth estimates. `rate()` reads the current estimate in bits per second and `rateChanged()` waits for a new one. Release the consumer or call `cancel()` when finished; releasing the last consumer stops PROBE; the session and stats snapshots do not request it. Client consumers survive reconnects and report no estimate while disconnected.
- **Authorization.** On `moq-lite-07-wip` (opt in with `ConnectOptions(versions: ...)`) and on moq-transport draft-17+ with the [MoQ Auth extension](/draft/moq-auth), listen to `moq.auth().grants()` for the `Grant` the relay sent back, `null` until it answers and while a client session reconnects. `await auth().add(token: ...)` presents a fresh token without reconnecting, returns an `AuthToken` whose grant joins the union until `cancel()`, and is presented again on every reconnect. A refusal throws with the relay's code.
- **Errors.** Read diagnostic payloads from `reason` and structured protocol failures from `details`.
- **Disabled renditions.** Audio and video convenience decoders reject a catalog configuration with `enabled: false`. Select an enabled rendition from the latest catalog before opening a decoder. Among the video renditions you can decode, prefer the highest `preference`; `0` is the default and `-1` marks a compatibility transcode.
- **Track demand.** Obtain a dynamic track request’s `demand()` before accepting it to watch subscribers through the handoff to its producer. The watch does not keep the track alive and ends when the track closes or the request is rejected.

## Runtime

Native I/O and background tasks run on a Tokio worker pool. Calls awaited by the
host still poll in place; independent native wake callbacks can run concurrently.
The generated bindings deliver native wakeups to the Dart isolate. Keep UI work
on its owning isolate.
A panic in a native background task reaches the pending and later fallible calls
of every object that depends on it as a task error; other objects keep running.

A session closed for exceeding the relay's request caps reports the protocol
kind `ProtocolKind.tooManyRequests`. Reduce
the simultaneous announcements or subscriptions, or ask the relay operator
to adjust the [session limits](/bin/relay/config#session-and-peer), before
reconnecting with the same workload.
A session closed because its credential expired reports `ProtocolKind.expired`, and a request refused for an expired token raises `ExpiredMoqException`; refresh the token before retrying.
A catalog that needs a newer moq-hang draft than this build reads raises `UnsupportedCatalogMoqException`; upgrade the library to read it.

## Reference

- API reference: [pub.dev/documentation/moq](https://pub.dev/documentation/moq/latest/)
- Source: [`dart/`](https://github.com/moq-dev/moq/tree/main/dart)
- Packages: [moq](https://pub.dev/packages/moq), [moq\_ffi](https://pub.dev/packages/moq_ffi)
