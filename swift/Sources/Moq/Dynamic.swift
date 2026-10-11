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
    /// priority, and cache; omit for defaults.
    public func accept(info: TrackInfo? = nil) throws -> TrackProducer {
        TrackProducer(try ffi.accept(info: info))
    }

    /// Create a fetch handler before accepting this requested track.
    public func dynamic() throws -> TrackDynamic {
        TrackDynamic(try ffi.dynamic())
    }

    /// Reject the request with an application error code, failing the subscriber.
    public func abort(errorCode: UInt16) throws {
        try ffi.abort(errorCode: errorCode)
    }
}

/// A watch-only handle to the callers waiting on a requested group.
///
/// Returned by `GroupRequest.demand()`. Weak: holding it does not keep the request alive. The last
/// caller to leave withdraws the request, so once unused, demand never returns: drop the request.
/// Waits throw once the request is answered: `MoqError.Closed` if it was dropped, otherwise the
/// error the accept or reject left for the waiting fetches.
public final class GroupDemand: Sendable {
    let ffi: MoqGroupDemand

    init(_ ffi: MoqGroupDemand) {
        self.ffi = ffi
    }

    /// The sequence of the group this watches.
    public var sequence: UInt64 {
        ffi.sequence()
    }

    /// Whether the group has at least one waiting caller right now.
    public var isUsed: Bool {
        ffi.isUsed()
    }

    /// Suspend until the group has at least one waiting caller.
    public func used() async throws {
        try await ffi.used()
    }

    /// Suspend until the group has no waiting callers.
    public func unused() async throws {
        try await ffi.unused()
    }
}

/// A request to produce one uncached group for a fetch consumer.
public final class GroupRequest: Sendable {
    let ffi: MoqGroupRequest

    init(_ ffi: MoqGroupRequest) {
        self.ffi = ffi
    }

    /// The requested group sequence within the track.
    public var sequence: UInt64 {
        ffi.sequence()
    }

    /// The consumer's delivery priority for this fetch.
    public var priority: UInt8 {
        ffi.priority()
    }

    /// A watch-only handle to whether any caller still wants this group.
    public func demand() throws -> GroupDemand {
        GroupDemand(try ffi.demand())
    }

    /// Accept the request and return a producer for the group.
    public func accept() throws -> GroupProducer {
        GroupProducer(try ffi.accept())
    }

    /// Reject the fetch with an application error code.
    public func abort(errorCode: UInt16) throws {
        try ffi.abort(errorCode: errorCode)
    }
}

/// A stream of uncached group requests for one track.
public final class TrackDynamic: AsyncSequence, Sendable {
    /// The group request emitted by this sequence.
    public typealias Element = GroupRequest

    let ffi: MoqTrackDynamic

    init(_ ffi: MoqTrackDynamic) {
        self.ffi = ffi
    }

    /// Wait for the next uncached group request.
    public func requestedGroup() async throws -> GroupRequest {
        GroupRequest(try await ffi.requestedGroup())
    }

    /// Cancel all current and future group request waits.
    public func cancel() {
        ffi.cancel()
    }

    /// Create an iterator that cancels native waits when iteration ends.
    public func makeAsyncIterator() -> AsyncThrowingStream<GroupRequest, Swift.Error>.Iterator {
        moqStream(cancel: { [ffi] in ffi.cancel() }) { [ffi] in
            GroupRequest(try await ffi.requestedGroup())
        }.makeAsyncIterator()
    }
}

/// A stream of track requests from subscribers for tracks that are not published
/// yet. Iterate directly: `for try await request in dynamic { ... }`. Hold this
/// while such requests should be served; the sequence ends (throwing `Closed`)
/// when the broadcast closes, and cancelling the consuming task stops serving.
public final class BroadcastDynamic: AsyncSequence, Sendable {
    /// The track request emitted by this sequence.
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

    /// Create an iterator that cancels native waits when iteration ends.
    public func makeAsyncIterator() -> AsyncThrowingStream<TrackRequest, Swift.Error>.Iterator {
        moqStream(cancel: { [ffi] in ffi.cancel() }) { [ffi] in
            TrackRequest(try await ffi.requestedTrack())
        }.makeAsyncIterator()
    }
}
