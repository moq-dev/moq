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

/// Subscribes to a catalog audio rendition and yields each [Read]; open one with `Consumer.subscribe`.
typedef Consumer = MoqAudioConsumer;

/// One item a [Consumer] reads, in delivery order: [ReadFrame], [ReadGap], or [ReadEnd].
typedef Read = MoqAudioRead;

/// Decoded PCM.
typedef ReadFrame = FrameMoqAudioRead;

/// Media after what was already read was lost; what follows does not continue it.
typedef ReadGap = GapMoqAudioRead;

/// The publisher stopped its encoder; frames starting before `endUs` are its remaining media.
typedef ReadEnd = EndMoqAudioRead;

/// The PCM layout written to a [Producer].
typedef EncoderInput = MoqAudioEncoderInput;

/// The codec side: codec, rate, channels, bitrate, and frame duration (0 takes the codec's own frame).
typedef EncoderOutput = MoqAudioEncoderOutput;

/// The PCM layout a [Consumer] delivers, how stale a group may get, and its playout delay.
typedef DecoderOutput = MoqAudioDecoderOutput;

/// PCM in the configured [SampleFormat], planar or interleaved, with its presentation timestamp.
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

/// Duration view over a gap.
extension AudioReadGapDuration on GapMoqAudioRead {
  /// The furthest presentation point already read.
  Duration get timestamp => Duration(microseconds: timestampUs);
}

/// Duration view over an encoder end.
extension AudioReadEndDuration on EndMoqAudioRead {
  /// Where the stopped encoder's media stops.
  Duration get end => Duration(microseconds: endUs);
}

/// Input, codec settings, and publication ownership for a producer.
typedef ProducerOptions = MoqAudioProducerOptions;
