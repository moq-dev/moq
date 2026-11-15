import 'dart:convert';
import 'dart:io' show Platform;
import 'dart:typed_data';

import 'package:moq/audio.dart' as moq_audio;
import 'package:moq/media.dart' as moq_media;
import 'package:moq/json.dart' as moq_json;
import 'package:moq/moq.dart';
import 'package:moq/src/surface.dart' show surfaceFrom;
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
  test('datagram-only requests have independent priority demand', () async {
    final broadcast = BroadcastProducer();
    final stream = broadcast.publishTrack(name: 'events', info: null);
    final requests = broadcast.requests().datagrams();
    final consumer = broadcast.consume().datagrams(name: 'events');
    consumer.setPriority(priority: 12);
    final request = await requests.requested().timeout(timeout);
    expect(request.name(), 'events');
    expect(request.priority(), 12);
    final producer = request.accept(info: null);
    expect(producer.priority(), 12);
    expect(stream.demand().wanted(), isNull);
    producer.insert(
      datagram: Datagram(
        sequence: 7,
        timestamp: Timestamp.unscaled(7),
        payload: utf8.encode('seven'),
      ),
    );
    final datagram = await consumer.recv().timeout(timeout);
    expect(datagram?.sequence, 7);
    expect(datagram?.timestamp.scale, isNull);
    expect(datagram?.timestamp.value, 7);
    expect(datagram?.payload, utf8.encode('seven'));
    // An unscaled timestamp names the sequence an append writes at.
    expect(
      producer.append(
        timestamp: Timestamp.unscaled(9),
        payload: utf8.encode('nine'),
      ),
      9,
    );
    expect((await consumer.recv().timeout(timeout))?.sequence, 9);
    consumer.cancel();
    expect(await producer.priorityChanged(last: 12).timeout(timeout), isNull);
    producer.finish();
    requests.cancel();
    broadcast.close();
  });

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

  test('raw writes on a scale-less track carry their group key', () async {
    final broadcast = BroadcastProducer();
    final track = broadcast.publishTrack(name: 'untimed', info: null);
    final consumer = track.consume(subscription: null);
    track.writeFrame(
      frame: Frame(
        payload: utf8.encode('payload'),
        timestamp: Timestamp.unscaled(0),
      ),
    );
    final frame = await consumer.readFrame().timeout(timeout);
    expect(frame?.timestamp.scale, isNull);
    expect(frame?.timestamp.value, 0);
    expect(consumer.info().timescale, isNull);
    // A scaled timestamp disagrees with the scale-less track.
    expect(
      () => track.writeFrame(
        frame: Frame(
          payload: utf8.encode('payload'),
          timestamp: Duration.zero.toTimestamp(),
        ),
      ),
      throwsA(
        isA<ProtocolMoqException>().having(
          (error) => error.details.kind,
          'kind',
          MoqProtocolKind.timestampMismatch,
        ),
      ),
    );
    track.finish();
    broadcast.close();
  });

  test('an omitted max age is zero and the unlimited flag sets none', () {
    final broadcast = BroadcastProducer();
    final omitted = broadcast
        .publishTrack(name: 'omitted', info: null)
        .consume(subscription: null)
        .info();
    expect(omitted.maxAge, Duration.zero);
    final unlimited = broadcast
        .publishTrack(name: 'unlimited', info: TrackInfo(maxAgeUnlimited: true))
        .consume(subscription: null)
        .info();
    expect(unlimited.maxAge, isNull);
    expect(unlimited.maxAgeUnlimited, isTrue);
    broadcast.close();
  });

  test('a default subscription skips superseded groups at once', () {
    for (final subscription in [defaultSubscription(), Subscription()]) {
      expect(subscription.delay.min, Duration.zero);
      expect(subscription.delay.max, Duration.zero);
    }
    final unset = Delay(min: null, max: null);
    expect(unset.min, isNull);
    expect(unset.max, isNull);
  });

  test('a negative duration in a raw record is refused', () {
    final broadcast = BroadcastProducer();
    final track = broadcast.publishTrack(name: 'negative', info: null);
    expect(
      () => track.consume(
        subscription: Subscription(
          delay: Delay(max: const Duration(microseconds: -1)),
        ),
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
        info: TrackInfo(maxAgeUnlimited: true, timescale: 1000000),
      );
      for (var ms = 0; ms < 3; ms++) {
        final group = track.appendGroup();
        group.writeFrame(
          frame: Frame(
            payload: utf8.encode('x'),
            timestamp: Duration(milliseconds: ms).toTimestamp(),
          ),
        );
        group.finish();
      }

      final consumer = track.consume(
        subscription: Subscription(
          delay: Delay(max: const Duration(seconds: 10)),
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

  test(
    'a credential is answered, names a route and a claim, and cancels',
    () async {
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

      expect(() => client.credential(''), throwsA(isA<ConfigMoqException>()));
      final Credential credential = client.credential('extra');
      // The server verifies no tokens of its own, so it refuses this one.
      await expectLater(
        credential.answered().timeout(timeout),
        throwsA(isA<UnsupportedMoqException>()),
      );

      final route = client.route('room/x', options: RouteOptions(hidden: true));
      final claim = client.session.publish().claim(
        prefix: 'live',
        options: RouteOptions(hidden: true),
      );
      route.setAuth(credential: credential);
      claim.setAuth(credential: credential);
      route.setAuth(credential: null);
      claim.setAuth(credential: null);

      credential.cancel();
      await expectLater(
        credential.answered().timeout(timeout),
        throwsA(isA<CancelledMoqException>()),
      );
      expect(
        () => route.setAuth(credential: credential),
        throwsA(isA<CancelledMoqException>()),
      );
      expect(
        () => claim.setAuth(credential: credential),
        throwsA(isA<CancelledMoqException>()),
      );
      route.cancel();
      claim.cancel();

      await client.close();
      serverSession.cancel(code: 0);
      server.cancel();
    },
  );

  test('an announced instance pins request and follow', () async {
    // moq-lite-06 carries no instance, so the session mints one for the route.
    const version = ['moq-lite-06'];
    final origin = MoqOriginProducer(config: MoqOriginConfig());
    final server = MoqServer(
      config: MoqServerConfig(
        bind: '127.0.0.1:0',
        versions: version,
        tls: MoqServerTls(generate: ['localhost']),
        quic: MoqQuicConfig(),
        publish: origin,
      ),
    );
    final address = await server.listen().timeout(timeout);
    final accepted = () async {
      final request = await server.accept().timeout(timeout);
      if (request == null) throw StateError('server closed before accept');
      return request.accept().timeout(timeout);
    }();
    final route = origin.route(path: 'live', options: RouteOptions());

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

    final announced = client.announced();
    final present = await nextRoute(announced) as AnnounceEventPresent;
    final minted = present.announce.route.instance;
    expect(minted.isMinted(), isTrue);

    await client.request('live', instance: minted).timeout(timeout);
    final following = client.follow('live', instance: minted);
    expect(await following.available().timeout(timeout), isNotNull);

    // The same text, parsed, names another instance that nothing serves.
    final named = Instance.parse(text: minted.text());
    expect(named.isMinted(), isFalse);
    expect(named, isNot(minted));
    await expectLater(
      client.request('live', instance: named).timeout(timeout),
      throwsA(isA<ProtocolMoqException>()),
    );

    following.cancel();
    announced.cancel();
    await client.close();
    serverSession.cancel(code: 0);
    route.cancel();
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

    final viewers = client.viewers();
    expect(viewers, isA<Viewers>());
    final viewer = viewers.watch(maxBps: 4000000);
    expect(viewer, isA<Viewer>());
    // A slice never exceeds its ceiling, whether or not QUIC has an estimate yet.
    viewer.update(maxBps: 1000000);
    expect(viewer.rate() ?? 0, lessThanOrEqualTo(1000000));
    viewers.setTarget(targetBps: 0);
    viewers.setTarget(targetBps: null);
    // Past what a PROBE target carries: refused.
    expect(
      () => viewers.watch(maxBps: 1 << 53),
      throwsA(isA<OutOfRangeMoqException>()),
    );
    viewer.dispose();

    final announcement = client.announced().updates().firstWhere(
      (event) => event is AnnounceEventPresent,
    );
    final route = relay.route(
      path: 'live',
      options: RouteOptions(hidden: true),
    );
    final track = route.broadcast().publishTrack(
      name: 'events',
      info: TrackInfo(timescale: 1000000),
    );
    route.announce();
    final announced =
        (await announcement.timeout(timeout) as AnnounceEventPresent).announce;
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
      frame: MoqFrame(
        payload: utf8.encode('dart round trip'),
        timestamp: Duration.zero.toTimestamp(),
      ),
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
      (event) => event is AnnounceEventPresent,
    );
    final route = server.route('live', options: RouteOptions(hidden: true));
    final broadcast = route.broadcast();
    final track = broadcast.publishTrack(name: 'events', info: null);
    route.announce();
    final announced =
        (await announcement.timeout(timeout) as AnnounceEventPresent).announce;
    expect(announced.prefix, 'live');

    await client.close();
    serverSession.cancel(code: 0);
    track.finish();
    broadcast.close();
    broadcast.close(); // a second close is a no-op
    route.dispose();
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

  test('a broadcast consumer\'s closed() resolves once it closes', () async {
    final broadcast = BroadcastProducer();
    var resolved = false;
    final closed = broadcast.consume().closed().then((_) => resolved = true);
    await Future<void>.delayed(const Duration(milliseconds: 10));
    expect(resolved, isFalse, reason: 'resolved while the broadcast is live');
    broadcast.close();
    await closed.timeout(timeout);
  });

  test('timestamps round-trip exactly at the track scale', () async {
    final broadcast = BroadcastProducer();
    final track = broadcast.publishTrack(
      name: 'video',
      info: TrackInfo(timescale: 90000),
    );
    final consumer = track.consume(subscription: null);
    // A present zero stays present, and one 90 kHz tick reads back as itself
    // rather than the 11 us a microsecond field floors it to.
    for (final value in [0, 1]) {
      track.writeFrame(
        frame: Frame(
          payload: utf8.encode('frame'),
          timestamp: Timestamp(value: value, scale: 90000),
        ),
      );
      final frame = await consumer.readFrame().timeout(timeout);
      expect(frame?.timestamp.value, value);
      expect(frame?.timestamp.scale, 90000);
    }

    for (final invalid in [
      Timestamp(value: 0, scale: 0),
      Timestamp(value: 0, scale: 1 << 62),
      Timestamp(value: 1 << 61, scale: 90000),
    ]) {
      expect(
        () => track.writeFrame(
          frame: Frame(payload: utf8.encode('frame'), timestamp: invalid),
        ),
        throwsA(isA<TimeOverflowMoqException>()),
      );
    }
    track.finish();
    broadcast.close();
  });

  test('timestamps convert to and from Durations', () {
    final exact = const Duration(milliseconds: 20).toTimestamp();
    expect([exact.value, exact.scale], [20000, 1000000]);
    expect(exact.toDuration(), const Duration(milliseconds: 20));
    // 5 ticks at 90 kHz is 55.6 us, and half a microsecond is 0.5: both round down.
    expect(
      Timestamp(value: 5, scale: 90000).toDuration(),
      const Duration(microseconds: 55),
    );
    expect(Timestamp(value: 1, scale: 2000000).toDuration(), Duration.zero);
    expect(
      () => Timestamp(value: (1 << 61) - 1, scale: 1).toDuration(),
      throwsRangeError,
    );
    expect(
      () => const Duration(microseconds: -1).toTimestamp(),
      throwsArgumentError,
    );
    // A scale-less track's key is no time.
    final unscaled = Timestamp.unscaled(3);
    expect([unscaled.value, unscaled.scale], [3, null]);
    expect(unscaled.toDuration, throwsStateError);
  });

  test('a broadcast is reachable only while announced', () async {
    final origin = MoqOriginProducer(config: MoqOriginConfig());
    final route = origin.route(
      path: 'live',
      options: RouteOptions(hidden: true),
    );
    final broadcast = route.broadcast();
    broadcast.publishTrack(name: 'events', info: null);
    final consumer = origin.consume();
    await expectLater(
      consumer.request(path: 'live').timeout(timeout),
      throwsA(anything),
    );

    route.announce();
    final announced = consumer.announced(config: MoqAnnounceConfig());
    final first = await nextRoute(announced);
    expect(first, isA<AnnounceEventPresent>());
    expect((first as AnnounceEventPresent).announce.prefix, 'live');
    expect(first.announce.route.instance, route.instance());

    route.withdraw();
    final retracted = await nextRoute(announced);
    expect(retracted, isA<AnnounceEventAbsent>());
    expect((retracted as AnnounceEventAbsent).announce.prefix, 'live');
    await expectLater(
      consumer.request(path: 'live').timeout(timeout),
      throwsA(anything),
    );

    // Announcing again continues the same stream.
    route.announce();
    final again = await nextRoute(announced) as AnnounceEventPresent;
    expect(again.announce.route.instance, route.instance());
    await consumer.request(path: 'live').timeout(timeout);

    // Disposing the broadcast handle keeps the route; cancelling the route retracts it,
    // and its broadcast lives on.
    broadcast.dispose();
    await consumer.request(path: 'live').timeout(timeout);
    final kept = route.broadcast();
    route.cancel();
    expect(await nextRoute(announced), isA<AnnounceEventAbsent>());
    expect(() => route.announce(), throwsA(isA<CancelledMoqException>()));
    kept.publishTrack(name: 'after', info: null);
    kept.close();
    route.dispose();
    announced.cancel();
    announced.dispose();
  });

  test('closing the broadcast retracts its route', () async {
    final origin = MoqOriginProducer(config: MoqOriginConfig());
    final announced = origin.consume().announced(config: MoqAnnounceConfig());
    final route = origin.route(path: 'live', options: RouteOptions());
    expect(await nextRoute(announced), isA<AnnounceEventPresent>());
    route.broadcast().close();
    expect(await nextRoute(announced), isA<AnnounceEventAbsent>());
    expect(() => route.announce(), throwsA(isA<ClosedMoqException>()));
    route.dispose();
    announced.cancel();
    announced.dispose();
  });

  test('a closed route leaves its replacement alone', () async {
    final origin = MoqOriginProducer(config: MoqOriginConfig());
    final stale = origin.route(path: 'live', options: RouteOptions());
    stale.broadcast().close();

    final replacement = origin.route(path: 'live', options: RouteOptions());
    expect(() => stale.announce(), throwsA(isA<ClosedMoqException>()));
    expect(() => stale.setCost(cost: 1), throwsA(isA<ClosedMoqException>()));
    stale.withdraw();
    await origin.consume().request(path: 'live').timeout(timeout);
    replacement.broadcast().close();
    stale.dispose();
    replacement.dispose();
  });

  test('a new route restarts followers but stays listed', () async {
    final origin = MoqOriginProducer(config: MoqOriginConfig());
    final consumer = origin.consume();
    final announced = consumer.announced(config: MoqAnnounceConfig());
    final following = consumer.follow(path: 'instance');
    final first = origin.route(path: 'instance', options: RouteOptions());

    final present = await nextRoute(announced) as AnnounceEventPresent;
    expect(present.announce.route.instance, first.instance());
    (await following.available().timeout(timeout))!.dispose();

    final camera = Instance.parse(text: 'camera');
    expect(camera.isMinted(), isFalse);
    final second = origin.route(
      path: 'instance',
      options: RouteOptions(instance: camera, epoch: 1),
    );
    expect(second.instance(), camera);
    (await following.available().timeout(timeout))!.dispose();

    // The takeover was not listed, so the next event is the path going absent.
    first.withdraw();
    second.withdraw();
    final absent = await nextRoute(announced) as AnnounceEventAbsent;
    expect(absent.announce.route.instance, camera);

    expect(
      () => Instance.parse(text: ''),
      throwsA(isA<InvalidInstanceMoqException>()),
    );
    first.cancel();
    second.cancel();
    first.dispose();
    second.dispose();
    following.cancel();
    following.dispose();
    announced.cancel();
    announced.dispose();
  });

  test(
    'a follower reports the path going offline and the next broadcast',
    () async {
      final origin = MoqOriginProducer(config: MoqOriginConfig());
      final following = origin.consume().follow(path: 'offline');
      final route = origin.route(path: 'offline', options: RouteOptions());
      (await following.available().timeout(timeout))!.dispose();

      route.withdraw();
      expect(await following.available().timeout(timeout), isNull);

      route.announce();
      (await following.available().timeout(timeout))!.dispose();

      route.cancel();
      route.dispose();
      following.cancel();
      following.dispose();
    },
  );

  test('announced pattern reports captures', () async {
    final origin = MoqOriginProducer(config: MoqOriginConfig());
    final announced = origin.consume().announced(
      config: MoqAnnounceConfig(prefix: 'room', filter: '*/chat'),
    );
    final route = origin.route(
      path: 'room/alice/chat',
      options: RouteOptions(),
    );

    final update = await nextRoute(announced) as AnnounceEventPresent;
    expect(update.announce.prefix, 'room/alice/chat');
    expect(update.announce.captures, ['alice']);
    route.dispose();
  });

  test('dynamic serves a request under a prefix', () async {
    final origin = MoqOriginProducer(config: MoqOriginConfig());
    final dynamic = origin.claim(prefix: 'live', options: RouteOptions());
    final pending = origin.consume().request(path: 'live/cam');
    final request = (await dynamic.requested().timeout(timeout))!;
    expect(request.path(), 'live/cam');
    // A hidden job route answers nobody until it announces, under the instance its
    // claim lends.
    final served = request.route(options: RouteOptions(hidden: true));
    expect(served.instance(), dynamic.instance());
    served.announce();
    await pending.timeout(timeout);
    // Closing the job's broadcast ends its route.
    served.broadcast().close();
    expect(() => served.announce(), throwsA(isA<ClosedMoqException>()));
    dynamic.cancel();
    await expectLater(
      dynamic.requested(),
      throwsA(isA<CancelledMoqException>()),
    );
    dynamic.dispose();
    served.dispose();
  });

  test('JSON tracks take over a raw track', () async {
    final broadcast = MoqBroadcastProducer();
    final producer = moq_json.SnapshotProducer(
      broadcast: broadcast,
      track: broadcast.publishTrack(
        name: 'status',
        info: MoqTrackInfo(
          timescale: 1000000,
          compression: MoqCompression.deflate,
        ),
      ),
      config: moq_json.SnapshotConfig(),
    );
    final consumer = moq_json.SnapshotConsumer(
      broadcast: broadcast.consume(),
      name: 'status',
    );

    for (var n = 1; n <= 4; n++) {
      producer.update(
        value: jsonEncode({'n': n}),
        timestamp: Duration(milliseconds: n).toTimestamp(),
      );
    }

    // next() yields every state in order; latest() skips the buffered third.
    moq_json.Value? value = await consumer.next().timeout(timeout);
    expect(jsonDecode(value!.value), {'n': 1});
    expect(value.timestamp.toDuration(), const Duration(milliseconds: 1));
    value = await consumer.next().timeout(timeout);
    expect(jsonDecode(value!.value), {'n': 2});
    value = await consumer.latest().timeout(timeout);
    expect(jsonDecode(value!.value), {'n': 4});
    expect(value.timestamp.toDuration(), const Duration(milliseconds: 4));

    consumer.cancel();
    producer.finish();
    broadcast.close();
  });

  test('JSON window yields pushes and pops in order', () async {
    final broadcast = MoqBroadcastProducer();
    final producer = moq_json.WindowProducer(
      track: broadcast.publishTrack(
        name: 'log',
        info: MoqTrackInfo(
          timescale: 1000000,
          compression: MoqCompression.deflate,
        ),
      ),
      config: moq_json.WindowConfig(),
    );
    final track = await broadcast
        .consume()
        .subscribeTrack(name: 'log', subscription: null)
        .timeout(timeout);
    final consumer = moq_json.WindowConsumer(track: track);

    for (var n = 0; n < 3; n++) {
      producer.push(
        value: jsonEncode({'n': n}),
        timestamp: Duration(milliseconds: n).toTimestamp(),
      );
    }
    producer.pop(
      count: 2,
      timestamp: const Duration(milliseconds: 3).toTimestamp(),
    );

    for (var n = 0; n < 3; n++) {
      final timed = await consumer.next().timeout(timeout);
      final push = timed!.event as moq_json.WindowEventPush;
      expect(push.index, n);
      expect(jsonDecode(push.value), {'n': n});
      expect(timed.timestamp.toDuration(), Duration(milliseconds: n));
    }
    final timed = await consumer.next().timeout(timeout);
    final pop = timed!.event as moq_json.WindowEventPop;
    expect([pop.start, pop.end], [0, 2]);
    expect(timed.timestamp.toDuration(), const Duration(milliseconds: 3));

    // The cut's new group restates record 2, which this reader already has.
    producer.cut();
    producer.push(
      value: jsonEncode({'n': 3}),
      timestamp: const Duration(milliseconds: 4).toTimestamp(),
    );
    final pushed = await consumer.next().timeout(timeout);
    final push = pushed!.event as moq_json.WindowEventPush;
    expect(push.index, 3);
    expect(pushed.timestamp.toDuration(), const Duration(milliseconds: 4));

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
        frame: moq_video.Frame(
          timestamp: Timestamp(value: i * 3003, scale: 90000),
          data: rgba,
        ),
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
        frame: moq_video.Frame(
          timestamp: Timestamp(value: i * 3003, scale: 90000),
          data: rgba,
        ),
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

  // A surface kind the package predates maps to the fallback arm instead of failing.
  test('video surface maps an unknown kind', () {
    expect(surfaceFrom(null), isNull);
    final pixels = surfaceFrom(PixelBufferMoqVideoSurface(7));
    expect((pixels as moq_video.PixelBuffer).pointer, 7);
    expect(surfaceFrom(_FutureSurface()), isA<moq_video.UnknownSurface>());
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
      delay: Delay(min: Duration(milliseconds: 80)),
      estimate: false,
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
          timestamp: Duration(milliseconds: i * 20).toTimestamp(),
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
      frame: moq_audio.Frame(
        timestamp: Duration.zero.toTimestamp(),
        data: Uint8List(960 * 4),
      ),
    );
    expect(producer.demand().name(), 'mic');
    producer.finish();
    broadcast.close();
  });
}

/// Stands in for a generated surface variant this package predates.
final class _FutureSurface extends MoqVideoSurface {
  @override
  RustBuffer lower() => throw UnimplementedError();

  @override
  int allocationSize() => throw UnimplementedError();

  @override
  int write(Uint8List buf) => throw UnimplementedError();
}
