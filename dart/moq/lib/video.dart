/// Native video encoding, publication, and decoding.
///
/// Import it with a prefix, since `Frame` and `Codec` mean something else in
/// the other libraries:
///
/// ```dart
/// import 'package:moq/video.dart' as video;
/// ```
library;

import 'package:moq_ffi/moq_ffi.dart';

/// Encodes raw pictures and publishes them as a track; construct it from the broadcast's `CatalogProducer`.
typedef Producer = MoqVideoProducer;

/// Encodes raw pictures without publishing them.
typedef Encoder = MoqVideoEncoder;

/// Subscribes to a catalog video rendition and yields each [Read]; open one with `Consumer.subscribe`.
typedef Consumer = MoqVideoConsumer;

/// One item a [Consumer] reads, in delivery order: [ReadFrame], [ReadGap], or [ReadEnd].
typedef Read = MoqVideoRead;

/// A decoded picture; `dispose()` its frame promptly, since held frames stall the decoder.
typedef ReadFrame = FrameMoqVideoRead;

/// Media after what was already read was lost; what follows does not continue it.
typedef ReadGap = GapMoqVideoRead;

/// The publisher stopped its encoder; what follows comes from a fresh encoder.
typedef ReadEnd = EndMoqVideoRead;

/// The pixel layout, resolution, and framerate written to a [Producer].
typedef EncoderInput = MoqVideoEncoderInput;

/// The codec, bitrate, group mode, and backend preference.
typedef EncoderOutput = MoqVideoEncoderOutput;

/// What a [Consumer] delivers: an optional resize, a max delay, and whether to keep the decoder's surface (macOS and iOS).
typedef DecoderOutput = MoqVideoDecoderOutput;

/// One raw picture with its presentation timestamp.
typedef Frame = MoqVideoFrame;

/// A decoded picture from a [Consumer]; `dispose()` it promptly, since held frames stall the decoder.
typedef DecodedFrame = MoqVideoDecodedFrame;

/// The platform image backing a [DecodedFrame], when [DecoderOutput] asked for one.
typedef Surface = MoqVideoSurface;

/// A borrowed `CVPixelBufferRef` address (macOS and iOS), valid while its [DecodedFrame] lives.
typedef PixelBuffer = PixelBufferMoqVideoSurface;

/// One encoded access unit from an [Encoder].
typedef Packet = MoqVideoPacket;

/// How pictures are split into independently decodable groups.
typedef Gop = MoqVideoGop;

/// A keyframe every `interval` pictures.
typedef Keyframe = KeyframeMoqVideoGop;

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

/// Duration view over a raw picture's timestamp.
extension VideoFrameDuration on MoqVideoFrame {
  /// Presentation timestamp.
  Duration get timestamp => Duration(microseconds: timestampUs);
}

/// Duration view over a decoded picture's timestamp.
extension VideoDecodedFrameDuration on MoqVideoDecodedFrame {
  /// Presentation timestamp.
  Duration get timestamp => Duration(microseconds: timestampUs());
}

/// Duration view over a gap.
extension VideoReadGapDuration on GapMoqVideoRead {
  /// The furthest presentation point already read.
  Duration get timestamp => Duration(microseconds: timestampUs);

  /// Upper bound on how long until the next picture resumes.
  Duration get duration => Duration(microseconds: durationUs);
}

/// Duration view over an encoder end.
extension VideoReadEndDuration on EndMoqVideoRead {
  /// Where the stopped encoder's media stops.
  Duration get end => Duration(microseconds: endUs);
}

/// Duration view over a decoder's latency bound.
extension VideoDecoderOutputDuration on MoqVideoDecoderOutput {
  /// Upper bound on buffering before a stalled group is skipped, or null for the default.
  Duration? get maxDelay =>
      maxDelayUs == null ? null : Duration(microseconds: maxDelayUs!);
}

/// Input, codec settings, and publication ownership for a producer.
typedef ProducerOptions = MoqVideoProducerOptions;

/// An exact video frame rate in frames per second.
typedef FrameRate = MoqVideoFrameRate;
