import Foundation
import MoqFFI

/// JSON tracks, mirroring the `moq-json` crate.
///
/// Each type wraps a track: a producer takes over a `TrackProducer` (the snapshot and stream
/// producers also advertise it in the broadcast's catalog, which has no window mode), and a
/// consumer takes over a `TrackConsumer` that has not read a group yet. Values cross the FFI boundary as JSON, encoded with `JSONEncoder` and decoded with
/// `JSONDecoder`. `JSONEncoder` rejects a top-level scalar (a bare number/string/bool) on
/// Foundation before the swift-foundation rewrite (iOS < 18 / macOS < 15), so encode an object
/// or array there.
///
/// Compression is the track's: publish JSON with `TrackInfo(compress: true)`, and a consumer reads
/// it with no option of its own.
public enum Json {}

extension Json {
    /// A decoded JSON value and the timestamp of the frame that carried it.
    public struct Timed<Value> {
        /// The decoded value.
        public let value: Value
        /// The frame's timestamp on the broadcast clock in microseconds, or `nil` when untimed.
        /// Rounded down to a whole microsecond, so a tick on a track like 90 kHz may not survive
        /// a read and republish exactly.
        public let timestampUs: UInt64?

        /// Pair `value` with its frame timestamp.
        public init(value: Value, timestampUs: UInt64?) {
            self.value = value
            self.timestampUs = timestampUs
        }

        init?(_ ffi: MoqJsonValue?) throws where Value: Decodable {
            guard let ffi else { return nil }
            self.init(value: try decodeJson(ffi.value), timestampUs: ffi.timestampUs)
        }
    }

    /// Write side of a JSON snapshot track (lossy latest-value).
    ///
    /// Each `update` supersedes the last, so a late joiner only sees the newest value.
    public final class SnapshotProducer<Value: Encodable>: Sendable {
        let ffi: MoqJsonSnapshotProducer

        /// Publish `track` as a JSON snapshot track, advertised in `broadcast`'s catalog until it
        /// finishes. Takes over `track`, whose handle is closed afterward; throws if the catalog
        /// already carries the track's name.
        ///
        /// `deltas` encodes updates as merge-patch deltas; with it off every update is a full
        /// snapshot in its own group. With deltas on, give the track a `refresh` ratio so a joiner
        /// past it gets the value restated in a fresh group.
        public init(
            broadcast: BroadcastProducer,
            track: TrackProducer,
            deltas: Bool = MoqJsonSnapshotConfig().deltas
        ) throws {
            ffi = try MoqJsonSnapshotProducer(
                broadcast: broadcast.ffi,
                track: track.ffi,
                config: MoqJsonSnapshotConfig(deltas: deltas)
            )
        }

        /// Publish a new value, encoded as a snapshot or merge-patch delta automatically. A no-op
        /// if unchanged from the previous update.
        public func update(_ value: Value, timestampUs: UInt64? = nil) throws {
            try ffi.update(value: encodeJson(value), timestampUs: timestampUs)
        }

        /// A watch-only handle to whether the track has subscribers.
        public func demand() throws -> TrackDemand {
            TrackDemand(try ffi.demand())
        }

        /// Finish the track, closing any open group.
        public func finish() throws {
            try ffi.finish()
        }
    }

    /// Read side of a JSON snapshot track (lossy latest-value). Iterating yields each state in
    /// order with its timestamp: `for try await state in json { ... }`. Groups older than the
    /// subscription's `maxDelay` are skipped; `latest()` skips to the newest state instead.
    public final class SnapshotConsumer<Value: Decodable & Sendable>: AsyncSequence, Sendable {
        /// The decoded snapshot state emitted by this sequence.
        public typealias Element = Timed<Value>

        let ffi: MoqJsonSnapshotConsumer

        /// Read `track` as a JSON snapshot track. Takes over `track`, whose handle is closed
        /// afterward; throws if it has already read a group.
        public init(track: TrackConsumer) throws {
            ffi = try MoqJsonSnapshotConsumer(track: track.ffi)
        }

