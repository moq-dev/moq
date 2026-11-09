/// Native video encoding, publication, and decoding.
///
/// Import it with a prefix, since `Frame` and `Codec` mean something else in
/// the other libraries:
///
/// ```dart
/// import 'package:moq/video.dart' as video;
/// ```
library;

import 'dart:async';
import 'dart:typed_data';

import 'package:moq_ffi/moq_ffi.dart';

import 'src/surface.dart';

export 'src/surface.dart' show PixelBuffer, Surface, UnknownSurface;

/// Encodes raw pictures and publishes them as a track; construct it from the broadcast's `CatalogProducer`.
typedef Producer = MoqVideoProducer;

/// Encodes raw pictures without publishing them.
typedef Encoder = MoqVideoEncoder;

/// The pixel layout, resolution, and framerate written to a [Producer].
typedef EncoderInput = MoqVideoEncoderInput;

/// The codec, bitrate, group mode, and backend preference.
typedef EncoderOutput = MoqVideoEncoderOutput;

/// What a [Consumer] delivers: an optional resize, a delay, and whether to keep the decoder's surface (macOS and iOS).
typedef DecoderOutput = MoqVideoDecoderOutput;

/// One raw picture with its presentation timestamp.
typedef Frame = MoqVideoFrame;

/// One encoded access unit from an [Encoder].
typedef Packet = MoqVideoPacket;

/// H.264 or H.265.
typedef Codec = MoqVideoCodec;

/// A CPU pixel layout (I420 or RGBA).
typedef PixelFormat = MoqVideoPixelFormat;

/// The encoder backend preference.
typedef EncoderKind = MoqVideoEncoderKind;

/// Prefer a platform hardware encoder, falling back to software.
typedef AutoEncoder = AutoMoqVideoEncoderKind;

/// Hardware only; fails if none is available.
typedef HardwareEncoder = HardwareMoqVideoEncoderKind;

/// Software only (openh264, H.264 only).
typedef SoftwareEncoder = SoftwareMoqVideoEncoderKind;

/// A specific backend by name, such as `videotoolbox`, `mediafoundation`, or `openh264`.
typedef NamedEncoder = NamedMoqVideoEncoderKind;

/// Input, codec settings, and publication ownership for a producer.
typedef ProducerOptions = MoqVideoProducerOptions;

/// An exact video frame rate in frames per second.
typedef FrameRate = MoqVideoFrameRate;

/// Subscribes to a catalog video rendition and yields each [Read]; open one with [Consumer.subscribe].
final class Consumer {
  final MoqVideoConsumer _inner;

  Consumer._(this._inner);

  /// Subscribe to the catalog video rendition [id] and decode it.
  static UniffiRustFuture<Consumer> subscribe({
    required MoqBroadcastConsumer broadcast,
    required String id,
    required MoqVideo catalogVideo,
    required DecoderOutput output,
  }) => _MappedFuture(
    MoqVideoConsumer.subscribe(
      broadcast: broadcast,
      id: id,
      catalogVideo: catalogVideo,
      output: output,
    ),
    Consumer._,
  );

  /// The next frame, gap, or end, or null once the track ends; cancelling the future aborts only this read.
  UniffiRustFuture<Read?> next() => _MappedFuture(
    _inner.next(),
    (read) => read == null ? null : Read._from(read),
  );

  /// Cancel all current and future reads.
  void cancel() => _inner.cancel();

  /// Release the native consumer.
  void dispose() => _inner.dispose();
}

/// One item a [Consumer] reads, in delivery order: [ReadFrame], [ReadGap], or [ReadEnd].
sealed class Read {
  const Read._();

  static Read _from(MoqVideoRead read) => switch (read) {
    FrameMoqVideoRead(:final frame) => ReadFrame._(DecodedFrame._(frame)),
    GapMoqVideoRead(:final timestamp, :final duration) => ReadGap._(
      timestamp,
      duration,
    ),
    EndMoqVideoRead(:final end) => ReadEnd._(end),
    _ => throw StateError('unknown video read: $read'),
  };
}

/// A decoded picture; `dispose()` its frame promptly, since held frames stall the decoder.
final class ReadFrame extends Read {
  /// The decoded picture.
  final DecodedFrame frame;

  const ReadFrame._(this.frame) : super._();
}

/// Media after what was already read was lost; what follows does not continue it.
final class ReadGap extends Read {
  /// The furthest presentation point already read.
  final MoqTimestamp timestamp;

  /// An upper bound on how long until the next picture resumes.
  final Duration duration;

  const ReadGap._(this.timestamp, this.duration) : super._();
}

/// The publisher stopped its encoder; what follows comes from a fresh encoder.
final class ReadEnd extends Read {
  /// Where the stopped encoder's media stops.
  final MoqTimestamp end;

  const ReadEnd._(this.end) : super._();
}

/// A decoded picture from a [Consumer]; `dispose()` it promptly, since held frames stall the decoder.
final class DecodedFrame {
  final MoqVideoDecodedFrame _inner;

  DecodedFrame._(this._inner);

  /// The presentation timestamp.
  MoqTimestamp timestamp() => _inner.timestamp();

  /// The decoded width in pixels; `resize` is only a hint.
  int width() => _inner.width();

  /// The decoded height in pixels.
  int height() => _inner.height();

  /// The picture converted to tightly packed CPU pixels in [format].
  Uint8List pixels({required PixelFormat format}) =>
      _inner.pixels(format: format);

  /// The decoder's platform surface, or null when the frame is in CPU memory.
  Surface? surface() => surfaceFrom(_inner.surface());

  /// Release the frame's surface back to the decoder.
  void dispose() => _inner.dispose();
}

/// A native future mapped through [_map], still cancellable at its source.
final class _MappedFuture<S, T> implements UniffiRustFuture<T> {
  final UniffiRustFuture<S> _source;
  final Future<T> _future;

  _MappedFuture(this._source, T Function(S) map) : _future = _source.then(map);

  @override
  Future<void> cancel() => _source.cancel();

  @override
  Stream<T> asStream() => _future.asStream();

  @override
  Future<T> catchError(Function onError, {bool Function(Object error)? test}) =>
      _future.catchError(onError, test: test);

  @override
  Future<R> then<R>(
    FutureOr<R> Function(T value) onValue, {
    Function? onError,
  }) => _future.then(onValue, onError: onError);

  @override
  Future<T> timeout(Duration timeLimit, {FutureOr<T> Function()? onTimeout}) =>
      _future.timeout(timeLimit, onTimeout: onTimeout);

  @override
  Future<T> whenComplete(FutureOr<void> Function() action) =>
      _future.whenComplete(action);
}
