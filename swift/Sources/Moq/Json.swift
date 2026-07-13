import Foundation
import MoqFFI

/// Write side of a JSON snapshot track (lossy latest-value).
///
/// Built via `BroadcastProducer.publishJsonSnapshot`. Each `update` supersedes the last, so a
/// late joiner only sees the newest value. Values cross the FFI boundary as JSON, encoded from
/// `Value` with `JSONEncoder`, so `Value` must encode to a JSON object or array: `JSONEncoder`
/// rejects a top-level scalar (a bare number/string/bool) on Foundation before the swift-foundation
/// rewrite (iOS < 18 / macOS < 15).
public final class JsonSnapshotProducer<Value: Encodable>: Sendable {
    let ffi: MoqJsonSnapshotProducer

    init(_ ffi: MoqJsonSnapshotProducer) {
        self.ffi = ffi
    }

    /// Publish a new value, encoded as a snapshot or merge-patch delta automatically. A no-op
    /// if unchanged from the previous update.
    public func update(_ value: Value) throws {
        try ffi.update(value: encodeJson(value))
    }

    /// Finish the track, closing any open group.
    public func finish() throws {
        try ffi.finish()
    }
}

/// Read side of a JSON snapshot track (lossy latest-value). Iterating yields the latest value,
/// collapsing the backlog for a reader that has fallen behind: `for try await value in json { ... }`.
public final class JsonSnapshotConsumer<Value: Decodable & Sendable>: AsyncSequence, Sendable {
    public typealias Element = Value

    let ffi: MoqJsonSnapshotConsumer

    init(_ ffi: MoqJsonSnapshotConsumer) {
        self.ffi = ffi
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

    public func makeAsyncIterator() -> AsyncThrowingStream<Value, Swift.Error>.Iterator {
        moqStream(cancel: { [ffi] in ffi.cancel() }) { [self] in
            try await next()
        }.makeAsyncIterator()
    }
}

/// Write side of a JSON stream track (lossless append-log).
///
/// Built via `BroadcastProducer.publishJsonStream`. Every `append` is preserved and delivered
/// in order. Values cross the FFI boundary as JSON, encoded from `Value` with `JSONEncoder`, so
/// `Value` must encode to a JSON object or array: `JSONEncoder` rejects a top-level scalar (a bare
/// number/string/bool) on Foundation before the swift-foundation rewrite (iOS < 18 / macOS < 15).
public final class JsonStreamProducer<Value: Encodable>: Sendable {
    let ffi: MoqJsonStreamProducer

    init(_ ffi: MoqJsonStreamProducer) {
        self.ffi = ffi
    }

    /// Append one record to the log.
    public func append(_ value: Value) throws {
        try ffi.append(value: encodeJson(value))
    }

    /// Finish the track, closing the group.
    public func finish() throws {
        try ffi.finish()
    }
}

/// Read side of a JSON stream track (lossless append-log). Iterating yields every record in
/// order: `for try await record in json { ... }`.
public final class JsonStreamConsumer<Value: Decodable & Sendable>: AsyncSequence, Sendable {
    public typealias Element = Value

    let ffi: MoqJsonStreamConsumer

    init(_ ffi: MoqJsonStreamConsumer) {
        self.ffi = ffi
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

    public func makeAsyncIterator() -> AsyncThrowingStream<Value, Swift.Error>.Iterator {
        moqStream(cancel: { [ffi] in ffi.cancel() }) { [self] in
            try await next()
        }.makeAsyncIterator()
    }
}

private func encodeJson(_ value: some Encodable) throws -> String {
    String(decoding: try JSONEncoder().encode(value), as: UTF8.self)
}

private func decodeJson<Value: Decodable>(_ json: String) throws -> Value {
    try JSONDecoder().decode(Value.self, from: Data(json.utf8))
}