        /// The next state in order, or `nil` once the track ends or is closed.
        public func next() async throws -> Timed<Value>? {
            try Timed(try await ffi.next())
        }

        /// Skip to the newest state, or wait for one; `nil` once the track ends or is closed.
        /// Skipped states are gone, and `next()` continues after this one.
        public func latest() async throws -> Timed<Value>? {
            try Timed(try await ffi.latest())
        }

        /// Cancel all current and future reads.
        public func cancel() {
            ffi.cancel()
        }

        /// Create an iterator that cancels native reads when iteration ends.
        public func makeAsyncIterator() -> AsyncThrowingStream<Timed<Value>, Swift.Error>.Iterator {
            moqStream(cancel: { [ffi] in ffi.cancel() }) { [self] in
                try await next()
            }.makeAsyncIterator()
        }
    }

    /// Write side of a JSON stream track (lossless append-log).
    ///
    /// Every `append` is preserved and delivered in order.
    public final class StreamProducer<Value: Encodable>: Sendable {
        let ffi: MoqJsonStreamProducer

        /// Publish `track` as a JSON stream track, advertised in `broadcast`'s catalog until it
        /// finishes. Takes over `track`, whose handle is closed afterward; throws if the catalog
        /// already carries the track's name.
        public init(broadcast: BroadcastProducer, track: TrackProducer) throws {
            ffi = try MoqJsonStreamProducer(broadcast: broadcast.ffi, track: track.ffi)
        }

        /// Append one record to the log.
        public func append(_ value: Value, timestampUs: UInt64? = nil) throws {
            try ffi.append(value: encodeJson(value), timestampUs: timestampUs)
        }

        /// A watch-only handle to whether the track has subscribers.
        public func demand() throws -> TrackDemand {
            TrackDemand(try ffi.demand())
        }

        /// Finish the track, closing the group.
        public func finish() throws {
            try ffi.finish()
        }
    }

    /// Read side of a JSON stream track (lossless append-log). Iterating yields every record in
    /// order: `for try await record in json { ... }`.
    public final class StreamConsumer<Value: Decodable & Sendable>: AsyncSequence, Sendable {
        /// The decoded stream record emitted by this sequence.
        public typealias Element = Timed<Value>

        let ffi: MoqJsonStreamConsumer

        /// Read `track` as a JSON stream track. Takes over `track`, whose handle is closed
        /// afterward; throws if it has already read a group.
        public init(track: TrackConsumer) throws {
            ffi = try MoqJsonStreamConsumer(track: track.ffi)
        }

        /// The next record, or `nil` once the track ends or is closed.
        public func next() async throws -> Timed<Value>? {
            try Timed(try await ffi.next())
        }

        /// Cancel all current and future reads.
        public func cancel() {
            ffi.cancel()
        }

        /// Create an iterator that cancels native reads when iteration ends.
        public func makeAsyncIterator() -> AsyncThrowingStream<Timed<Value>, Swift.Error>.Iterator {
            moqStream(cancel: { [ffi] in ffi.cancel() }) { [self] in
                try await next()
            }.makeAsyncIterator()
        }
    }
}

extension Json {
    /// Write side of a JSON window track: records pushed at the back and popped from the front.
    ///
    /// The window rolls to new groups that restate the records it retains, so a reader can join at
    /// any point. Not advertised in the broadcast's catalog; a reader names the track.
    public final class WindowProducer<Value: Encodable>: Sendable {
        let ffi: MoqJsonWindowProducer

