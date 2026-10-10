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
        public func update(_ value: Value) throws {
            try ffi.update(value: encodeJson(value))
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

    /// Read side of a JSON snapshot track (lossy latest-value). Iterating yields the latest value,
    /// collapsing the backlog for a reader that has fallen behind: `for try await value in json { ... }`.
    public final class SnapshotConsumer<Value: Decodable & Sendable>: AsyncSequence, Sendable {
        /// The decoded snapshot value emitted by this sequence.
        public typealias Element = Value

        let ffi: MoqJsonSnapshotConsumer

        /// Read `track` as a JSON snapshot track. Takes over `track`, whose handle is closed
        /// afterward; throws if it has already read a group. `compression` must match the flag
        /// the producer used.
        public init(track: TrackConsumer, compression: Bool = false) throws {
            // deltaRatio is producer-only, so leave it at its default here.
            ffi = try MoqJsonSnapshotConsumer(track: track.ffi, config: MoqJsonSnapshotConfig(compression: compression))
        }

        /// The next value, decoded as `Value`, or `nil` once the track ends or is closed.
        public func next() async throws -> Value? {
            guard let json = try await ffi.next() else { return nil }
            return try decodeJson(json) as Value
        }

        /// Cancel all current and future reads.
        public func cancel() {
            ffi.cancel()
        }

        /// Create an iterator that cancels native reads when iteration ends.
        public func makeAsyncIterator() -> AsyncThrowingStream<Value, Swift.Error>.Iterator {
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
        public func append(_ value: Value) throws {
            try ffi.append(value: encodeJson(value))
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
        public typealias Element = Value

        let ffi: MoqJsonStreamConsumer

        /// Read `track` as a JSON stream track. Takes over `track`, whose handle is closed
        /// afterward; throws if it has already read a group. `compression` must match the flag
        /// the producer used.
        public init(track: TrackConsumer, compression: Bool = false) throws {
            ffi = try MoqJsonStreamConsumer(track: track.ffi, config: MoqJsonStreamConfig(compression: compression))
        }

        /// The next record, decoded as `Value`, or `nil` once the track ends or is closed.
        public func next() async throws -> Value? {
            guard let json = try await ffi.next() else { return nil }
            return try decodeJson(json) as Value
        }

        /// Cancel all current and future reads.
        public func cancel() {
            ffi.cancel()
        }

        /// Create an iterator that cancels native reads when iteration ends.
        public func makeAsyncIterator() -> AsyncThrowingStream<Value, Swift.Error>.Iterator {
            moqStream(cancel: { [ffi] in ffi.cancel() }) { [self] in
                try await next()
            }.makeAsyncIterator()
        }
    }
}

private func encodeJson(_ value: some Encodable) throws -> String {
    String(decoding: try JSONEncoder().encode(value), as: UTF8.self)
}

private func decodeJson<Value: Decodable>(_ json: String) throws -> Value {
    try JSONDecoder().decode(Value.self, from: Data(json.utf8))
}
