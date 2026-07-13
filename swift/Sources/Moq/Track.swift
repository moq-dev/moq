import Foundation
import MoqFFI

/// Read side of a raw track. Iterating yields groups in sequence order, skipping
/// forward if the reader falls behind: `for try await group in track { ... }`.
public final class TrackConsumer: AsyncSequence, Sendable {
    public typealias Element = GroupConsumer

    let ffi: MoqTrackConsumer

    init(_ ffi: MoqTrackConsumer) {
        self.ffi = ffi
    }

    /// The next group in sequence order, skipping forward on fall-behind. `nil`
    /// once the track ends.
    public func nextGroup() async throws -> GroupConsumer? {
        (try await ffi.nextGroup()).map(GroupConsumer.init)
    }

    /// The next group in arrival order, which may be out of sequence. `nil` once
    /// the track ends.
    public func recvGroup() async throws -> GroupConsumer? {
        (try await ffi.recvGroup()).map(GroupConsumer.init)
    }

    /// Read the first timestamped frame of the next group. Convenience for
    /// one-frame-per-group tracks (status/command style). `nil` once the track ends.
    public func readFrame() async throws -> Frame? {
        try await ffi.readFrame()
    }

    /// The next best-effort datagram in arrival order. `nil` once the track ends.
    public func recvDatagram() async throws -> Datagram? {
        try await ffi.recvDatagram()
    }

    /// The publisher-side properties learned during subscription.
    public func info() async throws -> TrackInfo {
        try await ffi.info()
    }

    /// Change this subscriber's delivery preferences.
    public func update(subscription: Subscription) {
        ffi.update(subscription: subscription)
    }

    /// Cancel all current and future reads.
    public func cancel() {
        ffi.cancel()
    }

    /// Datagrams in arrival order. Datagram delivery requires a datagram-capable
    /// transport and wire version; there is no stream fallback.
    public var datagrams: AsyncThrowingStream<Datagram, Swift.Error> {
        moqStream(cancel: { [ffi] in ffi.cancel() }) { [ffi] in
            try await ffi.recvDatagram()
        }
    }

    /// Groups in arrival order, including out-of-sequence deliveries. The default
    /// `AsyncSequence` iteration uses sequence order instead.
    public var groupsAsArrived: AsyncThrowingStream<GroupConsumer, Swift.Error> {
        moqStream(cancel: { [ffi] in ffi.cancel() }) { [ffi] in
            (try await ffi.recvGroup()).map(GroupConsumer.init)
        }
    }

    public func makeAsyncIterator() -> AsyncThrowingStream<GroupConsumer, Swift.Error>.Iterator {
        moqStream(cancel: { [ffi] in ffi.cancel() }) { [ffi] in
            (try await ffi.nextGroup()).map(GroupConsumer.init)
        }.makeAsyncIterator()
    }
}

/// Read side of a single group. Iterating yields timestamped raw frames.
public final class GroupConsumer: AsyncSequence, Sendable {
    public typealias Element = Frame

    let ffi: MoqGroupConsumer

    init(_ ffi: MoqGroupConsumer) {
        self.ffi = ffi
    }

    /// The sequence number of this group within the track.
    public var sequence: UInt64 {
        ffi.sequence()
    }

    /// The next timestamped frame, or `nil` once the group ends.
    public func readFrame() async throws -> Frame? {
        try await ffi.readFrame()
    }

    /// Cancel all current and future reads.
    public func cancel() {
        ffi.cancel()
    }

    public func makeAsyncIterator() -> AsyncThrowingStream<Frame, Swift.Error>.Iterator {
        moqStream(cancel: { [ffi] in ffi.cancel() }) { [ffi] in
            try await ffi.readFrame()
        }.makeAsyncIterator()
    }
}

/// Write side of a raw track.
public final class TrackProducer: Sendable {
    let ffi: MoqTrackProducer

    init(_ ffi: MoqTrackProducer) {
        self.ffi = ffi
    }

    /// The track's name.
    public var name: String {
        get throws { try ffi.name() }
    }

    /// A read handle for this track (local pub/sub, no origin needed).
    /// `subscription` tunes delivery priority, group ordering priority, and group range; omit for defaults.
    public func consume(subscription: Subscription? = nil) throws -> TrackConsumer {
        TrackConsumer(try ffi.consume(subscription: subscription))
    }

    /// Suspend until the track has at least one active consumer.
    public func used() async throws {
        try await ffi.used()
    }

    /// Suspend until the track has no active consumers.
    public func unused() async throws {
        try await ffi.unused()
    }

    /// Serve fetches for groups that are not currently cached.
    public func dynamic() throws -> TrackDynamic {
        TrackDynamic(try ffi.dynamic())
    }

    /// Append a new group, returning a producer for its frames.
    public func appendGroup() throws -> GroupProducer {
        GroupProducer(try ffi.appendGroup())
    }

    /// Write a single-frame group with a timestamp in microseconds.
    public func writeFrame(_ payload: Data, timestampUs: UInt64) throws {
        try ffi.writeFrame(payload: payload, timestampUs: timestampUs)
    }

    /// Send a best-effort datagram and return its sequence number.
    ///
    /// `timestampUs` is the presentation timestamp in microseconds. Payloads are
    /// capped at 1200 bytes. There is no stream fallback.
    public func appendDatagram(timestampUs: UInt64, payload: Data) throws -> UInt64 {
        try ffi.appendDatagram(timestampUs: timestampUs, payload: payload)
    }

    /// Abort the track with an application error code, failing active consumers.
    public func abort(errorCode: Int32) throws {
        try ffi.abort(errorCode: errorCode)
    }

    /// Finish the track. No more groups can be appended.
    public func finish() throws {
        try ffi.finish()
    }
}

/// Write side of a single group.
public final class GroupProducer: Sendable {
    let ffi: MoqGroupProducer

    init(_ ffi: MoqGroupProducer) {
        self.ffi = ffi
    }

    /// The sequence number of this group within the track.
    public var sequence: UInt64 {
        ffi.sequence()
    }

    /// A read handle for this group's frames.
    public func consume() throws -> GroupConsumer {
        GroupConsumer(try ffi.consume())
    }

    /// Write a frame with a timestamp in microseconds.
    public func writeFrame(_ payload: Data, timestampUs: UInt64) throws {
        try ffi.writeFrame(payload: payload, timestampUs: timestampUs)
    }

    /// Mark the group complete. No more frames can be written.
    public func finish() throws {
        try ffi.finish()
    }
}
