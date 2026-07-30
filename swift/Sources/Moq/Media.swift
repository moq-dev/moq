import Foundation
import MoqFFI

/// Read side of a broadcast's catalog. Iterating yields catalog updates as the
/// set of tracks changes.
public final class CatalogConsumer: AsyncSequence, Sendable {
    /// The catalog update emitted by this sequence.
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

    /// Create an iterator that cancels native reads when iteration ends.
    public func makeAsyncIterator() -> AsyncThrowingStream<Catalog, Swift.Error>.Iterator {
        moqStream(cancel: { [ffi] in ffi.cancel() }) { [ffi] in
            try await ffi.next()
        }.makeAsyncIterator()
    }
}

/// Read side of a media track. Iterating yields decoded frames in decode order.
public final class MediaConsumer: AsyncSequence, Sendable {
    /// The decoded media frame emitted by this sequence.
    public typealias Element = MediaFrame

    let ffi: MoqMediaConsumer

    init(_ ffi: MoqMediaConsumer) {
        self.ffi = ffi
    }

    /// The next frame, or `nil` once the track ends or is closed.
    public func next() async throws -> MediaFrame? {
        try await ffi.next()
    }

    /// Cancel all current and future reads.
    public func cancel() {
        ffi.cancel()
    }

    /// Create an iterator that cancels native reads when iteration ends.
    public func makeAsyncIterator() -> AsyncThrowingStream<MediaFrame, Swift.Error>.Iterator {
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
    ///
    /// The importer derives keyframe status from the bitstream, so only the payload and its
    /// timestamp cross the boundary.
    public func writeFrame(_ payload: Data, timestampUs: UInt64 = 0) throws {
        try ffi.writeFrame(frame: Frame(payload: payload, timestampUs: timestampUs))
    }

    /// Draw a group boundary here.
    ///
    /// Audio has no boundary of its own (every packet is independently decodable), so this is the
    /// only thing that gives it groups: call it after every frame for one group (one QUIC stream)
    /// the relay forwards without waiting, or at a segment cadence to align with video. Video
    /// groups at its own keyframes and needs this only to override that.
    ///
    /// On a container this declares a new segment, rolling a group on every track it publishes.
    public func cut() throws {
        try ffi.cut()
    }

    /// Draw a group boundary and number the next group `sequence`.
    ///
    /// ``cut()`` with an explicit sequence, for a publisher whose group numbers have to be
    /// deterministic: two encoders aligning per GOP so a consumer can fail over between them.
    public func seek(_ sequence: UInt64) throws {
        try ffi.seek(sequence: sequence)
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
