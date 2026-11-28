import Foundation
import MoqFFI

/// Read side of a raw track. Iterating yields groups in start order, skipping
/// forward if the reader falls behind: `for try await group in track { ... }`.
public final class TrackConsumer: AsyncSequence, Sendable {
    /// The group emitted by this sequence, in start order.
    public typealias Element = GroupConsumer

    let ffi: MoqTrackConsumer

    init(_ ffi: MoqTrackConsumer) {
        self.ffi = ffi
    }

    /// The next group in start order, skipping forward on fall-behind. `nil`
    /// once the track ends. Shares the ordered cursor with `readFrame`: a group
    /// one method has already taken is not returned by the other.
    public func nextGroup() async throws -> GroupConsumer? {
        (try await ffi.nextGroup()).map(GroupConsumer.init)
    }

    /// The next group in arrival order, which may be out of start order: groups
    /// already held come in the subscription's group order, newest first unless it
    /// asks ascending. `nil` once the track ends.
    public func recvGroup() async throws -> GroupConsumer? {
        (try await ffi.recvGroup()).map(GroupConsumer.init)
    }

    /// Read the first raw frame of the next group. Convenience for
    /// one-frame-per-group tracks (status/command style). Completed empty groups
    /// are skipped. `nil` only once the track ends. Cancelling one call keeps
    /// the current group so a later `readFrame` or `nextGroup` still sees it.
    public func readFrame() async throws -> Frame? {
        try await ffi.readFrame()
    }

    /// The publisher-side properties learned during subscription.
    public func info() -> TrackInfo {
        ffi.info()
    }

    /// Change this subscriber's delivery preferences. Throws on a range time the track
    /// can't serve.
    public func update(subscription: Subscription) throws {
        try checkSubscription(subscription)
        try ffi.update(subscription: subscription)
    }

    /// Ask the publisher for a group newer than the newest held, returning once the request
    /// is sent. Judged by the track's `TrackRefresh`: it is sent once the newest group is old
    /// enough, when whatever serves the track reads it (the session forwarding it upstream, or
    /// a local publisher waiting in `TrackDemand.refresh()`). Concurrent calls share one
    /// request and all return when it is sent. Cancelling the task before then withdraws its
    /// request. Returns at once when there is nothing to send, or when a newer group lands
    /// first. The new group arrives through the normal reads, such as `nextGroup`.
    public func refresh() async throws {
        try await ffi.refresh()
    }

    /// Cancel all current and future reads.
    public func cancel() {
        ffi.cancel()
    }

    /// Groups in arrival order, including out-of-order deliveries. Groups already held come in
    /// the subscription's order, newest first unless it asks ascending, as `recvGroup` returns
    /// them. The default `AsyncSequence` iteration uses start order instead.
    public var groupsAsArrived: AsyncThrowingStream<GroupConsumer, Swift.Error> {
        moqStream(cancel: { [ffi] in ffi.cancel() }) { [ffi] in
            (try await ffi.recvGroup()).map(GroupConsumer.init)
        }
    }

    /// Create an iterator that cancels native reads when iteration ends.
    public func makeAsyncIterator() -> AsyncThrowingStream<GroupConsumer, Swift.Error>.Iterator {
        moqStream(cancel: { [ffi] in ffi.cancel() }) { [ffi] in
            (try await ffi.nextGroup()).map(GroupConsumer.init)
        }.makeAsyncIterator()
    }
}

/// Read side of a single group. Iterating yields raw frames.
public final class GroupConsumer: AsyncSequence, Sendable {
    /// The raw frame emitted by this sequence.
    public typealias Element = Frame

    let ffi: MoqGroupConsumer

    init(_ ffi: MoqGroupConsumer) {
        self.ffi = ffi
    }

    /// The start of this group within the track.
    public var start: UInt64 {
        ffi.start()
    }

    /// The next raw frame, or `nil` once the group ends.
    public func readFrame() async throws -> Frame? {
        try await ffi.readFrame()
    }

    /// Cancel all current and future reads.
    public func cancel() {
        ffi.cancel()
    }

    /// Create an iterator that cancels native reads when iteration ends.
    public func makeAsyncIterator() -> AsyncThrowingStream<Frame, Swift.Error>.Iterator {
        moqStream(cancel: { [ffi] in ffi.cancel() }) { [ffi] in
            try await ffi.readFrame()
        }.makeAsyncIterator()
    }
}

