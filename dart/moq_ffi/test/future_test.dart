import 'dart:ffi';

import 'package:moq_ffi/moq_ffi.dart';
import 'package:test/test.dart';

final class _NativeFuture {
  late Pointer<NativeFunction<UniffiRustFutureContinuationCallback>> callback;
  late Pointer<Void> callbackData;
  var waiting = false;
  var cancelled = false;
  var ready = false;
  var polls = 0;
  var cancels = 0;
  var completes = 0;
  var frees = 0;

  void signal(int result) => callback.asFunction<void Function(int, int)>()(
    callbackData.address,
    result,
  );

  UniffiRustFuture<int> start() => uniffiRustCallAsync<int, int>(
    () => Pointer<Void>.fromAddress(1),
    (handle, next, data) {
      polls++;
      callback = next;
      callbackData = data;
      if (cancelled || ready) {
        signal(UNIFFI_RUST_FUTURE_POLL_READY);
      } else {
        waiting = true;
      }
    },
    (handle, status) {
      completes++;
      status.ref.code = ready ? CALL_SUCCESS : CALL_CANCELLED;
      return 42;
    },
    (handle) {
      cancels++;
      cancelled = true;
      if (waiting) {
        waiting = false;
        signal(UNIFFI_RUST_FUTURE_POLL_READY);
      }
    },
    (handle) {
      frees++;
      if (waiting) {
        waiting = false;
        signal(UNIFFI_RUST_FUTURE_POLL_READY);
      }
    },
    (value) => value,
  );

  void wake() {
    waiting = false;
    signal(UNIFFI_RUST_FUTURE_POLL_MAYBE_READY);
  }

  void deliver() {
    ready = true;
    waiting = false;
    signal(UNIFFI_RUST_FUTURE_POLL_READY);
  }
}

void main() {
  test(
    'idle future cancellation waits for exactly one native cleanup',
    () async {
      final native = _NativeFuture();
      final future = native.start();
      final error = expectLater(
        future,
        throwsA(isA<UniffiCancelledException>()),
      );
      await Future.wait([future.cancel(), future.cancel()]);
      await error;
      expect(native.cancels, 1);
      expect(native.completes, 1);
      expect(native.frees, 1);
      await future.cancel();
      expect(native.cancels, 1);
    },
  );

  test('a queued wake after cancellation repolls before cleanup', () async {
    final native = _NativeFuture();
    final future = native.start();
    final error = expectLater(future, throwsA(isA<UniffiCancelledException>()));
    native.wake();
    await future.cancel();
    await error;
    expect(native.polls, 2);
    expect(native.completes, 1);
    expect(native.frees, 1);
  });

  test('an already ready result wins the cancellation race', () async {
    final native = _NativeFuture();
    final future = native.start();
    native.deliver();
    await future.cancel();
    expect(await future, 42);
    expect(native.cancels, 1);
    expect(native.completes, 1);
    expect(native.frees, 1);
  });

  test(
    'an invalid poll releases native state before closing its callback',
    () async {
      final native = _NativeFuture();
      final future = native.start();
      final error = expectLater(future, throwsA(isA<UniffiInternalError>()));
      native.signal(123);
      await error;
      expect(native.completes, 0);
      expect(native.frees, 1);
      await future.cancel();
      expect(native.cancels, 0);
    },
  );

  test('cancelling a real native read leaves its owner usable', () async {
    final origin = MoqOriginProducer(config: MoqOriginConfig());
    final consumer = origin.consume();
    final cursor = consumer.announced(config: MoqAnnounceConfig());
    try {
      final pending = cursor.next();
      final error = expectLater(
        pending,
        throwsA(isA<UniffiCancelledException>()),
      );
      await pending.cancel();
      await error;
      final next = cursor.next();
      final publication = origin.createBroadcast(path: 'still-listening');
      try {
        publication.announce(route: MoqAnnounceRoute());
        expect(await next, isA<StartMoqAnnounceEvent>());
      } finally {
        publication.broadcast().close();
      }
    } finally {
      cursor.cancel();
      cursor.dispose();
      consumer.dispose();
      origin.dispose();
    }
  });
}
