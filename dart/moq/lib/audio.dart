/// Native audio encoding, publication, and decoding.
///
/// Import it with a prefix, since `Frame` and `Codec` mean something else in
/// the other libraries:
///
/// ```dart
/// import 'package:moq/audio.dart' as audio;
/// ```
library;

import 'package:moq_ffi/moq_ffi.dart';

/// Encodes PCM and publishes it as a track; construct it from the broadcast's `CatalogProducer`.
typedef Producer = MoqAudioProducer;

/// Encodes PCM without publishing it.
typedef Encoder = MoqAudioEncoder;

/// Subscribes to a catalog audio rendition and yields decoded PCM; open one with `Consumer.subscribe`.
typedef Consumer = MoqAudioConsumer;

/// The PCM layout written to a [Producer].
typedef EncoderInput = MoqAudioEncoderInput;

/// The codec side: codec, track name, rate, channels, bitrate, and frame duration (0 takes the codec's own frame).
typedef EncoderOutput = MoqAudioEncoderOutput;

/// The PCM layout a [Consumer] delivers and how stale a group may get.
typedef DecoderOutput = MoqAudioDecoderOutput;

/// Interleaved PCM with its presentation timestamp.
typedef Frame = MoqAudioFrame;

/// One encoded payload from an [Encoder].
typedef Packet = MoqAudioPacket;

/// The packets an [Encoder] drains on finish and the padded samples to discard.
typedef EncoderFinish = MoqAudioEncoderFinish;

/// The audio codec: `Codec.opus()` or `Codec.aac()`.
typedef Codec = MoqAudioCodec;

/// A raw PCM sample layout.
typedef SampleFormat = MoqAudioSampleFormat;

/// Duration view over a PCM frame's timestamp.
extension AudioFrameDuration on MoqAudioFrame {
  /// Presentation timestamp of the first sample.
  Duration get timestamp => Duration(microseconds: timestampUs);
}

/// Duration view over an encoder's frame size.
extension AudioEncoderOutputDuration on MoqAudioEncoderOutput {
  /// Encoded frame duration; zero takes the codec's own.
  Duration get frameDuration => Duration(microseconds: frameDurationUs);
}

/// Duration view over a decoder's latency bound.
extension AudioDecoderOutputDuration on MoqAudioDecoderOutput {
  /// Upper bound on buffering before a stalled group is skipped, or null for the default.
  Duration? get maxDelay =>
      maxDelayUs == null ? null : Duration(microseconds: maxDelayUs!);
}
