import Foundation
import MoqFFI

/// JSON tracks, mirroring the `moq-json` crate.
///
/// Each type wraps a track: a producer takes over a `TrackProducer` and advertises it in the
/// broadcast's catalog, and a consumer takes over a `TrackConsumer` that has not read a group
/// yet. Values cross the FFI boundary as JSON, encoded with `JSONEncoder` and decoded with
/// `JSONDecoder`. `JSONEncoder` rejects a top-level scalar (a bare number/string/bool) on
/// Foundation before the swift-foundation rewrite (iOS < 18 / macOS < 15), so encode an object
/// or array there.
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
        /// `deltaRatio` controls how aggressively merge-patch deltas replace full snapshots (`0`
        /// disables deltas). Set `compression` to DEFLATE each group; the consumer must pass the
        /// same flag.
        public init(
            broadcast: BroadcastProducer,
            track: TrackProducer,
            deltaRatio: UInt32 = MoqJsonSnapshotConfig().deltaRatio,
            compression: Bool = false
        ) throws {
            ffi = try MoqJsonSnapshotProducer(
                broadcast: broadcast.ffi,
                track: track.ffi,
                config: MoqJsonSnapshotConfig(deltaRatio: deltaRatio, compression: compression)
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
    /// subscription's `maxDelayUs` are skipped; `latest()` skips to the newest state instead.
    public final class SnapshotConsumer<Value: Decodable & Sendable>: AsyncSequence, Sendable {
        /// The decoded snapshot state emitted by this sequence.
        public typealias Element = Timed<Value>

        let ffi: MoqJsonSnapshotConsumer

        /// Read `track` as a JSON snapshot track. Takes over `track`, whose handle is closed
        /// afterward; throws if it has already read a group. `compression` must match the flag
        /// the producer used.
        public init(track: TrackConsumer, compression: Bool = false) throws {
            // deltaRatio is producer-only, so leave it at its default here.
            ffi = try MoqJsonSnapshotConsumer(track: track.ffi, config: MoqJsonSnapshotConfig(compression: compression))
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
        /// already carries the track's name. Set `compression` to DEFLATE the group; the consumer
        /// must pass the same flag.
        public init(broadcast: BroadcastProducer, track: TrackProducer, compression: Bool = false) throws {
            ffi = try MoqJsonStreamProducer(
                broadcast: broadcast.ffi,
                track: track.ffi,
                config: MoqJsonStreamConfig(compression: compression)
            )
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
        /// afterward; throws if it has already read a group. `compression` must match the flag
        /// the producer used.
        public init(track: TrackConsumer, compression: Bool = false) throws {
            ffi = try MoqJsonStreamConsumer(track: track.ffi, config: MoqJsonStreamConfig(compression: compression))
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

extension Json.Timed: Sendable where Value: Sendable {}
extension Json.Timed: Equatable where Value: Equatable {}

private func encodeJson(_ value: some Encodable) throws -> String {
    String(decoding: try JSONEncoder().encode(value), as: UTF8.self)
}

private func decodeJson<Value: Decodable>(_ json: String) throws -> Value {
    try JSONDecoder().decode(Value.self, from: Data(json.utf8))
}
