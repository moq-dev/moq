import 'dart:convert';

import 'package:moq/moq.dart';
import 'package:test/test.dart';

const timeout = Duration(seconds: 10);

void main() {
  test('connects, announces, subscribes, and delivers a frame', () async {
    final relay = MoqOriginProducer(config: MoqOriginConfig());
    final server = MoqServer();
    server.setBind(addr: '127.0.0.1:0');
    server.setTlsGenerate(hostnames: ['localhost']);
    server.setPublish(origin: relay);
    server.setConsume(origin: relay);
    final address = await server.listen().timeout(timeout);

    final accepted = () async {
      final request = await server.accept().timeout(timeout);
      if (request == null) throw StateError('server closed before accept');
      return request.accept().timeout(timeout);
    }();

    final client = await Moq.connect(
      'https://$address',
      tlsVerify: false,
      bind: '127.0.0.1:0',
    ).timeout(timeout);
    final serverSession = await accepted;
    expect(client.bandwidth(), isA<MoqBandwidth>());

    final announcement = client.announcements().first;
    final broadcast = relay.createBroadcast(path: 'live');
    final track = broadcast.publishTrack(name: 'events', info: null);
    broadcast.announce(route: MoqRoute());
    final announced = await announcement.timeout(timeout);
    expect(announced.prefix(), 'live');

    final requested = await client
        .requestBroadcast(announced.prefix())
        .timeout(timeout);
    final consumer = await requested
        .subscribeTrack(name: 'events', subscription: null)
        .timeout(timeout);

    // Routed subscriptions pull their source lazily when the consumer is first read.
    final nextGroup = consumer.nextGroup();
    await track.used().timeout(timeout);

    final producer = track.appendGroup();
    producer.writeFrame(
      frame: MoqFrame(payload: utf8.encode('dart round trip')),
    );
    producer.finish();

    final group = await nextGroup.timeout(timeout);
    final frame = await group?.readFrame().timeout(timeout);
    expect(frame, isNotNull);
    expect(utf8.decode(frame!.payload), 'dart round trip');

    client.close();
    serverSession.cancel(code: 0);
    server.cancel();
  });

  test('announce then unannounce is visible', () async {
    final origin = MoqOriginProducer(config: MoqOriginConfig());
    final broadcast = origin.createBroadcast(path: 'live');
    broadcast.publishTrack(name: 'events', info: null);
    broadcast.announce(route: MoqRoute());

    final announced = origin.consume().announced(config: MoqAnnounceConfig());
    final first = await announced.next().timeout(timeout);
    expect(first?.prefix(), 'live');
    expect(first?.active(), isTrue);

    broadcast.unannounce();
    final retracted = await announced.next().timeout(timeout);
    expect(retracted?.prefix(), 'live');
    expect(retracted?.active(), isFalse);
    await origin.consume().requestBroadcast(path: 'live').timeout(timeout);
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

    final update = await announced.next().timeout(timeout);
    expect(update?.prefix(), 'room/alice/chat');
    expect(update?.captures(), ['alice']);
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
}
