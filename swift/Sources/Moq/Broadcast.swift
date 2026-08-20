import Foundation
import MoqFFI

/// Read side of a broadcast: subscribe to its catalog and tracks.
public final class BroadcastConsumer: Sendable {
    let ffi: MoqBroadcastConsumer

    init(_ ffi: MoqBroadcastConsumer) {
        self.ffi = ffi
    }

    /// The route the broadcast currently takes to reach this origin: relay hop
    /// ids (oldest first) plus the publisher's advertised cost (lower wins).
    public var route: Route {
        ffi.route()
    }

    /// Watch the broadcast's route: yields the current route first, then every
    /// change (e.g. an upstream failover).
    public func routeUpdates() -> RouteWatch {
        RouteWatch(ffi.routeUpdates())
    }

    /// Subscribe to the broadcast's catalog (the description of its tracks).
    public func subscribeCatalog() async throws -> CatalogConsumer {
        CatalogConsumer(try await ffi.subscribeCatalog())
    }

    /// Subscribe to a track by name, delivering raw frame payloads with no codec
    /// or container parsing. `subscription` tunes delivery priority, group ordering priority,
    /// and group range; omit for defaults.
    public func subscribeTrack(name: String, subscription: Subscription? = nil) async throws -> TrackConsumer {
        TrackConsumer(try await ffi.subscribeTrack(name: name, subscription: subscription))
    }

    /// Fetch one complete group by track name and group sequence without holding
    /// a live subscription. The group may still be receiving frames.
    public func fetchGroup(
        name: String,
        sequence: UInt64,
        options: FetchGroupOptions? = nil
    ) async throws -> GroupConsumer {
        GroupConsumer(try await ffi.fetchGroup(name: name, sequence: sequence, options: options))
    }

    /// Fetch one complete group and decode its track container into media frames.
    public func fetchMediaGroup(
        name: String,
        sequence: UInt64,
        container: Container,
        options: FetchGroupOptions? = nil
    ) async throws -> MediaGroupConsumer {
        MediaGroupConsumer(
            try await ffi.fetchMediaGroup(
                name: name,
                sequence: sequence,
                container: container,
                options: options
            )
        )
    }

    /// Subscribe to a media track, delivering frames in decode order. `container`
    /// comes from the catalog. `subscription` tunes delivery priority, group ordering
    /// priority, group range, and the max age; omit for defaults. Raise
    /// `Subscription.maxAgeMs` to buffer instead of skipping a stalled group.
    public func subscribeMedia(
        name: String,
        container: Container,
        subscription: Subscription? = nil
    ) async throws -> MediaConsumer {
        MediaConsumer(
            try await ffi.subscribeMedia(
                name: name, container: container, subscription: subscription))
    }

    /// Resolve a catalog rendition's `broadcast` reference to the broadcast serving its track.
    ///
    /// `reference` is `Video.broadcast` / `Audio.broadcast`: `nil` or empty names this
    /// broadcast, anything else names a sibling relative to it (e.g. `./source`). Call it on a
    /// rendition that carries one before `subscribeMedia`, `subscribeTrack`, `fetchGroup`, or
    /// `fetchMediaGroup`, which take a track name rather than a rendition; `decodeAudio` and
    /// `decodeVideo` resolve it themselves.
    ///
    /// Throws if this broadcast came from a local producer rather than an origin, since a
    /// standalone broadcast has no sibling to name.
    public func resolve(_ reference: String?) async throws -> BroadcastConsumer {
        BroadcastConsumer(try await ffi.resolve(reference: reference))
    }

    /// Subscribe to a raw-audio track, decoding to PCM in the layout `output`
    /// declares. `catalogAudio` is the matching rendition from the catalog.
    public func decodeAudio(name: String, catalogAudio: Audio, output: AudioDecoderOutput) async throws -> AudioConsumer {
        AudioConsumer(try await ffi.decodeAudio(name: name, catalogAudio: catalogAudio, output: output))
    }

    /// Subscribe to a video track and decode it inside the bindings, yielding
    /// packed I420. `catalogVideo` is the matching rendition from the catalog.
    ///
    /// `output.resize` is best effort, so read each frame's own dimensions.
    public func decodeVideo(
        name: String,
        catalogVideo: Video,
        output: VideoDecoderOutput = VideoDecoderOutput()
    ) async throws -> VideoConsumer {
        VideoConsumer(try await ffi.decodeVideo(name: name, catalogVideo: catalogVideo, output: output))
    }

    /// Subscribe to a JSON snapshot track (lossy latest-value), decoding each value as `Value`.
    ///
    /// Yields only the newest value, collapsing the backlog for a reader that has fallen behind.
    /// `compression` must match the flag the producer used.
    public func subscribeJsonSnapshot<Value: Decodable & Sendable>(
        name: String,
        as _: Value.Type,
        compression: Bool = false
    ) async throws -> JsonSnapshotConsumer<Value> {
        // deltaRatio is producer-only, so leave it at its default here.
        JsonSnapshotConsumer(try await ffi.subscribeJsonSnapshot(name: name, config: MoqJsonSnapshotConfig(compression: compression)))
    }