/// Read side of a metadata subscription: a track's groups, each frame as its header alone.
/// Iterating yields groups in arrival order: `for try await group in meta { ... }`.
public final class MetaConsumer: AsyncSequence, Sendable {
    /// The group emitted by this sequence, in arrival order.
    public typealias Element = MetaGroup

    let ffi: MoqMetaConsumer

    init(_ ffi: MoqMetaConsumer) {
        self.ffi = ffi
    }

    /// The next group in arrival order, as `TrackConsumer.recvGroup` returns it. `nil` once
    /// the track ends.
    public func recvGroup() async throws -> MetaGroup? {
        (try await ffi.recvGroup()).map(MetaGroup.init)
    }

    /// The publisher-side properties learned during subscription.
    public func info() -> TrackInfo {
        ffi.info()
    }

    /// Change this subscriber's delivery preferences, as `TrackConsumer.update` does.
    public func update(subscription: Subscription) throws {
        try checkSubscription(subscription)
        try ffi.update(subscription: subscription)
    }

    /// Cancel all current and future reads.
    public func cancel() {
        ffi.cancel()
    }

    /// Create an iterator that cancels native reads when iteration ends.
    public func makeAsyncIterator() -> AsyncThrowingStream<MetaGroup, Swift.Error>.Iterator {
        moqStream(cancel: { [ffi] in ffi.cancel() }) { [ffi] in
            (try await ffi.recvGroup()).map(MetaGroup.init)
        }.makeAsyncIterator()
    }
}

/// One group of a metadata subscription. Iterating yields its frames' headers.
public final class MetaGroup: AsyncSequence, Sendable {
    /// The frame header emitted by this sequence.
    public typealias Element = FrameHeader

    let ffi: MoqMetaGroup

    init(_ ffi: MoqMetaGroup) {
        self.ffi = ffi
    }

    /// The start of this group within the track.
    public var start: UInt64 {
        ffi.start()
    }

    /// The next frame's header, without its payload, or `nil` once the group ends.
    public func nextHeader() async throws -> FrameHeader? {
        try await ffi.nextHeader()
    }

    /// Cancel all current and future reads.
    public func cancel() {
        ffi.cancel()
    }

    /// Create an iterator that cancels native reads when iteration ends.
    public func makeAsyncIterator() -> AsyncThrowingStream<FrameHeader, Swift.Error>.Iterator {
        moqStream(cancel: { [ffi] in ffi.cancel() }) { [ffi] in
            try await ffi.nextHeader()
        }.makeAsyncIterator()
    }
}

/// A watch-only handle to whether a published track has subscribers, and when they ask for a
/// new group.
///
/// Returned by a producer's `demand()`. Weak: holding it neither keeps the track open nor locks
/// the producer, so a wait can park here while the producer keeps publishing. Waits throw
/// `MoqError.Closed` once the track is released.
public final class TrackDemand: Sendable {
    let ffi: MoqTrackDemand

    init(_ ffi: MoqTrackDemand) {
        self.ffi = ffi
    }

    /// The name of the track this watches.
    public var name: String {
        ffi.name()
    }

    /// Whether anyone consumes the track: a subscription, fetch, or a group one of them is
    /// still reading.
    public var wanted: Bool {
        ffi.wanted()
    }

    /// Suspend until `wanted` changes and return it. The first call returns at once.
    /// End a track nobody wants by dropping every reference to its producer once this returns
    /// false: a reader that joined since is served by the next request for the track while a
    /// request handler is live.
    public func wantedChanged() async throws -> Bool {
        try await ffi.wantedChanged()
    }

    /// Suspend until a subscriber asks for a new group: the first group of a track with none,
    /// or a join judged by the track's `TrackRefresh` once its interval passed. Returns at once
    /// while the request is pending, and the next group written answers it, so loop: wait
    /// here, then write a group that starts with a keyframe or a snapshot.
    public func refresh() async throws {
        try await ffi.refresh()
    }
}

/// Write side of a raw track.
public final class TrackProducer: Sendable {
    let ffi: MoqTrackProducer

    init(_ ffi: MoqTrackProducer) {
        self.ffi = ffi
    }