        /// Publish `track` as a JSON window track. Takes over `track`, whose handle is closed
        /// afterward.
        ///
        /// `checkpointRecords` bounds the records retained and restated for a window that is never
        /// popped: a reader that keeps up still sees every record, while a later joiner is told the
        /// older ones were skipped. Give the track a `refresh` ratio so a joiner past it gets the
        /// window restated in a fresh group.
        public init(track: TrackProducer, checkpointRecords: UInt64? = nil) throws {
            ffi = try MoqJsonWindowProducer(
                track: track.ffi,
                config: MoqJsonWindowConfig(checkpointRecords: checkpointRecords)
            )
        }

        /// Append one record to the back of the window.
        public func push(_ value: Value, timestampUs: UInt64? = nil) throws {
            try ffi.push(value: encodeJson(value), timestampUs: timestampUs)
        }

        /// Drop `count` records from the front of the window, clamped to what it holds.
        public func pop(_ count: UInt64, timestampUs: UInt64? = nil) throws {
            try ffi.pop(count: count, timestampUs: timestampUs)
        }

        /// Close the open group, so the next edit opens one restating the retained window. A no-op
        /// when no group is open. Cut at a natural boundary, such as the end of an utterance.
        public func cut() throws {
            try ffi.cut()
        }

        /// A watch-only handle to whether the track has subscribers.
        public func demand() throws -> TrackDemand {
            TrackDemand(try ffi.demand())
        }

        /// Finish the track, closing any open group.
        public func finish() throws {
            try ffi.finish()
        }
    }

    /// One change to a window, as a `WindowConsumer` sees it. Ranges are absolute record indices.
    public enum WindowEvent<Value> {
        /// A record joined the window at this index.
        case push(index: UInt64, value: Value)
        /// These records left the window.
        case pop(Range<UInt64>)
        /// These records will never be delivered: they were pushed and dropped while this reader
        /// was behind, or fell outside the producer's checkpoint when it joined.
        case skip(Range<UInt64>)
    }

    /// Read side of a JSON window track. Iterating yields each change in order with its
    /// timestamp: `for try await event in window { ... }`. Group rolls never surface.
    public final class WindowConsumer<Value: Decodable & Sendable>: AsyncSequence, Sendable {
        /// The decoded window change emitted by this sequence.
        public typealias Element = Timed<WindowEvent<Value>>

        let ffi: MoqJsonWindowConsumer

        /// Read `track` as a JSON window track. Takes over `track`, whose handle is closed
        /// afterward; throws if it has already read a group.
        public init(track: TrackConsumer) throws {
            ffi = try MoqJsonWindowConsumer(track: track.ffi)
        }

        /// The next change, or `nil` once the track ends or is closed. A record first learned of
        /// from a restatement carries that frame's timestamp, not its original push time.
        public func next() async throws -> Element? {
            guard let timed = try await ffi.next() else { return nil }
            let event: WindowEvent<Value>
            switch timed.event {
            case let .push(index, value): event = .push(index: index, value: try decodeJson(value))
            case let .pop(start, end): event = .pop(start..<end)
            case let .skip(start, end): event = .skip(start..<end)
            }
            return Timed(value: event, timestampUs: timed.timestampUs)
        }

        /// Cancel all current and future reads.
        public func cancel() {
            ffi.cancel()
        }

        /// Create an iterator that cancels native reads when iteration ends.
        public func makeAsyncIterator() -> AsyncThrowingStream<Element, Swift.Error>.Iterator {
            moqStream(cancel: { [ffi] in ffi.cancel() }) { [self] in
                try await next()
            }.makeAsyncIterator()
        }
    }
}

extension Json.WindowEvent: Sendable where Value: Sendable {}
extension Json.WindowEvent: Equatable where Value: Equatable {}
extension Json.Timed: Sendable where Value: Sendable {}
extension Json.Timed: Equatable where Value: Equatable {}

private func encodeJson(_ value: some Encodable) throws -> String {
    String(decoding: try JSONEncoder().encode(value), as: UTF8.self)
}

private func decodeJson<Value: Decodable>(_ json: String) throws -> Value {
    try JSONDecoder().decode(Value.self, from: Data(json.utf8))
}
