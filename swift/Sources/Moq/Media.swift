import Foundation
import MoqFFI

/// Catalogs, encoded media imports, and container consumers.
public enum Media {
    /// A media frame whose keyframe flag marks a group start or video keyframe; audio flags only group starts.
    public typealias MediaFrame = MoqFFI.MoqMediaFrame
    /// The JSON manifest describing a broadcast's tracks: video and audio
    /// renditions, display geometry, and untyped application sections.
    public typealias Catalog = MoqFFI.MoqCatalog
    /// A video rendition in the catalog: codec, dimensions, bitrate, whether it is
    /// enabled, framerate, and container.
    public typealias Video = MoqFFI.MoqVideo
    /// Caller-provided catalog fields for a video track.
    public typealias VideoHint = MoqFFI.MoqVideoHint
    /// A single audio codec an importer can parse.
    public typealias AudioFormat = MoqFFI.MoqAudioFormat
    /// A single video codec an importer can parse.
    public typealias VideoFormat = MoqFFI.MoqVideoFormat
    /// A container that publishes its own tracks.
    public typealias ContainerFormat = MoqFFI.MoqContainerFormat
    /// Catalog properties shared by every video rendition. A `nil` field clears
    /// that property from the next catalog snapshot.
    public typealias VideoProperties = MoqFFI.MoqVideoProperties
    /// An audio rendition in the catalog: codec, sample rate, channel count,
    /// bitrate, whether it is enabled, and container.
    public typealias Audio = MoqFFI.MoqAudio
    /// A width and height in pixels.
    public typealias Dimensions = MoqFFI.MoqDimensions
    /// How a track's frames are packaged (Legacy, CMAF, or LOC), as advertised in
    /// the catalog.
    public typealias Container = MoqFFI.MoqContainer

    /// The named or requested track an importer publishes.
    public enum Target {
        /// A new track with an explicit name, or a format-derived unique name.
        case named(name: String?)
        /// A subscriber-requested track.
        case requested(TrackRequest)
        var ffi: MoqMediaTarget {
            switch self {
            case .named(let name): return .named(name: name)
            case .requested(let request): return .requested(request: request.ffi)
            }
        }
    }
    /// An audio codec, its initialization bytes, and label.
    public typealias AudioInit = MoqAudioInit
    /// A video codec, its initialization bytes, label, and hints.
    public typealias VideoInit = MoqVideoInit
    /// A container format and its leading bytes.
    public typealias ContainerInit = MoqContainerInit
    /// A track name, its container, and live delivery options.
    public typealias ContainerConfig = MoqMediaContainerConfig
    /// A fetched group's name, sequence, container, and delivery options.
    public typealias ContainerGroupConfig = MoqMediaContainerGroupConfig

    /// Updates a catalog without keeping its broadcast open.
    public final class CatalogProducer: Sendable {
        let ffi: MoqMediaCatalogProducer
        /// Construct a weak catalog handle for this broadcast.
        public init(broadcast: BroadcastProducer) throws { ffi = try MoqMediaCatalogProducer(broadcast: broadcast.ffi) }
        /// Replace the video properties shared by every rendition.
        public func setVideoProperties(_ properties: VideoProperties) throws { try ffi.setVideoProperties(properties: properties) }
        /// Set an application catalog section from its serialized JSON value.
        public func setSection(name: String, json: String) throws { try ffi.setSection(name: name, json: json) }
        /// Remove an application catalog section if present.
        public func removeSection(name: String) throws { try ffi.removeSection(name: name) }
    }

    /// Read side of a broadcast's catalog. Iterating yields catalog updates as the
    /// set of tracks changes.
    public final class CatalogConsumer: AsyncSequence, Sendable {
        /// The catalog update emitted by this sequence.
        public typealias Element = Catalog

        let ffi: MoqMediaCatalogConsumer

        /// Subscribe to this broadcast's catalog snapshots.
        public static func subscribe(broadcast: BroadcastConsumer) async throws -> CatalogConsumer {
            CatalogConsumer(try await MoqMediaCatalogConsumer.subscribe(broadcast: broadcast.ffi))
        }
        init(_ ffi: MoqMediaCatalogConsumer) {
            self.ffi = ffi
        }

        /// The next catalog update, or `nil` once the track ends or is closed.
        public func next() async throws -> Catalog? {
            try await ffi.next()
        }

        /// Cancel all current and future reads.
        public func cancel() {
            ffi.cancel()
        }

