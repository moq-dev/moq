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

    /// Watch this track's subscribers, including after accepting it.
    public func demand() throws -> TrackDemand {
        TrackDemand(try ffi.demand())
    }

    /// Create a fetch handler before accepting this requested track.
    public func requests() throws -> TrackRequests {
        TrackRequests(try ffi.requests())
    }

    /// Reject the request with an application error code, failing the subscriber.
    public func abort(errorCode: UInt16) throws {
        try ffi.abort(errorCode: errorCode)
    }
}

/// A run of groups readers miss, for a `TrackRequests` to serve.
///
/// Accept each group you have and drop the rest; `wanted` names what is still owed, in the
/// fill's order. Releasing the fill names whatever is left as dropped while a reader still
/// wants it; once nobody does, it is withdrawn, so stop serving it once `wanted` is nil.
/// Stop serving a single group when `unwanted(sequence:)` returns for it, and move on to `wanted`.
public final class Fill: Sendable {
    let ffi: MoqFill

    init(_ ffi: MoqFill) {
        self.ffi = ffi
    }

    /// The first group to fill.
    public var startGroup: UInt64 {
        ffi.startGroup()
    }

    /// The frame within the first group to start from.
    public var startFrame: UInt64 {
        ffi.startFrame()
    }

    /// The exclusive end group.
    public var endGroup: UInt64 {
        ffi.endGroup()
    }

    /// The most urgent delivery priority among the readers waiting on it.
    public var priority: Int8 {
        ffi.priority()
    }

    /// The most tolerant age budget among its readers, or nil for the publisher's max age
    /// alone.
    public var maxDelay: TimeInterval? {
        ffi.maxDelay()
    }

    /// Which end to fill first.
    public var order: Subscription.Order {
        ffi.order()
    }

    /// The next group still owed in the fill's order, or nil once every group is answered or
    /// no reader waits on the fill.
    public var wanted: UInt64? {
        ffi.wanted()
    }

    /// Suspend until `wanted` changes and return it. The first call returns at once.
    public func wantedChanged() async throws -> UInt64? {
        try await ffi.wantedChanged()
    }

    /// Insert group `sequence` and return a producer for it. The first group starts at
    /// `startFrame`, so its frames keep their indices.
    public func accept(sequence: UInt64) throws -> GroupProducer {
        GroupProducer(try ffi.accept(sequence: sequence))
    }

    /// Name groups `start` up to the exclusive `end` as never to be delivered.
    public func dropGroups(start: UInt64, end: UInt64) {
        ffi.dropGroups(start: start, end: end)
    }

    /// Whether a reader still waits for group `sequence` from this fill: false once every
    /// reader took it or narrowed past it, or nobody waits on the fill.
    public func isWanted(sequence: UInt64) -> Bool {
        ffi.isWanted(sequence: sequence)
    }

    /// Suspend until no reader waits for group `sequence` from this fill, so stop serving it.
    public func unwanted(sequence: UInt64) async throws {
        try await ffi.unwanted(sequence: sequence)
    }
}

/// A stream of the fills readers of one track need: runs of groups the cache lacks.
public final class TrackRequests: AsyncSequence, Sendable {
    /// The fill emitted by this sequence.
    public typealias Element = Fill

    let ffi: MoqTrackRequests

    init(_ ffi: MoqTrackRequests) {
        self.ffi = ffi
    }

    /// Wait for the next fill.
    public func requestedFill() async throws -> Fill {
        Fill(try await ffi.requestedFill())
    }

    /// Cancel all current and future fill waits.
    public func cancel() {
        ffi.cancel()
    }

    /// Create an iterator that cancels native waits when iteration ends.
    public func makeAsyncIterator() -> AsyncThrowingStream<Fill, Swift.Error>.Iterator {
        moqStream(cancel: { [ffi] in ffi.cancel() }) { [ffi] in
            Fill(try await ffi.requestedFill())
        }.makeAsyncIterator()
    }
}

/// A stream of track requests from subscribers for tracks that are not published
/// yet. Iterate directly: `for try await request in requests { ... }`. Hold this
/// while such requests should be served; the sequence ends (throwing `Closed`)
/// when the broadcast closes, and cancelling the consuming task stops serving.
public final class BroadcastRequests: AsyncSequence, Sendable {
    /// The track request emitted by this sequence.
    public typealias Element = TrackRequest

    let ffi: MoqBroadcastRequests

    init(_ ffi: MoqBroadcastRequests) {
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
