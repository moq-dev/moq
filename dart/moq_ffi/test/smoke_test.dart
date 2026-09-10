import 'dart:convert';

import 'package:moq_ffi/moq_ffi.dart';
import 'package:test/test.dart';

void main() {
  test('stream abort preserves the structured protocol record', () async {
    final track = MoqBroadcastProducer().publishTrack(
      name: 'errors',
      info: null,
    );
    final consumer = track.consume(subscription: null);
    final producer = track.appendGroup();
    final group = await consumer.nextGroup();
    producer.abort(errorCode: 404);
    await expectLater(
      group!.readFrame(),
      throwsA(
        isA<ProtocolMoqException>().having(
          (error) => error.details,
          'protocol details',
          isA<MoqProtocolException>()
              .having((details) => details.scope, 'scope', MoqErrorScope.stream)
              .having((details) => details.code, 'code', 64 + 404)
              .having((details) => details.kind, 'kind', MoqProtocolKind.app),
        ),
      ),
    );
  });

  test('raw track round trips a frame', () async {
    final broadcast = MoqBroadcastProducer();
    final track = broadcast.publishTrack(name: 'events', info: null);
    final consumer = track.consume(subscription: null);
    final nextGroup = consumer.nextGroup();

    final producer = track.appendGroup();
    producer.writeFrame(frame: MoqFrame(payload: utf8.encode('dart')));
    producer.finish();

    final group = await nextGroup;
    final frame = await group?.readFrame();
    expect(frame, isNotNull);
    expect(utf8.decode(frame!.payload), 'dart');
  });
}