        /// Create an iterator that cancels native reads when iteration ends.
        public func makeAsyncIterator() -> AsyncThrowingStream<Catalog, Swift.Error>.Iterator {
            moqStream(cancel: { [ffi] in ffi.cancel() }) { [ffi] in
                try await ffi.next()
            }.makeAsyncIterator()
        }
    }

    /// Read side of a media track. Iterating yields decoded frames in decode order.
    public final class ContainerConsumer: AsyncSequence, Sendable {
        /// The decoded media frame emitted by this sequence.
        public typealias Element = MediaFrame

        let ffi: MoqMediaContainerConsumer

        /// Subscribe to a track and decode its container.
        public static func subscribe(broadcast: BroadcastConsumer, config: ContainerConfig) async throws -> ContainerConsumer {
            ContainerConsumer(try await MoqMediaContainerConsumer.subscribe(broadcast: broadcast.ffi, config: config))
        }
        init(_ ffi: MoqMediaContainerConsumer) {
            self.ffi = ffi
        }

        /// The next frame, or `nil` once the track ends or is closed.
        public func next() async throws -> MediaFrame? {
            try await ffi.next()
        }

        /// Cancel all current and future reads.
        public func cancel() {
            ffi.cancel()
        }

        /// Create an iterator that cancels native reads when iteration ends.
        public func makeAsyncIterator() -> AsyncThrowingStream<MediaFrame, Swift.Error>.Iterator {
            moqStream(cancel: { [ffi] in ffi.cancel() }) { [ffi] in
                try await ffi.next()
            }.makeAsyncIterator()
        }
    }

    /// A finite, container-decoded media group returned by ``ContainerGroupConsumer/fetch(broadcast:config:)``.
    public final class ContainerGroupConsumer: AsyncSequence, Sendable {
        /// The decoded media frame emitted by this sequence.
        public typealias Element = MediaFrame

        let ffi: MoqMediaContainerGroupConsumer

        /// Fetch and decode exactly one media group.
        public static func fetch(broadcast: BroadcastConsumer, config: ContainerGroupConfig) async throws -> ContainerGroupConsumer {
            ContainerGroupConsumer(try await MoqMediaContainerGroupConsumer.fetch(broadcast: broadcast.ffi, config: config))
        }
        init(_ ffi: MoqMediaContainerGroupConsumer) {
            self.ffi = ffi
        }

        /// The sequence number of this group within the track.
        public var sequence: UInt64 {
            ffi.sequence()
        }

        /// The next frame, or `nil` once the group ends.
        public func next() async throws -> MediaFrame? {
            try await ffi.next()
        }

        /// Cancel all current and future reads.
        public func cancel() {
            ffi.cancel()
        }

        /// Create an iterator that cancels native reads when iteration ends.
        public func makeAsyncIterator() -> AsyncThrowingStream<MediaFrame, Swift.Error>.Iterator {
            moqStream(cancel: { [ffi] in ffi.cancel() }) { [ffi] in
                try await ffi.next()
            }.makeAsyncIterator()
        }
    }

    /// Write side of a media track fed pre-framed payloads.
    public final class TrackProducer: Sendable {
        let ffi: MoqMediaTrackProducer

        /// Import complete audio frames on a named or requested track.
        public static func audio(broadcast: BroadcastProducer, initData: AudioInit, target: Target = .named(name: nil)) throws -> TrackProducer {
            TrackProducer(try MoqMediaTrackProducer.audio(broadcast: broadcast.ffi, target: target.ffi, init: initData))
        }
        /// Import complete video frames on a named or requested track.
        public static func video(broadcast: BroadcastProducer, initData: VideoInit, target: Target = .named(name: nil)) throws -> TrackProducer {
            TrackProducer(try MoqMediaTrackProducer.video(broadcast: broadcast.ffi, target: target.ffi, init: initData))
        }
        init(_ ffi: MoqMediaTrackProducer) {
            self.ffi = ffi
        }

        /// A watch-only handle to whether the track has subscribers.
        public func demand() throws -> TrackDemand {
            TrackDemand(try ffi.demand())
        }

        /// Write a frame with the given presentation timestamp (microseconds).
        ///
        /// The importer derives keyframe status from the bitstream, so only the payload and its
        /// timestamp cross the boundary.
        public func writeFrame(_ payload: Data, timestampUs: UInt64 = 0) throws {
            try ffi.writeFrame(frame: Frame(payload: payload, timestampUs: timestampUs))
        }

