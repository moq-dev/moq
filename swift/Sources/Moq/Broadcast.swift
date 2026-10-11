import Foundation
import MoqFFI

/// Read side of a broadcast: subscribe to its catalog and tracks.
public final class BroadcastConsumer: Sendable {
    let ffi: MoqBroadcastConsumer

    init(_ ffi: MoqBroadcastConsumer) {
        self.ffi = ffi
    }

    /// Subscribe to a track by name, delivering raw frame payloads with no codec
    /// or container parsing. `subscription` tunes delivery priority, group range, and
    /// staleness; omit for defaults.
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

    /// Resolve a catalog rendition's `broadcast` reference to the broadcast serving its track.
    ///
    /// `reference` is `Media.Video.broadcast` / `Media.Audio.broadcast`: `nil` or empty names this
    /// broadcast, anything else names a sibling relative to it (e.g. `./source`). Call it on a
    /// rendition that carries one before `Media.ContainerConsumer.subscribe`, `subscribeTrack`, `fetchGroup`, or
    /// `Media.ContainerGroupConsumer.fetch`, which take a track name rather than a rendition; `decodeAudio` and
    /// `decodeVideo` resolve it themselves.
    ///
    /// Throws if this broadcast came from a local producer rather than an origin, since a
    /// standalone broadcast has no sibling to name.
    public func resolve(_ reference: String?) async throws -> BroadcastConsumer {
        BroadcastConsumer(try await ffi.resolve(reference: reference))
    }

    /// Subscribe to a raw-audio track, decoding to PCM in the layout `output`
    /// declares. `catalogAudio` is the matching rendition from the catalog.
    public func decodeAudio(name: String, catalogAudio: Media.Audio, output: AudioDecoderOutput) async throws -> AudioConsumer {
        AudioConsumer(try await ffi.decodeAudio(name: name, catalogAudio: catalogAudio, output: output))
    }

    /// Subscribe to a video track and decode it inside the bindings.
    /// `catalogVideo` is the matching rendition from the catalog.
    ///
    /// Each frame converts to a packed CPU layout on demand via
    /// `pixels(format:)`. `output.resize` is best effort, so read each frame's
    /// own dimensions.
    public func decodeVideo(
        name: String,
        catalogVideo: Media.Video,
        output: VideoDecoderOutput = VideoDecoderOutput()
    ) async throws -> VideoConsumer {
        VideoConsumer(try await ffi.decodeVideo(name: name, catalogVideo: catalogVideo, output: output))
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

    /// Advertise this broadcast's exact path as a route.
    ///
    /// Announcing again re-prices the route in place. Until announced, the
    /// broadcast is invisible and unroutable for local consumers and peers alike.
    public func announce(route: Route = Route()) throws {
        try ffi.announce(route: route)
    }

    /// Retract this broadcast's advertisement, if any, from local consumers and peers alike.
    public func unannounce() throws {
        try ffi.unannounce()
    }

    /// Open a track for arbitrary byte payloads, with no codec or container.
    /// `info` sets track properties (priority, cache, timescale); omit for defaults.
    public func publishTrack(name: String, info: TrackInfo? = nil) throws -> TrackProducer {
        TrackProducer(try ffi.publishTrack(name: name, info: info))
    }

    /// Open a raw-audio track. PCM written via `AudioProducer.write` is encoded
    /// inside the FFI boundary per `input`/`output`. Select the codec with
    /// `AudioCodec.opus()` or `AudioCodec.aac()`, placed in `output`.
    ///
    /// Pass `bandwidth` to reserve this track's bitrate against the session's
    /// allocator so a co-resident video encoder sizes itself against what is left.
    public func encodeAudio(
        name: String,
        input: AudioEncoderInput,
        output: AudioEncoderOutput,
        bandwidth: Bandwidth? = nil
    ) throws -> AudioProducer {
        AudioProducer(try ffi.encodeAudio(name: name, input: input, output: output, bandwidth: bandwidth?.ffi))
    }

    /// Open a raw-video track. Pixels written via `VideoProducer.write` are
    /// encoded (H.264 or H.265) inside the FFI boundary per `input`/`output`.
    ///
    /// Set `output.track` to choose the track name; otherwise one is derived from
    /// the codec (`.avc3` / `.hev1`). The catalog rendition is published
    /// immediately so subscribers can discover it before the first frame exists.
    ///
    /// Pass `bandwidth` to reserve this track's configured bitrate and follow
    /// the grant.
    public func encodeVideo(
        input: VideoEncoderInput,
        output: VideoEncoderOutput,
        bandwidth: Bandwidth? = nil
    ) throws -> VideoProducer {
        VideoProducer(try ffi.encodeVideo(input: input, output: output, bandwidth: bandwidth?.ffi))
    }

    /// End the broadcast for good: retract it and serve no new tracks.
    ///
    /// Tracks already subscribed carry on to their own end. Closing again is a no-op.
    public func close() throws {
        try ffi.close()
    }
}
