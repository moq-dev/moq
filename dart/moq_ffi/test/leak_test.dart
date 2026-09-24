import 'dart:io';
import 'dart:typed_data';

import 'package:moq_ffi/moq_ffi.dart';
import 'package:test/test.dart';

// Each call moves `size` bytes across the FFI boundary, so a leak of that
// buffer grows resident memory by `iterations * size`. Growth under a quarter of
// that leaves room for allocator and Dart heap noise without hiding a leak.
const size = 64 * 1024;
const iterations = 2000;
const leaked = size * iterations;

void main() {
  test('a returned String is released', () {
    final track = MoqBroadcastProducer().publishTrack(
      name: 'x' * size,
      info: null,
    );
    // Warm up so one-time allocations do not count as growth.
    for (var i = 0; i < 100; i++) {
      track.name();
    }

    final before = ProcessInfo.currentRss;
    for (var i = 0; i < iterations; i++) {
      track.name();
    }
    final growth = ProcessInfo.currentRss - before;

    expect(growth, lessThan(leaked ~/ 4));
  });

  test('a non-null optional argument is released', () async {
    // A local broadcast has no origin to resolve against, so each call lowers
    // the optional String and then throws, also covering the error buffer.
    final consumer = MoqBroadcastProducer().consume();
    final reference = 'x' * size;
    Future<void> call() => expectLater(
      consumer.resolve(reference: reference),
      throwsA(isA<MoqException>()),
    );

    for (var i = 0; i < 100; i++) {
      await call();
    }

    final before = ProcessInfo.currentRss;
    for (var i = 0; i < iterations; i++) {
      await call();
    }
    final growth = ProcessInfo.currentRss - before;

    expect(growth, lessThan(leaked ~/ 4));
  });

  test('an async return is released', () async {
    final payload = Uint8List(size);

    // Handles are released deterministically so the only growth left is what
    // the bindings leak, not frames a live track still holds.
    Future<void> roundTrip() async {
      final broadcast = MoqBroadcastProducer();
      final track = broadcast.publishTrack(name: 'frames', info: null);
      final consumer = track.consume(subscription: null);
      final producer = track.appendGroup();
      producer.writeFrame(frame: MoqFrame(payload: payload));
      producer.finish();
      final group = await consumer.nextGroup();
      final frame = await group!.readFrame();
      expect(frame!.payload.length, size);
      group.dispose();
      producer.dispose();
      consumer.dispose();
      track.dispose();
      broadcast.dispose();
    }

    for (var i = 0; i < 100; i++) {
      await roundTrip();
    }

    final before = ProcessInfo.currentRss;
    for (var i = 0; i < iterations; i++) {
      await roundTrip();
    }
    final growth = ProcessInfo.currentRss - before;

    expect(growth, lessThan(leaked ~/ 4));
  });
}
