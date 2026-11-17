import 'package:moq_ffi/moq_ffi.dart';

/// The platform image backing a `DecodedFrame`, when `DecoderOutput` asked for one: a
/// [PixelBuffer] on macOS and iOS, or an [UnknownSurface] for a kind this version cannot name yet.
typedef Surface = MoqVideoSurface;

/// A borrowed `CVPixelBufferRef` address (macOS and iOS), valid while its `DecodedFrame` lives.
typedef PixelBuffer = PixelBufferMoqVideoSurface;

/// A surface kind this version of the package cannot name yet.
typedef UnknownSurface = UnknownMoqVideoSurface;
