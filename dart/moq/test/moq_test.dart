import 'dart:convert';
import 'dart:typed_data';

import 'package:moq/media.dart' as moq_media;
import 'package:moq/json.dart' as moq_json;
import 'package:moq/moq.dart';
import 'package:test/test.dart';

const timeout = Duration(seconds: 10);

/// The next announce event.
Future<AnnounceEvent> nextRoute(AnnounceConsumer announced) async {
  final event = await announced.next().timeout(timeout);
  if (event == null) throw StateError('announce stream ended');
  return event;
}

void main() {
  test('catalog handle closes with its broadcast', () {
    final broadcast = BroadcastProducer();
    final catalog = moq_media.CatalogProducer(broadcast: broadcast);
    catalog.setSection(name: 'app', json: '{"value":42}');
    broadcast.close();
    expect(
      () => catalog.removeSection(name: 'app'),
      throwsA(isA<ClosedMoqException>()),
    );
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
      options: const ConnectOptions(tlsVerify: false, bind: '127.0.0.1:0'),
    ).timeout(timeout);
    final serverSession = await accepted;
    expect(client.bandwidth(), isA<MoqBandwidth>());

    final announcement = client.announced().updates().firstWhere(
      (event) => event is AnnounceEventStart,
    );
    final broadcast = relay.createBroadcast(path: 'live');
    final track = broadcast.publishTrack(name: 'events', info: null);
    broadcast.announce(route: MoqRoute());
    final announced =
        (await announcement.timeout(timeout) as AnnounceEventStart).announce;
    expect(announced.prefix, 'live');

    final requested = await client
        .requestBroadcast(announced.prefix)
        .timeout(timeout);
    final consumer = await requested
        .subscribeTrack(name: 'events', subscription: null)
        .timeout(timeout);

    // Routed subscriptions pull their source lazily when the consumer is first read.
    final nextGroup = consumer.nextGroup();
    await track.demand().used().timeout(timeout);

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
      ),
    ).timeout(timeout);
    expect(server.certFingerprints(), isNotEmpty);

    final accepted = () async {
      final request = await server.requests().first.timeout(timeout);
      return request.accept().timeout(timeout);
    }();

    // A one-shot QUIC-only dial with explicit pacing: every knob reaches the
    // FFI client, and QUIC alone still connects.
    final client = await Moq.connect(
      'https://${server.localAddr}',
      options: ConnectOptions(
        tlsVerify: false,
        bind: '127.0.0.1:0',
        reconnect: false,
        backoff: Backoff(initialUs: 1000, maxUs: 2000, timeoutUs: 3000),
        websocketEnabled: false,
        websocketDelay: Duration.zero,
      ),
    ).timeout(timeout);
    final serverSession = await accepted;

    final announcement = client.announced().updates().firstWhere(
      (event) => event is AnnounceEventStart,
    );
    final broadcast = server.createBroadcast('live');
    final track = broadcast.publishTrack(name: 'events', info: null);
    broadcast.announce(route: MoqRoute());
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
          websocketDelay: Duration(microseconds: -1),
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

  test('microsecond fields read back as Durations', () {
    final backoff = Backoff(initialUs: 1000, maxUs: 2000, timeoutUs: 3000);
    expect(backoff.initial, const Duration(milliseconds: 1));
    expect(backoff.max, const Duration(milliseconds: 2));
    expect(backoff.timeout, const Duration(milliseconds: 3));

    expect(ConnectionStats().rtt, isNull);
    expect(
      ConnectionStats(rttUs: 1500).rtt,
      const Duration(microseconds: 1500),
    );
    expect(
      Frame(payload: Uint8List(0), timestampUs: 20000).timestamp,
      const Duration(milliseconds: 20),
    );
  });

  test('a broadcast is reachable only while announced', () async {
    final origin = MoqOriginProducer(config: MoqOriginConfig());
    final broadcast = origin.createBroadcast(path: 'live');
    broadcast.publishTrack(name: 'events', info: null);
    final consumer = origin.consume();
    await expectLater(
      consumer.requestBroadcast(path: 'live').timeout(timeout),
      throwsA(anything),
    );

    broadcast.announce(route: MoqRoute());
    final announced = consumer.announced(config: MoqAnnounceConfig());
    final first = await nextRoute(announced);
    expect(first, isA<AnnounceEventStart>());
    expect((first as AnnounceEventStart).announce.prefix, 'live');

    broadcast.unannounce();
    final retracted = await nextRoute(announced);
    expect(retracted, isA<AnnounceEventEnd>());
    expect((retracted as AnnounceEventEnd).announce.prefix, 'live');
    await expectLater(
      consumer.requestBroadcast(path: 'live').timeout(timeout),
      throwsA(anything),
    );

    broadcast.announce(route: MoqRoute());
    expect(await nextRoute(announced), isA<AnnounceEventStart>());
    await consumer.requestBroadcast(path: 'live').timeout(timeout);
    announced.cancel();
    announced.dispose();
  });

  test('announced pattern reports captures', () async {
    final origin = MoqOriginProducer(config: MoqOriginConfig());
    final announced = origin.consume().announced(
      config: MoqAnnounceConfig(prefix: 'room', filter: '*/chat'),
    );
    final broadcast = origin.createBroadcast(path: 'room/alice/chat');
    broadcast.announce(route: MoqRoute());

    final update = await nextRoute(announced) as AnnounceEventStart;
    expect(update.announce.prefix, 'room/alice/chat');
    expect(update.announce.captures, ['alice']);
  });

  test('dynamic serves a request under a prefix', () async {
    final origin = MoqOriginProducer(config: MoqOriginConfig());
    final dynamic = origin.dynamic_(prefix: 'live', route: MoqRoute());
    final pending = origin.consume().requestBroadcast(path: 'live/cam');
    final request = await dynamic.requestedBroadcast().timeout(timeout);
    expect(request.path(), 'live/cam');
    final served = MoqBroadcastProducer();
    request.accept(broadcast: served);
    await pending.timeout(timeout);
    dynamic.cancel();
    dynamic.dispose();
    served.dispose();
  });

  test('JSON tracks take over a raw track', () async {
    final broadcast = MoqBroadcastProducer();
    final producer = moq_json.SnapshotProducer(
      broadcast: broadcast,
      track: broadcast.publishTrack(name: 'status', info: null),
      config: moq_json.SnapshotConfig(),
    );
    final track = await broadcast
        .consume()
        .subscribeTrack(name: 'status', subscription: null)
        .timeout(timeout);
    final consumer = moq_json.SnapshotConsumer(
      track: track,
      config: moq_json.SnapshotConfig(),
    );

    producer.update(value: jsonEncode({'state': 'live'}));
    final value = await consumer.next().timeout(timeout);
    expect(jsonDecode(value!), {'state': 'live'});

    consumer.cancel();
    producer.finish();
    broadcast.close();
  });
}
