import Foundation
import MoqFFI

/// Read side of a broadcast: subscribe to its catalog and tracks.
public final class BroadcastConsumer: Sendable {
    let ffi: MoqBroadcastConsumer

    init(_ ffi: MoqBroadcastConsumer) {
        self.ffi = ffi
    }

    /// Subscribe to the broadcast's catalog (the description of its tracks).
    public func subscribeCatalog() async throws -> CatalogConsumer {
        CatalogConsumer(try await ffi.subscribeCatalog())
    }

    /// Subscribe to a track by name, delivering raw frame payloads with no codec
    /// or container parsing. `subscription` tunes delivery (priority, ordering,
    /// group range); omit for defaults.
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

    /// Subscribe to a media track, delivering frames in decode order. `container`
    /// comes from the catalog; `maxLatencyMs` bounds buffering before skipping a GoP.
    /// `subscription` tunes delivery (priority, ordering, group range); omit for defaults.
    public func subscribeMedia(
        name: String,
        container: Container,
        maxLatencyMs: UInt64,
        subscription: Subscription? = nil
    ) async throws -> MediaConsumer {
        MediaConsumer(
            try await ffi.subscribeMedia(
                name: name, container: container, maxLatencyMs: maxLatencyMs, subscription: subscription))
    }

    /// Subscribe to a raw-audio track, decoding to PCM in the layout `output`
    /// declares. `catalogAudio` is the matching rendition from the catalog.
    public func subscribeAudio(name: String, catalogAudio: Audio, output: AudioDecoderOutput) async throws -> AudioConsumer {
        AudioConsumer(try await ffi.subscribeAudio(name: name, catalogAudio: catalogAudio, output: output))
    }
}

/// Write side of a broadcast: open tracks and publish frames. Does nothing until
/// announced to an origin (see `OriginProducer.announce`).
public final class BroadcastProducer: Sendable {
    let ffi: MoqBroadcastProducer

    public init() throws {
        ffi = try MoqBroadcastProducer()
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

    /// Open a media track. `format` controls how `initData` and frame payloads
    /// are interpreted (e.g. `"opus"`, `"avc3"`).
    public func publishMedia(format: String, initData: Data) throws -> MediaProducer {
        MediaProducer(try ffi.publishMedia(init: MoqInit(format: format, data: initData, video: nil)))
    }

    /// Open a media track fed by a raw byte stream with inferred frame boundaries
    /// (e.g. piped Annex-B H.264). Only self-describing formats are supported.
    public func publishMediaStream(format: String) throws -> MediaStreamProducer {
        MediaStreamProducer(try ffi.publishMediaStream(init: MoqInit(format: format, data: Data(), video: nil)))
    }

    /// Open a track for arbitrary byte payloads, with no codec or container.
    /// `info` sets track properties (priority, cache, timescale); omit for defaults.
    public func publishTrack(name: String, info: TrackInfo? = nil) throws -> TrackProducer {
        TrackProducer(try ffi.publishTrack(name: name, info: info))
    }

    /// Open a raw-audio track. PCM written via `AudioProducer.write` is encoded
    /// (e.g. to Opus) inside the FFI boundary per `input`/`output`.
    public func publishAudio(name: String, input: AudioEncoderInput, output: AudioEncoderOutput) throws -> AudioProducer {
        AudioProducer(try ffi.publishAudio(name: name, input: input, output: output))
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
