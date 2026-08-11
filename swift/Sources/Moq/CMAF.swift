import Foundation
import MoqFFI

/// CMAF data produced from one batch of encoded media frames.
public struct CMAFOutput: Sendable {
    /// The current initialization segment, once inline codec metadata is available.
    public let initialization: Data?
    /// The media fragment, or `nil` when the batch produced no usable samples.
    public let fragment: Data?
}

/// Muxes one encoded media rendition into CMAF initialization and media segments.
public final class CMAFMuxer: Sendable {
    private let ffi: MoqCmafMuxer

    /// Creates a video muxer whose output timestamps are relative to `originTimestampUs`.
    ///
    /// Use the same origin for independently fetched audio and video so their fragments share
    /// one zero-based presentation timeline.
    public init(video: Video, originTimestampUs: UInt64 = 0) throws {
        self.ffi = try MoqCmafMuxer(
            config: MoqCmafConfig(
                track: .video(config: video),
                originUs: originTimestampUs
            )
        )
    }

    /// Creates an audio muxer whose output timestamps are relative to `originTimestampUs`.
    ///
    /// Use the same origin for independently fetched audio and video so their fragments share
    /// one zero-based presentation timeline.
    public init(audio: Audio, originTimestampUs: UInt64 = 0) throws {
        self.ffi = try MoqCmafMuxer(
            config: MoqCmafConfig(
                track: .audio(config: audio),
                originUs: originTimestampUs
            )
        )
    }

    /// Returns the current initialization segment, or `nil` until inline codec metadata arrives.
    public var initialization: Data? {
        get throws {
            try ffi.initSegment()
        }
    }

    /// Normalizes and encodes a frame batch on the configured presentation timeline.
    ///
    /// The returned initialization matches the returned fragment. Inline H.264/H.265 parameter
    /// sets are absorbed before the fragment is encoded. Split batches at codec reconfiguration
    /// boundaries; the thrown error reports the first frame using the new configuration.
    public func mux(sequence: UInt32, frames: [MediaFrame]) throws -> CMAFOutput {
        let output = try ffi.mux(sequence: sequence, frames: frames)
        return CMAFOutput(
            initialization: output.initialization,
            fragment: output.fragment
        )
    }
}
