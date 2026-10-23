import 'dart:async';

import 'package:moq/moq.dart';
import 'package:moq/src/stream.dart';
import 'package:test/test.dart';

final class _Read<T> implements UniffiRustFuture<T> {
  final _result = Completer<T>();
  final cleaned = Completer<void>();
  late final Future<T> _future = _result.future.then(
    (value) {
      cleaned.complete();
      return value;
    },
    onError: (Object error, StackTrace stack) {
      cleaned.complete();
      Error.throwWithStackTrace(error, stack);
    },
  );
  var cancels = 0;

  void deliver(T value) => _result.complete(value);
  void fail(Object error) => _result.completeError(error);

  @override
  Future<void> cancel() {
    if (!cleaned.isCompleted && cancels == 0) {
      cancels++;
      if (!_result.isCompleted) fail(const UniffiCancelledException());
    }
    return _future.then<void>((_) {}, onError: (Object _, StackTrace _) {});
  }

  @override
  Stream<T> asStream() => _future.asStream();
  @override
  Future<T> catchError(Function onError, {bool Function(Object)? test}) =>
      _future.catchError(onError, test: test);
  @override
  Future<R> then<R>(FutureOr<R> Function(T) onValue, {Function? onError}) =>
      _future.then(onValue, onError: onError);
  @override
  Future<T> timeout(Duration timeLimit, {FutureOr<T> Function()? onTimeout}) =>
      _future.timeout(timeLimit, onTimeout: onTimeout);
  @override
  Future<T> whenComplete(FutureOr<void> Function() action) =>
      _future.whenComplete(action);
}

final class _Source extends NativeStream<int?> {
  final started = StreamController<_Read<int?>>.broadcast(sync: true);
  final discarded = <int?>[];
  var reads = 0;
  var releases = 0;

  @override
  UniffiRustFuture<int?> read() {
    reads++;
    final read = _Read<int?>();
    started.add(read);
    return read;
  }

  @override
  bool finished(int? value) => value == null;
  @override
  void discard(int? value) => discarded.add(value);
  @override
  void release() => releases++;
}

final class _Announcements implements AnnounceConsumer {
  final started = Completer<_Read<AnnounceEvent?>>();
  var cancels = 0;
  var disposals = 0;

  @override
  UniffiRustFuture<AnnounceEvent?> next() {
    final read = _Read<AnnounceEvent?>();
    started.complete(read);
    return read;
  }

  @override
  void cancel() => cancels++;
  @override
  void dispose() => disposals++;
  @override
  dynamic noSuchMethod(Invocation invocation) => super.noSuchMethod(invocation);
}

final class _Auth implements Auth {
  final started = Completer<_Read<Grant?>>();
  var disposals = 0;

  @override
  Grant? grant() => null;
  @override
  UniffiRustFuture<Grant?> grantChanged() {
    final read = _Read<Grant?>();
    started.complete(read);
    return read;
  }

  @override
  void dispose() => disposals++;
  @override
  dynamic noSuchMethod(Invocation invocation) => super.noSuchMethod(invocation);
}

void main() {
  test(
    'idle announcement cancellation releases its operation and cursor',
    () async {
      final source = _Announcements();
      final subscription = source.updates().listen((_) {});
      final read = await source.started.future;
      await subscription.cancel();
      expect(read.cancels, 1);
      expect(read.cleaned.isCompleted, isTrue);
      expect(source.cancels, 1);
      expect(source.disposals, 1);
      await subscription.cancel();
      expect(source.disposals, 1);
    },
  );

  test('idle grant cancellation leaves the shared auth usable', () async {
    final auth = _Auth();
    final grants = <Grant?>[];
    final subscription = auth.grants().listen(grants.add);
    final read = await auth.started.future;
    await subscription.cancel();
    expect(grants, [null]);
    expect(read.cancels, 1);
    expect(read.cleaned.isCompleted, isTrue);
    expect(auth.disposals, 0);
    expect(auth.grant(), isNull);
  });

  test('cancellation before the first read still releases ownership', () async {
    final source = _Source();
    final subscription = source.listen((_) {});
    await subscription.cancel();
    expect(source.reads, 0);
    expect(source.releases, 1);
  });

  test('delivery racing cancellation is discarded exactly once', () async {
    final source = _Source();
    final started = source.started.stream.first;
    final values = <int?>[];
    final subscription = source.listen(values.add);
    final read = await started;
    read.deliver(7);
    await subscription.cancel();
    expect(values, isEmpty);
    expect(source.discarded, [7]);
    expect(source.releases, 1);
    expect(read.cleaned.isCompleted, isTrue);
  });

  test('a paused listener holds only one result and can cancel it', () async {
    final source = _Source();
    final started = source.started.stream.first;
    final subscription = source.listen((_) => fail('delivered while paused'));
    final read = await started;
    subscription.pause();
    read.deliver(7);
    await read.cleaned.future;
    expect(source.reads, 1);
    await subscription.cancel();
    expect(source.discarded, [7]);
    expect(source.releases, 1);
  });

  test('cancellation in onData starts no additional read', () async {
    final source = _Source();
    final started = source.started.stream.first;
    final cancelled = Completer<void>();
    late final StreamSubscription<int?> subscription;
    subscription = source.listen((_) {
      subscription.cancel().then((_) => cancelled.complete());
    });
    (await started).deliver(7);
    await cancelled.future;
    expect(source.reads, 1);
    expect(source.discarded, isEmpty);
    expect(source.releases, 1);
  });

  test('read errors are delivered once before cleanup', () async {
    final source = _Source();
    final started = source.started.stream.first;
    final errors = <Object>[];
    final done = Completer<void>();
    final subscription = source.listen(
      (_) => fail('unexpected event'),
      onError: errors.add,
      onDone: done.complete,
    );
    final error = StateError('read failed');
    (await started).fail(error);
    await done.future;
    await subscription.cancel();
    expect(errors, [error]);
    expect(source.releases, 1);
  });

  test(
    'read errors racing cancellation are not delivered afterwards',
    () async {
      final source = _Source();
      final started = source.started.stream.first;
      final subscription = source.listen(
        (_) => fail('unexpected event'),
        onError: (Object _) => fail('error after cancellation'),
      );
      (await started).fail(StateError('read failed'));
      await subscription.cancel();
      expect(source.releases, 1);
    },
  );

  test('idle request cancellation keeps the native server listening', () async {
    final server = await Server.listen(
      options: const ListenOptions(
        bind: '127.0.0.1:0',
        tlsGenerate: ['localhost'],
      ),
    );
    try {
      final subscription = server.requests().listen(
        (_) => fail('unexpected request'),
      );
      await Future<void>(() {});
      expect(server.certFingerprints, throwsA(isA<BusyMoqException>()));
      await subscription.cancel();
      expect(server.certFingerprints(), isNotEmpty);
      final accept = server.requests().first.then(
        (request) => request.accept(),
      );
      final client = await Moq.connect(
        'https://${server.localAddr}',
        options: ConnectOptions(
          tls: ClientTls(insecure: true, hostName: 'localhost'),
          once: true,
          websocket: const WebSocketConfig(enabled: false),
        ),
      );
      final session = await accept;
      client.session.cancel(code: 0);
      session.cancel(code: 0);
    } finally {
      server.close();
    }
  });
}