    /// A read handle for this track (local pub/sub, no origin needed).
    /// `subscription` tunes delivery priority, group range, and staleness; omit for defaults.
    public func consume(subscription: Subscription? = nil) throws -> TrackConsumer {
        try checkSubscription(subscription)
        return TrackConsumer(try ffi.consume(subscription: subscription))
    }

    /// A watch-only handle to the track's name and whether it has subscribers.
    public func demand() throws -> TrackDemand {
        TrackDemand(try ffi.demand())
    }

    /// Serve fetches for groups that are not currently cached.
    public func requests() throws -> TrackRequests {
        TrackRequests(try ffi.requests())
    }

    /// Append the next group with its first frame, returning a producer for the rest. The group
    /// starts at the frame's `timestamp`, converted to the track's timescale; on a scale-less
    /// track it is `Timestamp.unscaled(key)` at the group's key. A start at or before a group
    /// the track holds throws.
    public func appendGroup(_ payload: Data, timestamp: Timestamp) throws -> GroupProducer {
        GroupProducer(try ffi.appendGroup(frame: Frame(payload: payload, timestamp: timestamp)))
    }

    /// Create a group at an explicit start, its first frame's `timestamp`, for sparse or
    /// replayed groups.
    public func createGroup(_ payload: Data, timestamp: Timestamp) throws -> GroupProducer {
        GroupProducer(try ffi.createGroup(frame: Frame(payload: payload, timestamp: timestamp)))
    }

    /// Declare that groups `start` up to the exclusive `end` will never be produced.
    /// Readers move past them, and creating a group there then throws.
    public func skipGroups(start: UInt64, end: UInt64) throws {
        try ffi.skipGroups(start: start, end: end)
    }

    /// Write a single-frame group with a presentation timestamp. A scale-less track has no time,
    /// so there the timestamp is `Timestamp.unscaled(key)`, naming the new group's key.
    public func writeFrame(_ payload: Data, timestamp: Timestamp) throws {
        try ffi.writeFrame(frame: Frame(payload: payload, timestamp: timestamp))
    }

    /// The newest group written, at its newest frame, or `nil` before any.
    public func largest() throws -> Position? {
        try ffi.largest()
    }

    /// Abort the track with an application error code, failing active consumers.
    public func abort(errorCode: UInt16) throws {
        try ffi.abort(errorCode: errorCode)
    }

    /// Finish the track. No more groups can be appended.
    public func finish() throws {
        try ffi.finish()
    }

    /// Declare the exclusive final group start ahead of the live edge.
    /// The producer remains open for groups below the boundary.
    public func finish(at finalStart: UInt64) throws {
        try ffi.finishAt(finalStart: finalStart)
    }
}

/// Write side of a single group.
public final class GroupProducer: Sendable {
    let ffi: MoqGroupProducer

    init(_ ffi: MoqGroupProducer) {
        self.ffi = ffi
    }

    /// The start of this group within the track.
    public var start: UInt64 {
        ffi.start()
    }

    /// A read handle for this group's frames.
    public func consume() throws -> GroupConsumer {
        GroupConsumer(try ffi.consume())
    }

    /// Wait until a frame of `size` bytes fits a window ahead of the fastest reader.
    ///
    /// Await it before each `writeFrame` so that reader paces the writer, as TCP
    /// backpressure does, instead of the group shedding frames it has not taken, which
    /// fails it with `Evicted`; slower readers are not waited on. It also waits until a reader holds the
    /// group, and returns once the group is closed so the next write throws why. Throws
    /// `Closed` after `abort`.
    public func ready(size: UInt64) async throws {
        try await ffi.ready(size: size)
    }

    /// Write a frame with a presentation timestamp, or `Timestamp.unscaled(start)` on a
    /// scale-less track. Never blocks: without `ready`, a writer that outpaces its readers sheds
    /// the group's front instead.
    public func writeFrame(_ payload: Data, timestamp: Timestamp) throws {
        try ffi.writeFrame(frame: Frame(payload: payload, timestamp: timestamp))
    }

    /// Mark the group complete. No more frames can be written.
    public func finish() throws {
        try ffi.finish()
    }

    /// Mark the group complete with its exclusive `end` in the track's timescale.
    /// No other group may start before `end`.
    public func finish(at end: UInt64) throws {
        try ffi.finishAt(end: end)
    }

    /// Abort the group with an application error code.
    public func abort(errorCode: UInt16) throws {
        try ffi.abort(errorCode: errorCode)
    }
}
