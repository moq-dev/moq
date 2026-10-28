import 'dart:async';

import 'package:moq_ffi/moq_ffi.dart';

/// A single listener that owns each pending native read and one buffered result.
abstract class NativeStream<T> extends Stream<T> {
  late final Stream<T> _events = _create();

  /// Start the next native read.
  UniffiRustFuture<T> read();

  /// Values to deliver before reading.
  Iterable<T> initial() => Iterable<T>.empty();

  /// Whether this value ends the source instead of being delivered.
  bool finished(T value) => false;

  /// Whether this error is the source's normal end.
  bool ended(Object error) => false;

  /// Release a value that cancellation prevented from being delivered.
  void discard(T value) {}

  /// Release resources owned by this listener after its pending read ends.
  void release() {}

  Stream<T> _create() {
    late final StreamController<T> controller;
    Future<void>? running;
    UniffiRustFuture<T>? pending;
    Completer<void>? resumed;
    var stopped = false;

    Future<void> run() async {
      try {
        if (stopped) return;
        for (final value in initial()) {
          if (resumed != null) await resumed!.future;
          if (stopped) {
            discard(value);
            return;
          }
          controller.add(value);
        }
        while (!stopped) {
          if (resumed != null) await resumed!.future;
          if (stopped) return;
          final next = read();
          pending = next;
          final T value;
          try {
            value = await next;
          } finally {
            pending = null;
          }
          if (resumed != null) await resumed!.future;
          if (stopped) {
            discard(value);
            return;
          }
          if (finished(value)) return;
          controller.add(value);
        }
      } catch (error, stack) {
        if (!stopped && !ended(error)) controller.addError(error, stack);
      } finally {
        try {
          release();
        } finally {
          // close waits for onCancel, which waits for this read loop to finish.
          unawaited(controller.close());
        }
      }
    }

    controller = StreamController<T>(
      sync: true,
      onListen: () => running = Future<void>.microtask(run),
      onPause: () => resumed ??= Completer<void>(),
      onResume: () {
        resumed?.complete();
        resumed = null;
      },
      onCancel: () {
        stopped = true;
        unawaited(pending?.cancel());
        resumed?.complete();
        resumed = null;
        return running;
      },
    );
    return controller.stream;
  }

  @override
  StreamSubscription<T> listen(
    void Function(T)? onData, {
    Function? onError,
    void Function()? onDone,
    bool? cancelOnError,
  }) => _events.listen(
    onData,
    onError: onError,
    onDone: onDone,
    cancelOnError: cancelOnError,
  );
}
