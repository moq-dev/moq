import 'package:moq_ffi/moq_ffi.dart';

// The FFI surface carries media timestamps as microsecond integers; durations
// already cross as Dart's Duration. Read the timestamps back as a Duration.
// These are extensions, so the `*Us` fields stay available for anyone building
// a record to send back across the boundary.

/// Duration view over a raw frame's presentation time.
extension FrameDuration on MoqFrame {
  /// Presentation timestamp, or null for an untimed frame.
  Duration? get timestamp =>
      timestampUs == null ? null : Duration(microseconds: timestampUs!);
}

/// Duration view over a datagram's presentation time.
extension DatagramDuration on MoqDatagram {
  /// Presentation timestamp, or null for an untimed datagram.
  Duration? get timestamp =>
      timestampUs == null ? null : Duration(microseconds: timestampUs!);
}
