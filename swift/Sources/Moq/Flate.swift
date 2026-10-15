import Foundation
import MoqFFI

/// Opaque snapshot and stream tracks, mirroring the moq-flate crate.
public enum Flate {}

extension Flate {
    /// Publishes the latest opaque payload on a track it takes over.
    public final class SnapshotProducer: Sendable {
        let ffi: MoqFlateSnapshotProducer

        /// Take over the track and advertise its compression and media type in the catalog.
        public init(broadcast: BroadcastProducer, track: TrackProducer, compression: Bool = false, mime: String? = nil) throws {
            ffi = try MoqFlateSnapshotProducer(broadcast: broadcast.ffi, track: track.ffi, config: MoqFlateConfig(compression: compression, mime: mime))
        }

        /// Replace the payload at the given broadcast time, or leave it untimed when nil.
        public func update(_ payload: Data, timestampUs: UInt64? = nil) throws {
            try ffi.update(payload: payload, timestampUs: timestampUs)
        }

        /// Close the track and retire its catalog entry.
        public func finish() throws { try ffi.finish() }
    }

    /// Publishes an ordered opaque payload log on a track it takes over.
    public final class StreamProducer: Sendable {
        let ffi: MoqFlateStreamProducer

        /// Take over the track and advertise its compression and media type in the catalog.
        public init(broadcast: BroadcastProducer, track: TrackProducer, compression: Bool = false, mime: String? = nil) throws {
            ffi = try MoqFlateStreamProducer(broadcast: broadcast.ffi, track: track.ffi, config: MoqFlateConfig(compression: compression, mime: mime))
        }

        /// Append a payload at the given broadcast time, or leave it untimed when nil.
        public func append(_ payload: Data, timestampUs: UInt64? = nil) throws {
            try ffi.append(payload: payload, timestampUs: timestampUs)
        }

        /// Close the track and retire its catalog entry.
        public func finish() throws { try ffi.finish() }
    }
}
