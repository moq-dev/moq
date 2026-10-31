import 'dart:convert';
import 'dart:io' show Platform;
import 'dart:typed_data';

import 'package:moq/audio.dart' as moq_audio;
import 'package:moq/media.dart' as moq_media;
import 'package:moq/json.dart' as moq_json;
import 'package:moq/moq.dart';
import 'package:moq/video.dart' as moq_video;
import 'package:test/test.dart';

const timeout = Duration(seconds: 10);

/// The next announce event.
Future<AnnounceEvent> nextRoute(AnnounceConsumer announced) async {
  final event = await announced.next().timeout(timeout);
  if (event == null) throw StateError('announce stream ended');
  return event;
}

void main() {
  test('container timebase belongs to one broadcast', () {
    final owner = BroadcastProducer();
    final other = BroadcastProducer();
    final timebase = moq_media.Timebase(broadcast: owner);
    expect(
      () => moq_media.ContainerProducer(
        broadcast: other,
        init: moq_media.ContainerInit(
          format: moq_media.ContainerFormat.fmp4,
          data: Uint8List(0),
          timebase: timebase,
        ),
      ),
      throwsA(isA<CodecMoqException>()),
    );
    owner.close();
    expect(
      () => moq_media.Timebase(broadcast: owner),
      throwsA(isA<ClosedMoqException>()),
    );
    other.close();
  });

  test('catalog handle closes with its broadcast', () {
    final broadcast = BroadcastProducer();
    final catalog = moq_media.CatalogProducer(broadcast: broadcast);
    catalog.setSection(name: 'app', json: '{"value":42}');
    catalog.now();
    broadcast.close();
    expect(
      () => catalog.removeSection(name: 'app'),
      throwsA(isA<ClosedMoqException>()),
    );
    expect(() => catalog.now(), throwsA(isA<ClosedMoqException>()));
  });

  test('raw writes without timestamps stay untimed', () async {
    final broadcast = BroadcastProducer();
    final track = broadcast.publishTrack(name: 'untimed', info: null);
    final consumer = track.consume(subscription: null);
    track.writeFrame(frame: Frame(payload: utf8.encode('payload')));
    final frame = await consumer.readFrame().timeout(timeout);
    expect(frame?.timestampUs, isNull);
    expect(consumer.info().timescale, isNull);
    track.finish();
    broadcast.close();
  });

  test('a default subscription skips superseded groups at once', () {
    expect(defaultSubscription().maxDelay, Duration.zero);
    expect(Subscription().maxDelay, isNull);
  });

  test('a negative duration in a raw record is refused', () {
    final broadcast = BroadcastProducer();
    final track = broadcast.publishTrack(name: 'negative', info: null);
    expect(
      () => track.consume(
        subscription: Subscription(maxDelay: const Duration(microseconds: -1)),
      ),
      throwsArgumentError,
    );
    broadcast.close();
  });

  test(
    'an ascending subscription reads a cached backlog oldest first',
    () async {
      final broadcast = BroadcastProducer();
      final track = broadcast.publishTrack(
        name: 'backlog',
        info: TrackInfo(timescale: 1000000),
      );
      for (var ms = 0; ms < 3; ms++) {
        final group = track.appendGroup();
        group.writeFrame(
          frame: Frame(payload: utf8.encode('x'), timestampUs: ms * 1000),
        );
        group.finish();
      }

      final consumer = track.consume(
        subscription: Subscription(
          maxDelay: const Duration(seconds: 10),
          ranges: [Range(start: RelativeMoqAnchor(2))],
          order: Order.ascending,
        ),
      );
      final sequences = <int>[];
      for (var i = 0; i < 3; i++) {
        final group = await consumer.recvGroup().timeout(timeout);
        sequences.add(group!.sequence());
      }
      expect(sequences, [0, 1, 2]);
      broadcast.close();
    },
  );

  test('both sides read the grant their peer gave them', () async {
    const version = ['moq-lite-07-wip'];
    final server = MoqServer(
      config: MoqServerConfig(
        bind: '127.0.0.1:0',
        versions: version,
        tls: MoqServerTls(generate: ['localhost']),
        quic: MoqQuicConfig(),
      ),
    );
    final address = await server.listen().timeout(timeout);
    final accepted = () async {
      final request = await server.accept().timeout(timeout);
      if (request == null) throw StateError('server closed before accept');
      return request.accept().timeout(timeout);
    }();

    final client = await Moq.connect(
      'https://$address',
      options: ConnectOptions(
        tls: ClientTls(insecure: true),
        bind: '127.0.0.1:0',
        versions: version,
        once: true,
      ),
    ).timeout(timeout);
    final serverSession = await accepted;

    // Generated records compare by identity, so compare the fields.
    void expectEverything(Grant? grant) {
      expect(grant?.publish, ['**']);
      expect(grant?.subscribe, ['**']);
      expect(grant?.expires, isNull);
    }

    expectEverything(
      await client
          .grants()
          .updates()
          .firstWhere((g) => g != null)
          .timeout(timeout),
    );
    expectEverything(
      await serverSession
          .grants()
          .updates()
          .firstWhere((g) => g != null)
          .timeout(timeout),
    );
    expect(serverSession.tokens(), isNull);

    // A peer that verifies no tokens of its own refuses another one.
    client.tokens().set_(tokens: ['extra']);
    await expectLater(
      client.tokens().answered().timeout(timeout),
      throwsA(isA<UnsupportedMoqException>()),
    );
    expectEverything(client.grants().grant());

    // The client drains first: cancelling the server session would fail its shutdown.
    await client.close();
    serverSession.cancel(code: 0);
    server.cancel();
  });

  test('connects, announces, subscribes, and delivers a frame', () async {
    final relay = MoqOriginProducer(config: MoqOriginConfig());
    final server = MoqServer(
      config: MoqServerConfig(
        bind: '127.0.0.1:0',
        tls: MoqServerTls(generate: ['localhost']),
        quic: MoqQuicConfig(),
        publish: relay,
        consume: relay,
      ),
    );
    final address = await server.listen().timeout(timeout);

    final accepted = () async {
      final request = await server.accept().timeout(timeout);
      if (request == null) throw StateError('server closed before accept');
      return request.accept().timeout(timeout);
    }();

    final client = await Moq.connect(
      'https://$address',
      options: ConnectOptions(
        tls: ClientTls(insecure: true),
        bind: '127.0.0.1:0',
      ),
    ).timeout(timeout);
    final serverSession = await accepted;
    expect(client.allocator(), isA<MoqAllocator>());

    final announcement = client.announced().updates().firstWhere(
      (event) => event is AnnounceEventStart,
    );
    final publication = relay.createBroadcast(path: 'live');
    final track = publication.broadcast().publishTrack(
      name: 'events',
      info: TrackInfo(timescale: 1000000),
    );
    publication.announce(route: AnnounceRoute());
    final announced =
        (await announcement.timeout(timeout) as AnnounceEventStart).announce;
    expect(announced.prefix, 'live');

    final requested = await client.request(announced.prefix).timeout(timeout);
    final consumer = await requested
        .subscribeTrack(name: 'events', subscription: null)
        .timeout(timeout);

    // Routed subscriptions pull their source lazily when the consumer is first read.
    final nextGroup = consumer.nextGroup();
    final demand = track.demand();
    while (await demand.wantedChanged().timeout(timeout) == null) {}

    final producer = track.appendGroup();
    producer.writeFrame(
      frame: MoqFrame(payload: utf8.encode('dart round trip'), timestampUs: 0),
    );
    producer.finish();

    final group = await nextGroup.timeout(timeout);
    final frame = await group?.readFrame().timeout(timeout);
    expect(frame, isNotNull);
    expect(utf8.decode(frame!.payload), 'dart round trip');

    await client.close();
    serverSession.cancel(code: 0);
    server.cancel();
  });

  test('Server.listen serves a broadcast to a connected client', () async {
    final server = await Server.listen(
      options: const ListenOptions(
        bind: '127.0.0.1:0',
        tlsGenerate: ['localhost'],
        versions: ['moq-lite-07-wip'],
      ),
    ).timeout(timeout);
    expect(server.certFingerprints(), isNotEmpty);

    final accepted = () async {
      final request = await server.requests().first.timeout(timeout);
      // The wrapper names itself ahead of moq-ffi, so a server can tell bindings apart.
      expect(request.peerImplementation(), startsWith('moq-dart/'));
      return request.accept().timeout(timeout);
    }();

    // A one-shot QUIC-only dial with explicit pacing: every knob reaches the
    // FFI client, and QUIC alone still connects.
    final client = await Moq.connect(
      'https://${server.localAddr}',
      options: ConnectOptions(
        tls: ClientTls(insecure: true, hostName: 'localhost'),
        bind: '127.0.0.1:0',
        versions: const ['moq-lite-07-wip'],
        timeout: Duration(seconds: 5),
        race: Duration.zero,
        resolutionDelay: Duration.zero,
        quic: QuicConfig(
          idleTimeout: Duration(seconds: 5),
          keepAlive: Duration.zero,
          gso: false,
          mtuDiscovery: true,
          congestionControl: CongestionControl.loss,
          keylog: false,
        ),
        once: true,
        backoff: Backoff(
          initial: const Duration(milliseconds: 1),
          max: const Duration(milliseconds: 2),
          timeout: const Duration(milliseconds: 3),
        ),
        websocket: WebSocketConfig(enabled: false, delay: Duration.zero),
      ),
    ).timeout(timeout);
    final serverSession = await accepted;

    final announcement = client.announced().updates().firstWhere(
      (event) => event is AnnounceEventStart,
    );
    final publication = server.createBroadcast('live');
    final broadcast = publication.broadcast();
    final track = broadcast.publishTrack(name: 'events', info: null);
    publication.announce(route: AnnounceRoute());
    final announced =
        (await announcement.timeout(timeout) as AnnounceEventStart).announce;
    expect(announced.prefix, 'live');

    await client.close();
    serverSession.cancel(code: 0);
    track.finish();
    broadcast.close();
    broadcast.close(); // a second close is a no-op
    server.close();
  });

  test('a negative websocket delay is refused before dialing', () {
    expect(
      Moq.connect(
        'https://localhost',
        options: const ConnectOptions(
          websocket: WebSocketConfig(delay: Duration(microseconds: -1)),
        ),
      ),
      throwsArgumentError,
    );
  });

  test('an invalid config fails before dialing', () {
    expect(
      Moq.connect(
        'https://localhost',
        options: const ConnectOptions(versions: ['moq-lite-99']),
      ),
      throwsA(isA<ConfigMoqException>()),
    );
  });

  test(
    'keylog without SSLKEYLOGFILE fails the listen',
    () {
      expect(
        Server.listen(
          options: const ListenOptions(
            bind: '127.0.0.1:0',
            tlsGenerate: ['localhost'],
            keylog: true,
          ),
        ),
        throwsA(isA<ConfigMoqException>()),
      );
    },
    // dart:io cannot unset a variable, so a run that inherits one skips.
    skip: (Platform.environment['SSLKEYLOGFILE'] ?? '').isNotEmpty
        ? 'SSLKEYLOGFILE is set'
        : false,
  );

  test('closing a server releases its port', () async {
    final first = await Server.listen(
      options: const ListenOptions(
        bind: '127.0.0.1:0',
        tlsGenerate: ['localhost'],
      ),
    ).timeout(timeout);
    final addr = first.localAddr;
    first.close();

    // No retry: close() released the listening socket before returning.
    final second = await Server.listen(
      options: ListenOptions(bind: addr, tlsGenerate: ['localhost']),
    ).timeout(timeout);
    expect(second.localAddr, addr);
    second.close();
  });

  test('a finished catalog leaves the broadcast up until closed', () {
    final broadcast = BroadcastProducer();
    final catalog = moq_media.CatalogProducer(broadcast: broadcast);
    broadcast.publishTrack(name: 'status', info: null).finish();
    catalog.finish();
    // Finishing the content leaves the broadcast up until it closes.
    broadcast.consume();
    broadcast.close();
    expect(() => catalog.finish(), throwsA(isA<ClosedMoqException>()));
  });

  test('microsecond timestamps read back as Durations', () {
    expect(
      Frame(payload: Uint8List(0), timestampUs: 20000).timestamp,
      const Duration(milliseconds: 20),
    );
    expect(
      moq_video.Frame(timestampUs: 33333, data: Uint8List(0)).timestamp,
      const Duration(microseconds: 33333),
    );
  });

  test('a broadcast is reachable only while announced', () async {
    final origin = MoqOriginProducer(config: MoqOriginConfig());
    final publication = origin.createBroadcast(path: 'live');
    final broadcast = publication.broadcast();
    broadcast.publishTrack(name: 'events', info: null);
    final consumer = origin.consume();
    await expectLater(
      consumer.request(path: 'live').timeout(timeout),
      throwsA(anything),
    );

    publication.announce(route: AnnounceRoute());
    final announced = consumer.announced(config: MoqAnnounceConfig());
    final first = await nextRoute(announced);
    expect(first, isA<AnnounceEventStart>());
    expect((first as AnnounceEventStart).announce.prefix, 'live');

    publication.unannounce();
    final retracted = await nextRoute(announced);
    expect(retracted, isA<AnnounceEventEnd>());
    expect((retracted as AnnounceEventEnd).announce.prefix, 'live');
    await expectLater(
      consumer.request(path: 'live').timeout(timeout),
      throwsA(anything),
    );

    publication.announce(route: AnnounceRoute());
    expect(await nextRoute(announced), isA<AnnounceEventStart>());
    await consumer.request(path: 'live').timeout(timeout);

    // Disposing the publication keeps the route; closing the broadcast ends it.
    publication.dispose();
    await consumer.request(path: 'live').timeout(timeout);
    broadcast.close();
    expect(await nextRoute(announced), isA<AnnounceEventEnd>());
    announced.cancel();
    announced.dispose();
  });

  test('a stale publication leaves its replacement alone', () async {
    final origin = MoqOriginProducer(config: MoqOriginConfig());
    final stale = origin.createBroadcast(path: 'live');
    stale.announce(route: AnnounceRoute());
    stale.broadcast().close();

    final replacement = origin.createBroadcast(path: 'live');
    replacement.announce(route: AnnounceRoute());
    expect(
      () => stale.announce(route: AnnounceRoute()),
      throwsA(isA<ClosedMoqException>()),
    );
    expect(() => stale.unannounce(), throwsA(isA<ClosedMoqException>()));
    await origin.consume().request(path: 'live').timeout(timeout);
    replacement.broadcast().close();
  });

  test('a new epoch restarts announce consumers', () async {
    final origin = MoqOriginProducer(config: MoqOriginConfig());
    final announced = origin.consume().announced(config: MoqAnnounceConfig());
    final publication = origin.createBroadcast(path: 'epoch');

    final first = mintEpoch();
    publication.announce(route: AnnounceRoute(epoch: first));
    final start = await nextRoute(announced) as AnnounceEventStart;
    expect(start.announce.route.epoch, first);

    final second = mintEpoch();
    publication.announce(route: AnnounceRoute(epoch: second));
    final restart = await nextRoute(announced) as AnnounceEventRestart;
    expect(restart.announce.route.epoch, second);

    expect(
      () => publication.announce(route: AnnounceRoute(epoch: 'not-an-epoch')),
      throwsA(isA<MoqException>()),
    );
    announced.cancel();
    announced.dispose();
  });

  test('announced pattern reports captures', () async {
    final origin = MoqOriginProducer(config: MoqOriginConfig());
    final announced = origin.consume().announced(
      config: MoqAnnounceConfig(prefix: 'room', filter: '*/chat'),
    );
    final publication = origin.createBroadcast(path: 'room/alice/chat');
    publication.announce(route: AnnounceRoute());

    final update = await nextRoute(announced) as AnnounceEventStart;
    expect(update.announce.prefix, 'room/alice/chat');
    expect(update.announce.captures, ['alice']);
  });

  test('dynamic serves a request under a prefix', () async {
    final origin = MoqOriginProducer(config: MoqOriginConfig());
    final dynamic = origin.requests(prefix: 'live');
    final pending = origin.consume().request(path: 'live/cam');
    final request = await dynamic.requestedBroadcast().timeout(timeout);
    expect(request.path(), 'live/cam');
    final prepared = request.prepare(epoch: mintEpoch());
    final served = prepared.publish();
    await pending.timeout(timeout);
    // The publication controls the prepared fill: closing that ends its announcement.
    prepared.broadcast().close();
    expect(() => served.unannounce(), throwsA(isA<ClosedMoqException>()));
    dynamic.cancel();
    dynamic.dispose();
    served.dispose();
  });

  test('JSON tracks take over a raw track', () async {
    final broadcast = MoqBroadcastProducer();
    final producer = moq_json.SnapshotProducer(
      broadcast: broadcast,
      track: broadcast.publishTrack(
        name: 'status',
        info: MoqTrackInfo(timescale: 1000000, compress: true),
      ),
      config: moq_json.SnapshotConfig(),
    );
    final track = await broadcast
        .consume()
        .subscribeTrack(name: 'status', subscription: null)
        .timeout(timeout);
    final consumer = moq_json.SnapshotConsumer(track: track);

    for (var n = 1; n <= 4; n++) {
      producer.update(value: jsonEncode({'n': n}), timestampUs: n * 1000);
    }

    // next() yields every state in order; latest() skips the buffered third.
    moq_json.Value? value = await consumer.next().timeout(timeout);
    expect(jsonDecode(value!.value), {'n': 1});
    expect(value.timestampUs, 1000);
    value = await consumer.next().timeout(timeout);
    expect(jsonDecode(value!.value), {'n': 2});
    value = await consumer.latest().timeout(timeout);
    expect(jsonDecode(value!.value), {'n': 4});
    expect(value.timestampUs, 4000);

    consumer.cancel();
    producer.finish();
    broadcast.close();
  });

  test('JSON window yields pushes and pops in order', () async {
    final broadcast = MoqBroadcastProducer();
    final producer = moq_json.WindowProducer(
      track: broadcast.publishTrack(
        name: 'log',
        info: MoqTrackInfo(timescale: 1000000, compress: true),
      ),
      config: moq_json.WindowConfig(),
    );
    final track = await broadcast
        .consume()
        .subscribeTrack(name: 'log', subscription: null)
        .timeout(timeout);
    final consumer = moq_json.WindowConsumer(track: track);

    for (var n = 0; n < 3; n++) {
      producer.push(value: jsonEncode({'n': n}), timestampUs: n * 1000);
    }
    producer.pop(count: 2, timestampUs: 3000);

    for (var n = 0; n < 3; n++) {
      final timed = await consumer.next().timeout(timeout);
      final push = timed!.event as moq_json.WindowEventPush;
      expect(push.index, n);
      expect(jsonDecode(push.value), {'n': n});
      expect(timed.timestampUs, n * 1000);
    }
    final timed = await consumer.next().timeout(timeout);
    final pop = timed!.event as moq_json.WindowEventPop;
    expect([pop.start, pop.end], [0, 2]);
    expect(timed.timestampUs, 3000);

    // The cut's new group restates record 2, which this reader already has.
    producer.cut();
    producer.push(value: jsonEncode({'n': 3}), timestampUs: 4000);
    final pushed = await consumer.next().timeout(timeout);
    final push = pushed!.event as moq_json.WindowEventPush;
    expect(push.index, 3);
    expect(pushed.timestampUs, 4000);

    producer.finish();
    expect(await consumer.next().timeout(timeout), isNull);
    broadcast.close();
  });

  test('decoded video frame owns its picture', () async {
    final broadcast = BroadcastProducer();
    final video = moq_video.Producer(
      catalog: moq_media.CatalogProducer(broadcast: broadcast),
      options: moq_video.ProducerOptions(
        input: moq_video.EncoderInput(
          format: moq_video.PixelFormat.rgba,
          width: 320,
          height: 240,
          framerate: moq_video.FrameRate(numerator: 30000, denominator: 1001),
        ),
        output: moq_video.EncoderOutput(
          codec: moq_video.Codec.h264,

          kind: moq_video.SoftwareEncoder(),
        ),
        track: 'camera',
      ),
    );

    // Seed the track so a subscriber joining below lands on encoded media.
    final rgba = Uint8List(320 * 240 * 4)..fillRange(0, 320 * 240 * 4, 0x80);
    video.cut();
    for (var i = 0; i < 10; i++) {
      video.write(
        frame: moq_video.Frame(timestampUs: i * 33333, data: rgba),
      );
    }

    final consumer = broadcast.consume();
    final catalogs = await moq_media.CatalogConsumer.subscribe(
      broadcast: consumer,
    ).timeout(timeout);
    final catalog = await catalogs.next().timeout(timeout);
    final decoder = await moq_video.Consumer.subscribe(
      broadcast: consumer,
      id: 'camera',
      catalogVideo: catalog!.video['camera']!,
      output: moq_video.DecoderOutput(),
    ).timeout(timeout);

    // Keep the encoder fed so the decoder sees frames after it joined.
    for (var i = 10; i < 40; i++) {
      video.write(
        frame: moq_video.Frame(timestampUs: i * 33333, data: rgba),
      );
    }

    final read = await decoder.next().timeout(timeout);
    final frame = (read as moq_video.ReadFrame).frame;
    // The frame outlives its consumer's cancellation.
    decoder.cancel();
    final pixels = frame.width() * frame.height();
    expect(
      frame.pixels(format: moq_video.PixelFormat.i420).length,
      pixels * 3 ~/ 2,
    );
    final packed = frame.pixels(format: moq_video.PixelFormat.rgba);
    expect(packed.length, pixels * 4);
    for (var i = 3; i < packed.length; i += 4) {
      expect(packed[i], 0xFF);
    }
    expect(frame.surface(), isNull);
    frame.dispose();

    video.finish();
    catalogs.cancel();
    broadcast.close();
  });

  // A fixed playout delay is reported back as is, and an estimated one moves as frames are read.
  test('audio consumer reports its playout delay', () async {
    final broadcast = BroadcastProducer();
    final audio = moq_audio.Producer(
      catalog: moq_media.CatalogProducer(broadcast: broadcast),
      options: moq_audio.ProducerOptions(
        input: moq_audio.EncoderInput(
          format: moq_audio.SampleFormat.f32,
          sampleRate: 48000,
          channels: 1,
        ),
        output: moq_audio.EncoderOutput(codec: moq_audio.Codec.opus()),
        track: 'mic',
      ),
    );

    final consumer = broadcast.consume();
    final catalogs = await moq_media.CatalogConsumer.subscribe(
      broadcast: consumer,
    ).timeout(timeout);
    final rendition = (await catalogs.next().timeout(timeout))!.audio['mic']!;

    final fixedOutput = moq_audio.DecoderOutput(
      format: moq_audio.SampleFormat.f32,
      delay: const Duration(milliseconds: 80),
    );
    final fixed = await moq_audio.Consumer.subscribe(
      broadcast: consumer,
      id: 'mic',
      catalogAudio: rendition,
      output: fixedOutput,
    ).timeout(timeout);
    final estimated = await moq_audio.Consumer.subscribe(
      broadcast: consumer,
      id: 'mic',
      catalogAudio: rendition,
      output: moq_audio.DecoderOutput(format: moq_audio.SampleFormat.f32),
    ).timeout(timeout);
    expect(fixed.delay(), const Duration(milliseconds: 80));
    final initial = estimated.delay();

    // Two 20 ms Opus frames of silence at 48 kHz mono.
    for (var i = 0; i < 2; i++) {
      audio.write(
        frame: moq_audio.Frame(
          timestampUs: i * 20000,
          data: Uint8List(960 * 4),
        ),
      );
    }
    expect(await fixed.next().timeout(timeout), isA<moq_audio.ReadFrame>());
    expect(await estimated.next().timeout(timeout), isA<moq_audio.ReadFrame>());

    expect(fixed.delay(), const Duration(milliseconds: 80));
    expect(estimated.delay(), isNot(initial));

    fixed.cancel();
    estimated.cancel();
    audio.finish();
    catalogs.cancel();
    broadcast.close();
  });

  test('audio producer encodes opus', () {
    final broadcast = BroadcastProducer();
    final producer = moq_audio.Producer(
      catalog: moq_media.CatalogProducer(broadcast: broadcast),
      options: moq_audio.ProducerOptions(
        input: moq_audio.EncoderInput(
          format: moq_audio.SampleFormat.f32,
          sampleRate: 48000,
          channels: 1,
        ),
        output: moq_audio.EncoderOutput(
          codec: moq_audio.Codec.opus(),
          frameDuration: const Duration(milliseconds: 10),
        ),
        track: 'mic',
      ),
    );
    producer.write(
      frame: moq_audio.Frame(timestampUs: 0, data: Uint8List(960 * 4)),
    );
    expect(producer.demand().name(), 'mic');
    producer.finish();
    broadcast.close();
  });
}