    /// Subscribe to a JSON stream track (lossless append-log), decoding each record as `Value`.
    ///
    /// Yields every record in order. `compression` must match the flag the producer used.
    public func subscribeJsonStream<Value: Decodable & Sendable>(
        name: String,
        as _: Value.Type,
        compression: Bool = false
    ) async throws -> JsonStreamConsumer<Value> {
        JsonStreamConsumer(try await ffi.subscribeJsonStream(name: name, config: MoqJsonStreamConfig(compression: compression)))
    }
}

/// A watch over a broadcast's route: an async sequence yielding the current
/// route first, then every change, ending when the broadcast does. Created by
/// `BroadcastConsumer.routeUpdates`.
public final class RouteWatch: AsyncSequence, Sendable {
    /// The route emitted by this sequence.
    public typealias Element = Route

    let ffi: MoqRouteWatch

    init(_ ffi: MoqRouteWatch) {
        self.ffi = ffi
    }

    /// Suspend until the next route: the current one on the first call, then
    /// each change. Returns nil once the broadcast ends.
    public func next() async throws -> Route? {
        try await ffi.next()
    }

    /// Stop the watch, unblocking any in-flight `next()`.
    public func cancel() {
        ffi.cancel()
    }

    /// Create an iterator that cancels native reads when iteration ends.
    public func makeAsyncIterator() -> AsyncThrowingStream<Route, Swift.Error>.Iterator {
        moqStream(cancel: { [ffi] in ffi.cancel() }) { [ffi] in
            try await ffi.next()
        }.makeAsyncIterator()
    }
}

/// Write side of a broadcast: open tracks and publish frames.
///
/// Constructing one directly creates a standalone broadcast for serving dynamic
/// requests (`BroadcastRequest.accept`) or local pub/sub. To publish at a path,
/// use `OriginProducer.createBroadcast(path:)` instead.
public final class BroadcastProducer: Sendable {
    let ffi: MoqBroadcastProducer

    /// Create a standalone broadcast for serving dynamic requests or local
    /// pub/sub. To publish at a path, use `OriginProducer.createBroadcast(path:)`.
    public init() throws {
        ffi = try MoqBroadcastProducer()
    }

    init(_ ffi: MoqBroadcastProducer) {
        self.ffi = ffi
    }

    /// A read handle for this broadcast's tracks.
    public func consume() throws -> BroadcastConsumer {
        BroadcastConsumer(try ffi.consume())
    }

    /// Accept subscriptions to tracks that are not published yet. Hold and iterate
    /// the returned `BroadcastDynamic` while such requests should be served.
    public func dynamic() throws -> BroadcastDynamic {
        BroadcastDynamic(try ffi.dynamic())
    }

    /// Update the broadcast's route: the hop chain, cost, and liveness it advertises.
    ///
    /// Use this as conditions shift (e.g. a standby transcoder lowering its cost
    /// once warm); consumers observe the change via `BroadcastConsumer.routeUpdates()`.
    public func setRoute(_ route: Route) throws {
        try ffi.setRoute(route: route)
    }

    /// Set whether the broadcast is announced, keeping the rest of its route.
    ///
    /// The origin advertises the path only while announced; an unannounced
    /// broadcast stays reachable by exact path for subscribes and fetches. This is
    /// how a publisher goes on and off the air without tearing down the broadcast.
    public func setAnnounce(_ announce: Bool) throws {
        try ffi.setAnnounce(announce: announce)
    }

    /// Replace the catalog properties shared by every video rendition.
    public func setVideoProperties(_ properties: VideoProperties) throws {
        try ffi.setVideoProperties(properties: properties)
    }

    /// Open a media track. `format` controls how `initData` and frame payloads
    /// are interpreted (e.g. `"opus"`, `"avc3"`). `video` seeds catalog fields
    /// that the stream cannot reveal before its first keyframe.
    /// Publish one audio codec as a new track. `initData` is required: audio resolves its whole
    /// rendition from those bytes.
    public func publishAudio(
        format: AudioFormat,
        initData: Data,
        label: String? = nil
    ) throws -> MediaProducer {
        MediaProducer(try ffi.publishAudio(init: MoqAudioInit(format: format, data: initData, label: label)))
    }

    /// Publish one video codec as a new track. `initData` may be empty for a format that resolves
    /// in band; `hint` seeds catalog fields the stream can't reveal.
    public func publishVideo(
        format: VideoFormat,
        initData: Data = Data(),
        label: String? = nil,
        hint: VideoHint? = nil
    ) throws -> MediaProducer {
        MediaProducer(
            try ffi.publishVideo(init: MoqVideoInit(format: format, data: initData, label: label, hint: hint))
        )
    }

    /// Publish a container, which demuxes and publishes its own tracks. There is no label or hint:
    /// a container describes each track it publishes from its own metadata.
    public func publishContainer(
        format: ContainerFormat,
        initData: Data = Data()
    ) throws -> ContainerProducer {
        ContainerProducer(try ffi.publishContainer(init: MoqContainerInit(format: format, data: initData)))
    }

