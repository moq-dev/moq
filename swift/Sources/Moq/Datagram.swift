import Foundation
import MoqFFI

extension BroadcastProducer {
    /// Create a live datagram track in its own namespace.
    public func createDatagrams(name: String, info: DatagramInfo? = nil) throws -> DatagramProducer {
        DatagramProducer(try ffi.createDatagrams(name: name, info: info))
    }
}

extension BroadcastConsumer {
    /// Subscribe to a live datagram track from now on, without a stream subscription.
    public func datagrams(name: String) throws -> DatagramConsumer {
        DatagramConsumer(try ffi.datagrams(name: name))
    }
}

/// Writes live datagrams with an independent sequence counter and no cache.
public final class DatagramProducer: Sendable {
    let ffi: MoqDatagramProducer

    init(_ ffi: MoqDatagramProducer) { self.ffi = ffi }

    /// The datagram track's name within its broadcast.
    public func name() throws -> String { try ffi.name() }

    /// The track's fixed publisher properties.
    public func info() throws -> DatagramInfo { try ffi.info() }

    /// Send a datagram and return its sequence; keep the payload below the path MTU.
    public func append(_ payload: Data, timestamp: Timestamp? = nil) throws -> UInt64 {
        try ffi.append(timestamp: timestamp, payload: payload)
    }

    /// Send a datagram at its explicit sequence; appends continue past the highest one.
    public func insert(_ datagram: Datagram) throws { try ffi.insert(datagram: datagram) }

    /// Create a local reader starting with the next datagram written.
    public func consume() throws -> DatagramConsumer { DatagramConsumer(try ffi.consume()) }

    /// The highest subscriber priority, or nil while nobody consumes this track.
    public func priority() throws -> Int8? { try ffi.priority() }

    /// Wait until aggregate subscriber priority differs from last.
    public func priorityChanged(last: Int8?) async throws -> Int8? {
        try await ffi.priorityChanged(last: last)
    }

    /// Finish the track; existing consumers drain their unread datagrams.
    public func finish() throws { try ffi.finish() }

    /// Abort the track with an application error, discarding unread datagrams.
    public func abort(errorCode: UInt16) throws { try ffi.abort(errorCode: errorCode) }
}

/// Reads live datagrams in arrival order from its own cursor.
public final class DatagramConsumer: AsyncSequence, Sendable {
    public typealias Element = Datagram
    public typealias AsyncIterator = AsyncThrowingStream<Datagram, Swift.Error>.Iterator
    let ffi: MoqDatagramConsumer

    init(_ ffi: MoqDatagramConsumer) { self.ffi = ffi }

    /// The datagram track's name within its broadcast.
    public func name() throws -> String { try ffi.name() }

    /// Wait for the publisher to accept this track and return its properties.
    public func info() async throws -> DatagramInfo { try await ffi.info() }

    /// Receive the next datagram, or nil after the track finishes.
    public func recv() async throws -> Datagram? { try await ffi.recv() }

    /// This consumer's subscriber priority.
    public func priority() throws -> Int8 { try ffi.priority() }

    /// Change this consumer's subscriber priority, higher first.
    public func setPriority(_ priority: Int8) throws { try ffi.setPriority(priority: priority) }

    /// Cancel reads and release this consumer's demand.
    public func cancel() { ffi.cancel() }

    /// Iterate datagrams, cancelling native reads when iteration ends.
    public func makeAsyncIterator() -> AsyncIterator {
        moqStream(cancel: { [ffi] in ffi.cancel() }) { [ffi] in
            try await ffi.recv()
        }.makeAsyncIterator()
    }
}

extension BroadcastRequests {
    /// Create an on-demand handler for datagram tracks.
    public func datagrams() throws -> DatagramRequests { DatagramRequests(try ffi.datagrams()) }
}

/// Serves on-demand datagram tracks through their own request queue.
public final class DatagramRequests: AsyncSequence, Sendable {
    public typealias Element = DatagramRequest
    public typealias AsyncIterator = AsyncThrowingStream<DatagramRequest, Swift.Error>.Iterator
    let ffi: MoqDatagramRequests

    init(_ ffi: MoqDatagramRequests) { self.ffi = ffi }

    /// Wait for the next datagram track requested by a consumer.
    public func requested() async throws -> DatagramRequest { DatagramRequest(try await ffi.requested()) }

    /// Cancel the handler and reject requests it has not handed out.
    public func cancel() { ffi.cancel() }

    /// Iterate datagram requests, cancelling native waits when iteration ends.
    public func makeAsyncIterator() -> AsyncIterator {
        moqStream(cancel: { [ffi] in ffi.cancel() }) { [ffi] in
            DatagramRequest(try await ffi.requested())
        }.makeAsyncIterator()
    }
}

/// A requested datagram track awaiting acceptance or rejection.
public final class DatagramRequest: Sendable {
    let ffi: MoqDatagramRequest

    init(_ ffi: MoqDatagramRequest) { self.ffi = ffi }

    /// The requested datagram track's name.
    public func name() throws -> String { try ffi.name() }

    /// The highest priority among consumers waiting for this track.
    public func priority() throws -> Int8? { try ffi.priority() }

    /// Accept the request with its fixed publisher properties.
    public func accept(info: DatagramInfo? = nil) throws -> DatagramProducer {
        DatagramProducer(try ffi.accept(info: info))
    }

    /// Reject this request with an application error code.
    public func abort(errorCode: UInt16) throws { try ffi.abort(errorCode: errorCode) }
}
