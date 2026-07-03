import Foundation
import MoqFFI

/// A subscriber-requested track that has not been accepted yet. Accept it to get
/// a `TrackProducer` for raw writes, or abort it to reject the subscriber.
public final class TrackRequest: Sendable {
    let ffi: MoqTrackRequest

    init(_ ffi: MoqTrackRequest) {
        self.ffi = ffi
    }

    /// The requested track name.
    public var name: String {
        get throws { try ffi.name() }
    }

    /// Accept the request as a raw track. `info` fixes the track's timescale,
    /// priority, ordering, and cache; omit for defaults.
    public func accept(info: TrackInfo? = nil) throws -> TrackProducer {
        TrackProducer(try ffi.accept(info: info))
    }

    /// Reject the request with an application error code, failing the subscriber.
    public func abort(errorCode: Int32) throws {
        try ffi.abort(errorCode: errorCode)
    }
}

/// A stream of track requests from subscribers for tracks that are not published
/// yet. Iterate directly: `for try await request in dynamic { ... }`. Hold this
/// while such requests should be served; the sequence ends (throwing `Closed`)
/// when the broadcast closes, and cancelling the consuming task stops serving.
public final class BroadcastDynamic: AsyncSequence, Sendable {
    public typealias Element = TrackRequest

    let ffi: MoqBroadcastDynamic

    init(_ ffi: MoqBroadcastDynamic) {
        self.ffi = ffi
    }

    /// The next requested track. Throws `Closed` once the broadcast closes.
    public func requestedTrack() async throws -> TrackRequest {
        TrackRequest(try await ffi.requestedTrack())
    }

    /// Cancel all current and future `requestedTrack()` calls.
    public func cancel() {
        ffi.cancel()
    }

    public func makeAsyncIterator() -> AsyncThrowingStream<TrackRequest, Swift.Error>.Iterator {
        moqStream(cancel: { [ffi] in ffi.cancel() }) { [ffi] in
            TrackRequest(try await ffi.requestedTrack())
        }.makeAsyncIterator()
    }
}
