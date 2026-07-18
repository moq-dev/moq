import MoqFFI

/// The publish side of an origin: create broadcasts so subscribers can
/// discover them.
public final class OriginProducer: Sendable {
    let ffi: MoqOriginProducer

    public init(cacheCapacityBytes: UInt64? = nil) {
        ffi = MoqOriginProducer(options: MoqOriginOptions(cacheCapacityBytes: cacheCapacityBytes))
    }

    init(_ ffi: MoqOriginProducer) {
        self.ffi = ffi
    }

    /// A read handle for this origin.
    public func consume() -> OriginConsumer {
        OriginConsumer(ffi.consume())
    }

    /// Serve broadcasts that consumers request without an announcement.
    public func dynamic() -> OriginDynamic {
        OriginDynamic(ffi.dynamic())
    }

    /// Create a broadcast at `path`, returning the producer that feeds it.
    ///
    /// The broadcast starts live: the origin announces the path so subscribers can
    /// discover it, becoming visible shortly after this returns. Toggle
    /// discoverability with `BroadcastProducer.setAnnounce(_:)`; `finish()` unpublishes
    /// immediately, while releasing the producer without finishing lingers briefly
    /// so a replacement publisher can take over.
    public func createBroadcast(path: String) throws -> BroadcastProducer {
        BroadcastProducer(try ffi.createBroadcast(path: path))
    }
}

/// A requested broadcast that has not been accepted yet.
public final class BroadcastRequest: Sendable {
    let ffi: MoqBroadcastRequest

    init(_ ffi: MoqBroadcastRequest) {
        self.ffi = ffi
    }

    /// The requested broadcast path.
    public var path: String {
        get throws { try ffi.path() }
    }

    /// Serve the request with an unannounced broadcast.
    public func accept(broadcast: BroadcastProducer) throws {
        try ffi.accept(broadcast: broadcast.ffi)
    }

    /// Abort the request with an application error code.
    public func abort(errorCode: UInt16) throws {
        try ffi.abort(errorCode: errorCode)
    }
}

/// A stream of broadcasts requested by consumers. Iterate directly:
/// `for try await request in dynamic { ... }`. Hold this while missing
/// broadcasts should be served; cancelling the consuming task stops serving.
public final class OriginDynamic: AsyncSequence, Sendable {
    public typealias Element = BroadcastRequest

    let ffi: MoqOriginDynamic

    init(_ ffi: MoqOriginDynamic) {
        self.ffi = ffi
    }

    /// The next requested broadcast. Throws `Closed` once the origin closes.
    public func requestedBroadcast() async throws -> BroadcastRequest {
        BroadcastRequest(try await ffi.requestedBroadcast())
    }

    /// Cancel all current and future `requestedBroadcast()` calls.
    public func cancel() {
        ffi.cancel()
    }

    public func makeAsyncIterator() -> AsyncThrowingStream<BroadcastRequest, Swift.Error>.Iterator {
        moqStream(cancel: { [ffi] in ffi.cancel() }) { [ffi] in
            BroadcastRequest(try await ffi.requestedBroadcast())
        }.makeAsyncIterator()
    }
}

/// The subscribe side of an origin: discover announced broadcasts.
public final class OriginConsumer: Sendable {
    let ffi: MoqOriginConsumer

    init(_ ffi: MoqOriginConsumer) {
        self.ffi = ffi
    }

    /// Stream every broadcast announced under a prefix.
    public func announced(prefix: String) throws -> Announced {
        Announced(try ffi.announced(prefix: prefix))
    }

    /// Wait for a single broadcast announced at an exact path.
    public func announcedBroadcast(path: String) throws -> AnnouncedBroadcast {
        AnnouncedBroadcast(try ffi.announcedBroadcast(path: path))
    }

    /// Request a broadcast by path, resolving as soon as it can be served: the announced
    /// broadcast if present, otherwise a dynamic fallback on the origin, or an error if
    /// neither can serve it. Unlike `announcedBroadcast`, this does not wait for a future
    /// announcement.
    public func requestBroadcast(path: String) async throws -> BroadcastConsumer {
        BroadcastConsumer(try await ffi.requestBroadcast(path: path))
    }
}

/// A stream of broadcast announcements. Iterate directly:
/// `for try await announcement in announced { ... }`. The sequence ends when the
/// origin closes; cancelling the consuming task cancels the subscription.
public final class Announced: AsyncSequence, Sendable {
    public typealias Element = Announcement

    let ffi: MoqAnnounced

    init(_ ffi: MoqAnnounced) {
        self.ffi = ffi
    }

    /// The next announcement, or `nil` once the origin closes.
    public func next() async throws -> Announcement? {
        (try await ffi.next()).map(Announcement.init)
    }

    /// Cancel all current and future `next()` calls.
    public func cancel() {
        ffi.cancel()
    }

    public func makeAsyncIterator() -> AsyncThrowingStream<Announcement, Swift.Error>.Iterator {
        moqStream(cancel: { [ffi] in ffi.cancel() }) { [ffi] in
            (try await ffi.next()).map(Announcement.init)
        }.makeAsyncIterator()
    }
}

/// A single broadcast announcement.
public final class Announcement: Sendable {
    let ffi: MoqAnnouncement

    init(_ ffi: MoqAnnouncement) {
        self.ffi = ffi
    }

    /// The path of the announced broadcast.
    public var path: String {
        ffi.path()
    }

    /// A consumer for the announced broadcast.
    public var broadcast: BroadcastConsumer {
        BroadcastConsumer(ffi.broadcast())
    }
}

/// A pending wait for a specific broadcast path.
public final class AnnouncedBroadcast: Sendable {
    let ffi: MoqAnnouncedBroadcast

    init(_ ffi: MoqAnnouncedBroadcast) {
        self.ffi = ffi
    }

    /// Suspend until the broadcast is announced. Throws `Closed` if cancelled or
    /// the origin closes first.
    public func available() async throws -> BroadcastConsumer {
        BroadcastConsumer(try await ffi.available())
    }

    /// Cancel the pending `available()` call.
    public func cancel() {
        ffi.cancel()
    }
}
