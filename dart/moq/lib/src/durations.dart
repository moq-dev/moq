import 'package:moq_ffi/moq_ffi.dart';

// The FFI surface carries microseconds as plain integers, because not every
// target language has a duration type. Dart does, so read them back as one.
// These are extensions, so the `*Us` fields stay available for anyone building
// a record to send back across the boundary.

/// Duration views over the transport statistics.
extension ConnectionStatsDuration on MoqConnectionStats {
  /// Smoothed round-trip time, or null when the transport has no estimate yet.
  Duration? get rtt => rttUs == null ? null : Duration(microseconds: rttUs!);
}

/// Duration views over the reconnect pacing.
extension BackoffDuration on MoqBackoff {
  /// Delay before the first reconnect attempt.
  Duration get initial => Duration(microseconds: initialUs);

  /// Maximum delay between reconnect attempts.
  Duration get max => Duration(microseconds: maxUs);

  /// Time spent retrying before giving up. [Duration.zero] retries forever.
  Duration get timeout => Duration(microseconds: timeoutUs);
}

/// Duration views over the subscription knobs.
extension SubscriptionDuration on MoqSubscription {
  /// Upper bound on buffering before a stalled group is skipped.
  Duration get maxAge => Duration(microseconds: maxAgeUs);
}

/// Duration views over the publisher-side track settings.
extension TrackInfoDuration on MoqTrackInfo {
  /// Maximum age of a non-latest group before the publisher evicts it, or null for the default.
  Duration? get maxAge =>
      maxAgeUs == null ? null : Duration(microseconds: maxAgeUs!);
}

/// Duration view over a raw frame's presentation time.
extension FrameDuration on MoqFrame {
  /// Presentation timestamp.
  Duration get timestamp => Duration(microseconds: timestampUs);
}

/// Duration view over a media frame's presentation time.
extension MediaFrameDuration on MoqMediaFrame {
  /// Presentation timestamp.
  Duration get timestamp => Duration(microseconds: timestampUs);
}

/// Duration view over a datagram's presentation time.
extension DatagramDuration on MoqDatagram {
  /// Presentation timestamp.
  Duration get timestamp => Duration(microseconds: timestampUs);
}
