// The same in-process rounds as the other binding benchmark runners (see bench/README.md).
import 'dart:convert';
import 'dart:ffi';
import 'dart:io';
import 'dart:typed_data';

import 'package:moq/moq.dart';

// POSIX clock() counts user and system CPU across every process thread.
final _clock = DynamicLibrary.process()
    .lookupFunction<Int64 Function(), int Function()>('clock');

// Native memcmp, like the other runners' byte equality, so a scalar Dart loop
// doesn't outweigh the copy being measured. The process provides it.
@Native<Int Function(Pointer<Uint8>, Pointer<Uint8>, Size)>(isLeaf: true)
external int memcmp(Pointer<Uint8> a, Pointer<Uint8> b, int length);

double cpu() {
  final ticks = _clock();
  if (ticks < 0) throw StateError('clock() cannot measure process CPU');
  return ticks / 1e6; // CLOCKS_PER_SEC on Linux and Darwin.
}

class Case {
  final int size, publishers, subscribers;
  const Case(this.size, this.publishers, this.subscribers);
  int get frames => ((8 << 20) ~/ size).clamp(1, 64);
  int get delivered => publishers * subscribers * frames * size;
  String get name => 'size=$size/pub=$publishers/sub=$subscribers';
}

void require(bool value) {
  if (!value) throw StateError('missing, reordered, or corrupt frame');
}

Future<void> throughput(
  Case c,
  double budget,
  List<Map<String, Object>> results,
) async {
  final origin = OriginProducer(
    config: OriginConfig(cacheCapacityBytes: 64 << 20),
  );
  final publication = origin.createBroadcast(path: 'bench');
  final broadcast = publication.broadcast();
  final tracks = List.generate(
    c.publishers,
    (p) =>
        broadcast.publishTrack(name: '$p', info: TrackInfo(timescale: 1000000)),
  );
  final consumers = [
    for (final track in tracks)
      for (var s = 0; s < c.subscribers; s++) track.consume(subscription: null),
  ];
  final payload = Uint8List.fromList(List.generate(c.size, (i) => i % 251));
  Future<void> read(TrackConsumer consumer, int n) async {
    final group = await consumer.recvGroup();
    if (group == null) throw StateError('track ended');
    try {
      for (var f = 0; f < c.frames; f++) {
        final frame = await group.readFrame();
        require(
          frame != null &&
              frame.timestampUs == n * c.frames + f &&
              frame.payload.length == payload.length &&
              memcmp(frame.payload.address, payload.address, payload.length) ==
                  0,
        );
      }
    } finally {
      group.dispose();
    }
  }

  Future<void> step(int n) async {
    final readers = [for (final consumer in consumers) read(consumer, n)];
    for (final track in tracks) {
      final group = track.appendGroup();
      try {
        for (var f = 0; f < c.frames; f++) {
          group.writeFrame(
            frame: Frame(payload: payload, timestampUs: n * c.frames + f),
          );
        }
        group.finish();
      } finally {
        group.dispose();
      }
    }
    await Future.wait(readers).timeout(const Duration(seconds: 30));
  }

  try {
    for (var n = 0; n < 3; n++) {
      await step(n);
    }
    final start = Stopwatch()..start();
    final cpuStart = cpu();
    var n = 3;
    do {
      await step(n++);
    } while (start.elapsedMicroseconds / 1e6 < budget);
    final wall = start.elapsedMicroseconds / 1e6;
    final cpuUsed = cpu() - cpuStart;
    final mb = (n - 3) * c.delivered / 1e6;
    results.add({
      'name': 'throughput/${c.name}',
      'unit': 'MB/s',
      'value': mb / wall,
    });
    results.add({
      'name': 'cpu/${c.name}',
      'unit': 'ms/MB',
      'value': cpuUsed * 1e3 / mb,
    });
  } finally {
    for (final consumer in consumers) {
      consumer.dispose();
    }
    for (final track in tracks) {
      track.dispose();
    }
    broadcast.dispose();
    publication.dispose();
    origin.dispose();
  }
}

Future<void> main() async {
  if (!Platform.isLinux && !Platform.isMacOS) {
    throw UnsupportedError('process CPU benchmark requires Linux or macOS');
  }
  final budget = double.parse(Platform.environment['MOQ_BENCH_SECONDS'] ?? '1');
  if (!budget.isFinite || budget <= 0) {
    throw ArgumentError('MOQ_BENCH_SECONDS must be positive and finite');
  }
  final out = Platform.environment['MOQ_BENCH_DIR'];
  if (out == null) throw ArgumentError('MOQ_BENCH_DIR is required');
  final results = <Map<String, Object>>[];
  for (final c in [
    const Case(100, 1, 1),
    const Case(1024, 1, 1),
    const Case(65536, 1, 1),
    const Case(1048576, 1, 1),
    const Case(1024, 1, 4),
    const Case(1024, 4, 1),
    const Case(1024, 4, 4),
  ]) {
    await throughput(c, budget, results);
  }
  final broadcast = BroadcastProducer();
  final track = broadcast.publishTrack(name: 'calls', info: null);
  final producer = track.appendGroup();
  final group = producer.consume();
  final consumer = track.consume(subscription: null);
  void call(String name, Object? Function() fn) {
    final start = Stopwatch()..start();
    var calls = 0;
    do {
      for (var i = 0; i < 1000; i++) {
        fn();
      }
      calls += 1000;
    } while (start.elapsedMicroseconds / 1e6 < budget);
    results.add({
      'name': 'call/$name',
      'unit': 'ns',
      'value': start.elapsedMicroseconds * 1e3 / calls,
    });
  }

  call('sequence', group.sequence);
  call('info', consumer.info);
  consumer.dispose();
  group.dispose();
  producer.dispose();
  track.dispose();
  broadcast.dispose();
  await Directory(out).create(recursive: true);
  await File('$out/dart.json').writeAsString(jsonEncode(results));
}
