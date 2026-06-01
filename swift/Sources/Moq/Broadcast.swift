import Foundation
import MoqFFI

/// Read side of a broadcast: subscribe to its catalog and tracks.
public final class BroadcastConsumer: Sendable {
    let ffi: MoqBroadcastConsumer

    init(_ ffi: MoqBroadcastConsumer) {
        self.ffi = ffi
    }

    /// Subscribe to the broadcast's catalog (the description of its tracks).
    public func subscribeCatalog() throws -> CatalogConsumer {
        CatalogConsumer(try ffi.subscribeCatalog())
    }

    /// Subscribe to a track by name, delivering raw frame payloads with no codec
    /// or container parsing.
    public func subscribeTrack(name: String) throws -> TrackConsumer {
        TrackConsumer(try ffi.subscribeTrack(name: name))
    }

    /// Subscribe to a media track, delivering frames in decode order. `container`
    /// comes from the catalog; `maxLatencyMs` bounds buffering before skipping a GoP.
    public func subscribeMedia(name: String, container: Container, maxLatencyMs: UInt64) throws -> MediaConsumer {
        MediaConsumer(try ffi.subscribeMedia(name: name, container: container, maxLatencyMs: maxLatencyMs))
    }

    /// Subscribe to a raw-audio track, decoding to PCM in the layout `output`
    /// declares. `catalogAudio` is the matching rendition from the catalog.
    public func subscribeAudio(name: String, catalogAudio: Audio, output: AudioDecoderOutput) throws -> AudioConsumer {
        AudioConsumer(try ffi.subscribeAudio(name: name, catalogAudio: catalogAudio, output: output))
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

    /// Open a media track. `format` controls how `initData` and frame payloads
    /// are interpreted (e.g. `"opus"`, `"avc3"`).
    public func publishMedia(format: String, initData: Data) throws -> MediaProducer {
        MediaProducer(try ffi.publishMedia(format: format, init: initData))
    }

    /// Open a media track fed by a raw byte stream with inferred frame boundaries
    /// (e.g. piped Annex-B H.264). Only self-describing formats are supported.
    public func publishMediaStream(format: String) throws -> MediaStreamProducer {
        MediaStreamProducer(try ffi.publishMediaStream(format: format))
    }

    /// Open a track for arbitrary byte payloads, with no codec or container.
    public func publishTrack(name: String) throws -> TrackProducer {
        TrackProducer(try ffi.publishTrack(name: name))
    }

    /// Open a raw-audio track. PCM written via `AudioProducer.write` is encoded
    /// (e.g. to Opus) inside the FFI boundary per `input`/`output`.
    public func publishAudio(name: String, input: AudioEncoderInput, output: AudioEncoderOutput) throws -> AudioProducer {
        AudioProducer(try ffi.publishAudio(name: name, input: input, output: output))
    }

    /// Finish the broadcast, finalizing the catalog stream.
    public func finish() throws {
        try ffi.finish()
    }
}
