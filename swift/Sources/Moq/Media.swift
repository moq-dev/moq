import Foundation
import MoqFFI

/// Read side of a broadcast's catalog. Iterating yields catalog updates as the
/// set of tracks changes.
public final class CatalogConsumer: AsyncSequence, Sendable {
    public typealias Element = Catalog

    let ffi: MoqCatalogConsumer

    init(_ ffi: MoqCatalogConsumer) {
        self.ffi = ffi
    }

    /// The next catalog update, or `nil` once the track ends or is closed.
    public func next() async throws -> Catalog? {
        try await ffi.next()
    }

    /// Cancel all current and future reads.
    public func cancel() {
        ffi.cancel()
    }

    public func makeAsyncIterator() -> AsyncThrowingStream<Catalog, Swift.Error>.Iterator {
        moqStream(cancel: { [ffi] in ffi.cancel() }) { [ffi] in
            try await ffi.next()
        }.makeAsyncIterator()
    }
}

/// Read side of a media track. Iterating yields decoded frames in decode order.
public final class MediaConsumer: AsyncSequence, Sendable {
    public typealias Element = Frame

    let ffi: MoqMediaConsumer

    init(_ ffi: MoqMediaConsumer) {
        self.ffi = ffi
    }

    /// The next frame, or `nil` once the track ends or is closed.
    public func next() async throws -> Frame? {
        try await ffi.next()
    }

    /// Cancel all current and future reads.
    public func cancel() {
        ffi.cancel()
    }

    public func makeAsyncIterator() -> AsyncThrowingStream<Frame, Swift.Error>.Iterator {
        moqStream(cancel: { [ffi] in ffi.cancel() }) { [ffi] in
            try await ffi.next()
        }.makeAsyncIterator()
    }
}

/// Write side of a media track fed pre-framed payloads.
public final class MediaProducer: Sendable {
    let ffi: MoqMediaProducer

    init(_ ffi: MoqMediaProducer) {
        self.ffi = ffi
    }

    /// The track's name.
    public var name: String {
        get throws { try ffi.name() }
    }

    /// Suspend until the track has at least one active consumer.
    public func used() async throws {
        try await ffi.used()
    }

    /// Suspend until the track has no active consumers.
    public func unused() async throws {
        try await ffi.unused()
    }

    /// Write a frame with the given presentation timestamp (microseconds).
    public func writeFrame(_ payload: Data, timestampUs: UInt64) throws {
        try ffi.writeFrame(payload: payload, timestampUs: timestampUs)
    }

    /// Finish the track and finalize encoding.
    public func finish() throws {
        try ffi.finish()
    }
}

/// Write side of a media track fed a raw byte stream with inferred frame boundaries.
public final class MediaStreamProducer: Sendable {
    let ffi: MoqMediaStreamProducer

    init(_ ffi: MoqMediaStreamProducer) {
        self.ffi = ffi
    }

    /// Push raw stream bytes (e.g. Annex-B H.264). The importer frames whole
    /// access units and buffers any partial trailing frame for the next call.
    public func write(_ payload: Data) throws {
        try ffi.write(payload: payload)
    }

    /// Finalize the track. A trailing access unit with no following delimiter is
    /// not emitted (matches the moq-cli stdin path).
    public func finish() throws {
        try ffi.finish()
    }
}
