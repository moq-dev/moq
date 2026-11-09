import 'package:moq_ffi/moq_ffi.dart';

/// The platform image backing a `DecodedFrame`, when `DecoderOutput` asked for one.
///
/// Switch on it with a default case, since new platforms add kinds.
abstract class Surface {
  const Surface._();
}

/// A borrowed `CVPixelBufferRef` address (macOS and iOS), valid while its `DecodedFrame` lives.
final class PixelBuffer extends Surface {
  /// The address of the `CVPixelBufferRef`.
  final int pointer;

  const PixelBuffer._(this.pointer) : super._();
}

/// A surface kind this version of the package does not know yet.
final class UnknownSurface extends Surface {
  const UnknownSurface._() : super._();
}

/// Maps a generated surface to [Surface], so a variant this package predates is an [UnknownSurface].
Surface? surfaceFrom(MoqVideoSurface? surface) => switch (surface) {
  null => null,
  PixelBufferMoqVideoSurface(:final pointer) => PixelBuffer._(pointer),
  _ => const UnknownSurface._(),
};
