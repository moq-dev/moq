import Foundation
import MoqFFI

/// Catalogs, encoded media imports, and container consumers.
public enum Media {
    /// A media frame whose keyframe flag marks a group start or video keyframe; audio flags only group starts.
    public typealias MediaFrame = MoqFFI.MoqMediaFrame
    /// One encoded frame, a gap, or an encoder end.
    public typealias Read = MoqMediaRead
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
    /// Where an audio or video decoder starts on a track that already holds
    /// groups: `.latest` for a live player.
    public typealias DecodeStart = MoqFFI.MoqDecodeStart
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
    /// Shares one source's timestamp offset without keeping its broadcast open.
    public final class Timebase: Sendable {
        let ffi: MoqMediaTimebase
        /// Create a shared source timebase owned by this broadcast.
        public init(broadcast: BroadcastProducer) throws { ffi = try MoqMediaTimebase(broadcast: broadcast.ffi) }
    }
    /// A container format, its leading bytes, and an optional shared source timebase.
    public struct ContainerInit: Sendable {
        /// The container format.
        public var format: ContainerFormat
        /// The leading container bytes.
        public var data: Data
        /// A shared source timebase, or nil for an independent offset.
        public var timebase: Timebase?
        /// Configure a container's leading bytes and source timeline.
        public init(format: ContainerFormat, data: Data, timebase: Timebase? = nil) {
            self.format = format
            self.data = data
            self.timebase = timebase
        }
        var ffi: MoqContainerInit { MoqContainerInit(format: format, data: data, timebase: timebase?.ffi) }
    }
    /// A catalog rendition: its ID (catalog key) and its video or audio config.
    public typealias Rendition = MoqRendition
    /// A catalog rendition and live delivery options.
    public typealias TrackConfig = MoqMediaTrackConfig
    /// A catalog rendition's group sequence and fetch delivery options.
    public typealias GroupConfig = MoqMediaGroupConfig

    /// Updates a catalog without keeping its broadcast open.
    public final class CatalogProducer: Sendable {
        let ffi: MoqMediaCatalogProducer
        /// Construct a weak catalog handle for this broadcast.
        public init(broadcast: BroadcastProducer) throws { ffi = try MoqMediaCatalogProducer(broadcast: broadcast.ffi) }

        /// Read the broadcast clock in microseconds, fixing it for subsequent media producers.
        public func now() throws -> UInt64 { try ffi.now() }
        /// Replace the video properties shared by every rendition.
        public func setVideoProperties(_ properties: VideoProperties) throws { try ffi.setVideoProperties(properties: properties) }
        /// Set an application catalog section from its serialized JSON value.
        public func setSection(name: String, json: String) throws { try ffi.setSection(name: name, json: json) }
        /// Remove an application catalog section if present.
        public func removeSection(name: String) throws { try ffi.removeSection(name: name) }
        /// Finish the catalog, which ends the broadcast's content.
        ///
        /// Finish every track first. The broadcast stays announced and keeps serving its finished
        /// tracks to late subscribers until its `close()` takes it offline; how long to keep it up
        /// is yours to decide.
        public func finish() throws { try ffi.finish() }
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

    /// Read side of a media track. Iterating yields encoded frames, gaps, and encoder ends.
    public final class TrackConsumer: AsyncSequence, Sendable {
        /// The media read emitted by this sequence.
        public typealias Element = Read

        let ffi: MoqMediaTrackConsumer

        /// Subscribe to a catalog rendition and decode its container, following its broadcast and track.
        public static func subscribe(broadcast: BroadcastConsumer, config: TrackConfig) async throws -> TrackConsumer {
            TrackConsumer(try await MoqMediaTrackConsumer.subscribe(broadcast: broadcast.ffi, config: config))
        }
        init(_ ffi: MoqMediaTrackConsumer) {
            self.ffi = ffi
        }

        /// The next frame, gap, or encoder end, or `nil` once the track ends or is closed.
        public func next() async throws -> Read? {
            try await ffi.next()
        }

        /// Cancel all current and future reads.
        public func cancel() {
            ffi.cancel()
        }

        /// Create an iterator that cancels native reads when iteration ends.
        public func makeAsyncIterator() -> AsyncThrowingStream<Read, Swift.Error>.Iterator {
            moqStream(cancel: { [ffi] in ffi.cancel() }) { [ffi] in
                try await ffi.next()
            }.makeAsyncIterator()
        }
    }

    /// A finite, container-decoded media group, from ``GroupConsumer/fetch(broadcast:config:)``
    /// or decoded from a raw ``Moq/GroupConsumer``.
    public final class GroupConsumer: AsyncSequence, Sendable {
        /// The decoded media frame emitted by this sequence.
        public typealias Element = MediaFrame

        let ffi: MoqMediaGroupConsumer

        /// Fetch and decode exactly one group of a catalog rendition, following its broadcast and track.
        public static func fetch(broadcast: BroadcastConsumer, config: GroupConfig) async throws -> GroupConsumer {
            GroupConsumer(try await MoqMediaGroupConsumer.fetch(broadcast: broadcast.ffi, config: config))
        }
        init(_ ffi: MoqMediaGroupConsumer) {
            self.ffi = ffi
        }

        /// Decode `group` with the track's container, as each frame lands.
        ///
        /// Pair it with ``Moq/TrackConsumer/recvGroup()`` to read media in arrival order: frame
        /// timestamps are container media time, not the transport's. Which groups arrive, and how
        /// long a stalled one stays readable, is the subscription's `maxDelay`. Takes over
        /// `group`, whose handle is closed afterward; throws if it has already read a frame.
        public convenience init(group: Moq.GroupConsumer, container: Container) throws {
            self.init(try MoqMediaGroupConsumer(group: group.ffi, container: container))
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
        public func writeFrame(_ payload: Data, timestampUs: UInt64) throws {
            try ffi.writeFrame(frame: Frame(payload: payload, timestampUs: timestampUs))
        }

        /// Mark a timeline break, preserving the advertised jitter and delay.
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

        /// Import a container into this broadcast, from chunks split anywhere.
        public convenience init(broadcast: BroadcastProducer, initData: ContainerInit) throws {
            self.init(try MoqMediaContainerProducer(broadcast: broadcast.ffi, init: initData.ffi))
        }
        init(_ ffi: MoqMediaContainerProducer) {
            self.ffi = ffi
        }

        /// Write container bytes; the container recovers its own framing.
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

        /// Finalize the track, publishing the last access unit still waiting for a
        /// following start code.
        public func finish() throws {
            try ffi.finish()
        }
    }
}
