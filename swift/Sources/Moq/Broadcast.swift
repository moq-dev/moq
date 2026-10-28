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

    /// Resolve a catalog rendition to the broadcast serving its track.
    ///
    /// The rendition's `broadcast` reference, `nil` or empty, names this broadcast; anything else
    /// names a sibling relative to it (e.g. `./source`). Its `epoch` is the publisher instance the
    /// reference names: a broadcast proven to serve another instance throws instead of resolving,
    /// and a route without an epoch cannot prove one, so there it is best effort. Call it on a
    /// rendition that carries a reference before `subscribeTrack` or `fetchGroup`, which take a
    /// track name rather than a rendition: the rendition's `track`, or its ID when `nil`. The
    /// rendition-based `Media.TrackConsumer.subscribe`, `Media.GroupConsumer.fetch`,
    /// `Audio.Consumer.subscribe`, and `Video.Consumer.subscribe` resolve it themselves.
    ///
    /// Throws if this broadcast came from a local producer rather than an origin, since a
    /// standalone broadcast has no sibling to name.
    public func resolve(_ rendition: Media.Rendition) async throws -> BroadcastConsumer {
        BroadcastConsumer(try await ffi.resolve(rendition: rendition))
    }

}

/// Write side of a broadcast: open tracks and publish frames. Its `Publication`,
/// if any, announces it.
///
/// Constructing one directly creates a standalone broadcast for local pub/sub.
/// To publish at a path, use `OriginProducer.createBroadcast(path:)` instead.
public final class BroadcastProducer: Sendable {
    let ffi: MoqBroadcastProducer

    /// Create a standalone broadcast for local pub/sub. To publish at a path,
    /// use `OriginProducer.createBroadcast(path:)`.
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
    /// the returned `BroadcastRequests` while such requests should be served.
    public func requests() throws -> BroadcastRequests {
        BroadcastRequests(try ffi.requests())
    }

    /// Open a track for arbitrary byte payloads, with no codec or container.
    /// `info` sets track properties (priority, cache, timescale); omit for defaults.
    public func publishTrack(name: String, info: TrackInfo? = nil) throws -> TrackProducer {
        TrackProducer(try ffi.publishTrack(name: name, info: info))
    }

    /// End the broadcast for good: retract it and serve no new tracks.
    ///
    /// Tracks still open on it end with `Unroutable`, which readers take as the publisher
    /// leaving, and their later calls throw `MoqError.Closed`; finish a track first to end it
    /// cleanly. Closing again is a no-op.
    public func close() throws {
        try ffi.close()
    }
}
