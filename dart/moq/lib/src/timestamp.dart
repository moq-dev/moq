import 'package:moq_ffi/moq_ffi.dart';

import 'aliases.dart';

final _microsPerSecond = BigInt.from(Duration.microsecondsPerSecond);

/// Converts a [MoqTimestamp] to Dart's [Duration].
extension TimestampDuration on MoqTimestamp {
  /// This timestamp as a [Duration], rounded down to a whole microsecond.
  ///
  /// The timestamp itself is exact; keep it, not the duration, to republish a
  /// tick such as 90 kHz. Throws a [StateError] when unscaled, since a
  /// scale-less track's key is no time, and a [RangeError] when the result
  /// does not fit in a [Duration].
  Duration toDuration() {
    final scale = this.scale;
    if (scale == null) {
      throw StateError('unscaled timestamp #$value has no time');
    }
    // value * 10^6 can exceed 64 bits at the allowed bounds.
    final units = BigInt.from(scale);
    final micros = BigInt.from(value) * _microsPerSecond ~/ units;
    if (!micros.isValidInt) {
      throw RangeError('timestamp $value/$scale s does not fit in a Duration');
    }
    return Duration(microseconds: micros.toInt());
  }
}

/// Converts a [Duration] to an exact [MoqTimestamp].
extension DurationTimestamp on Duration {
  /// This duration as an exact timestamp at microsecond scale.
  ///
  /// Throws an [ArgumentError] when negative.
  Timestamp toTimestamp() {
    if (isNegative) {
      throw ArgumentError.value(this, 'duration', 'must not be negative');
    }
    return Timestamp(
      value: inMicroseconds,
      scale: Duration.microsecondsPerSecond,
    );
  }
}