        /// Record a local encoder's frame handoff on the broadcast media clock.
        /// Call after `writeFrame` only for local encoder output.
        public func flush(timestampUs: UInt64) throws {
            try ffi.flush(timestampUs: timestampUs)
        }

        /// Mark a timeline break and restart handoff measurement, preserving advertised jitter.
        public func discontinuity() throws {
            try ffi.discontinuity()
        }

        /// Draw a group boundary here.
        ///
        /// Audio has no boundary of its own (every packet is independently decodable), so this is the
        /// only thing that gives it groups: call it after every frame for one group (one QUIC stream)
        /// the relay forwards without waiting, or at a segment cadence to align with video. Video
        /// groups at its own keyframes and needs this only to override that.
        public func cut() throws {
            try ffi.cut()
        }

        /// Draw a group boundary and number the next group `sequence`.
        ///
        /// ``cut()`` with an explicit sequence, for a publisher whose group numbers have to be
        /// deterministic: two encoders aligning per GOP so a consumer can fail over between them.
        public func seek(_ sequence: UInt64) throws {
            try ffi.seek(sequence: sequence)
        }

        /// Finish the track and finalize encoding.
        public func finish() throws {
            try ffi.finish()
        }
    }

    /// Write side of a container, which demuxes and publishes its own tracks.
    public final class ContainerProducer: Sendable {
        let ffi: MoqMediaContainerProducer

        /// Import complete container chunks into this broadcast.
        public convenience init(broadcast: BroadcastProducer, initData: ContainerInit) throws {
            self.init(try MoqMediaContainerProducer(broadcast: broadcast.ffi, init: initData))
        }
        init(_ ffi: MoqMediaContainerProducer) {
            self.ffi = ffi
        }

        /// Write a whole chunk of container bytes.
        ///
        /// No timestamp: a container carries its tracks' timing itself, and the importer reads it out
        /// rather than taking the caller's word for it.
        public func write(_ payload: Data) throws {
            try ffi.write(payload: payload)
        }

        /// Declare that the next chunk starts a new segment, rolling a group on every track.
        ///
        /// An fMP4 source carrying `styp` atoms declares its own, so this is only needed when it
        /// doesn't; formats with no segment concept (MKV, TS, FLV) ignore it.
        public func cut() throws {
            try ffi.cut()
        }

        /// Start a new segment and number its groups `sequence`.
        public func seek(_ sequence: UInt64) throws {
            try ffi.seek(sequence: sequence)
        }

        /// Finish every track this container publishes.
        public func finish() throws {
            try ffi.finish()
        }
    }

    /// Write side of a container fed a raw byte stream, which recovers its own framing.
    public final class ContainerStreamProducer: Sendable {
        let ffi: MoqMediaContainerStreamProducer

        /// Import a container byte stream into this broadcast.
        public convenience init(broadcast: BroadcastProducer, format: ContainerFormat) throws {
            self.init(try MoqMediaContainerStreamProducer(broadcast: broadcast.ffi, format: format))
        }
        init(_ ffi: MoqMediaContainerStreamProducer) {
            self.ffi = ffi
        }

        /// Push raw container bytes; chunk boundaries don't matter.
        public func write(_ payload: Data) throws {
            try ffi.write(payload: payload)
        }

        /// Finish every track this container publishes.
        public func finish() throws {
            try ffi.finish()
        }
    }

    /// Write side of a media track fed a raw byte stream with inferred frame boundaries.
    public final class TrackStreamProducer: Sendable {
        let ffi: MoqMediaTrackStreamProducer

        /// Import a video byte stream, inferring frame boundaries.
        public static func video(broadcast: BroadcastProducer, initData: VideoInit, target: Target = .named(name: nil)) throws -> TrackStreamProducer {
            TrackStreamProducer(try MoqMediaTrackStreamProducer.video(broadcast: broadcast.ffi, target: target.ffi, init: initData))
        }
        init(_ ffi: MoqMediaTrackStreamProducer) {
            self.ffi = ffi
        }

        /// A watch-only handle to whether this media track has subscribers.
        public func demand() throws -> TrackDemand {
            TrackDemand(try ffi.demand())
        }

        /// Push raw stream bytes (e.g. Annex-B H.264). The importer frames whole
        /// access units and buffers any partial trailing frame for the next call.
        public func write(_ payload: Data) throws {
            try ffi.write(payload: payload)
        }

        /// Finalize the track. A trailing access unit with no following delimiter is
        /// not emitted (matches the moq-cli stdin path).
        public func finish() throws {
            try ffi.finish()
        }
    }
}