    /// Publish one audio codec onto a track requested through `BroadcastDynamic`.
    public func publishAudio(
        on request: TrackRequest,
        format: AudioFormat,
        initData: Data,
        label: String? = nil
    ) throws -> MediaProducer {
        MediaProducer(
            try ffi.publishAudioOnTrack(
                request: request.ffi,
                init: MoqAudioInit(format: format, data: initData, label: label)
            )
        )
    }

    /// Publish one video codec onto a track requested through `BroadcastDynamic`.
    public func publishVideo(
        on request: TrackRequest,
        format: VideoFormat,
        initData: Data = Data(),
        label: String? = nil,
        hint: VideoHint? = nil
    ) throws -> MediaProducer {
        MediaProducer(
            try ffi.publishVideoOnTrack(
                request: request.ffi,
                init: MoqVideoInit(format: format, data: initData, label: label, hint: hint)
            )
        )
    }

    /// Open a video track fed by a raw byte stream with inferred frame boundaries (e.g. piped
    /// Annex-B H.264). Only the self-delimiting formats work: `.avc3`, `.hev1`, `.av01`. Audio has
    /// no counterpart, having no frame boundaries to infer.
    public func publishVideoStream(
        format: VideoFormat,
        label: String? = nil,
        hint: VideoHint? = nil
    ) throws -> MediaStreamProducer {
        MediaStreamProducer(
            try ffi.publishVideoStream(init: MoqVideoInit(format: format, data: Data(), label: label, hint: hint))
        )
    }

    /// Open a container fed by a raw byte stream, which recovers its own framing.
    public func publishContainerStream(format: ContainerFormat) throws -> ContainerStreamProducer {
        ContainerStreamProducer(
            try ffi.publishContainerStream(format: format)
        )
    }

    /// Open a track for arbitrary byte payloads, with no codec or container.
    /// `info` sets track properties (priority, cache, timescale); omit for defaults.
    public func publishTrack(name: String, info: TrackInfo? = nil) throws -> TrackProducer {
        TrackProducer(try ffi.publishTrack(name: name, info: info))
    }

    /// Open a raw-audio track. PCM written via `AudioProducer.write` is encoded
    /// (e.g. to Opus) inside the FFI boundary per `input`/`output`.
    public func encodeAudio(name: String, input: AudioEncoderInput, output: AudioEncoderOutput) throws -> AudioProducer {
        AudioProducer(try ffi.encodeAudio(name: name, input: input, output: output))
    }

    /// Open a raw-video track. Pixels written via `VideoProducer.write` are
    /// encoded (H.264 or H.265) inside the FFI boundary per `input`/`output`.
    ///
    /// The track is named after the codec (`.avc3` / `.hev1`) and its catalog
    /// rendition is published immediately, read out of the encoder itself, so
    /// subscribers discover it through the catalog rather than a name you pick,
    /// and can find it before the first frame exists.
    public func encodeVideo(input: VideoEncoderInput, output: VideoEncoderOutput) throws -> VideoProducer {
        VideoProducer(try ffi.encodeVideo(input: input, output: output))
    }

    /// Open a JSON snapshot track (lossy latest-value), encoding each value from `Value`.
    ///
    /// Each `update` supersedes the last; a late joiner only sees the newest value. `deltaRatio`
    /// controls how aggressively merge-patch deltas replace full snapshots (`0` disables deltas).
    /// Set `compression` to DEFLATE each group; the consumer must pass the same flag. Advertise
    /// the track with `setCatalogSection` if consumers should discover it.
    public func publishJsonSnapshot<Value: Encodable>(
        name: String,
        of _: Value.Type,
        deltaRatio: UInt32 = MoqJsonSnapshotConfig().deltaRatio,
        compression: Bool = false
    ) throws -> JsonSnapshotProducer<Value> {
        JsonSnapshotProducer(try ffi.publishJsonSnapshot(name: name, config: MoqJsonSnapshotConfig(deltaRatio: deltaRatio, compression: compression)))
    }

    /// Open a JSON stream track (lossless append-log), encoding each record from `Value`.
    ///
    /// Every appended record is preserved and delivered in order. Set `compression` to DEFLATE
    /// the group; the consumer must pass the same flag.
    public func publishJsonStream<Value: Encodable>(
        name: String,
        of _: Value.Type,
        compression: Bool = false
    ) throws -> JsonStreamProducer<Value> {
        JsonStreamProducer(try ffi.publishJsonStream(name: name, config: MoqJsonStreamConfig(compression: compression)))
    }

    /// Set (or replace) an untyped application catalog section by name.
    ///
    /// `json` is any JSON document as a string; it rides alongside `video`/`audio` and reaches
    /// subscribers via `Catalog.sections`. `name` must not be a reserved media section
    /// (`video`/`audio`). The catalog is republished automatically.
    public func setCatalogSection(name: String, json: String) throws {
        try ffi.setCatalogSection(name: name, json: json)
    }

    /// Remove an untyped application catalog section by name. A no-op if it was absent.
    public func removeCatalogSection(name: String) throws {
        try ffi.removeCatalogSection(name: name)
    }

    /// Finish the broadcast, finalizing the catalog stream.
    public func finish() throws {
        try ffi.finish()
    }
}
