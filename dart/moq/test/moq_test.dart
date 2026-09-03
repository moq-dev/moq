import 'dart:convert';

import 'package:moq/moq.dart';
import 'package:test/test.dart';

const timeout = Duration(seconds: 10);

void main() {
  test('connects, announces, subscribes, and delivers a frame', () async {
    final relay = MoqOriginProducer(options: MoqOriginOptions());
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

    final announcement = client.announcements().first;
    final broadcast = relay.createBroadcast(path: 'live');
    final track = broadcast.publishTrack(name: 'events', info: null);
    final announced = await announcement.timeout(timeout);
    expect(announced.path(), 'live');

    final requested = await client
        .requestBroadcast(announced.path())
        .timeout(timeout);
    final consumer = await requested
        .subscribeTrack(name: 'events', subscription: null)
        .timeout(timeout);
    await track.used().timeout(timeout);

    final nextGroup = consumer.nextGroup();
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
}
