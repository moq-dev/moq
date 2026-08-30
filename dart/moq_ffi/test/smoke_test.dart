import 'dart:convert';

import 'package:moq_ffi/moq_ffi.dart';
import 'package:test/test.dart';

void main() {
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
