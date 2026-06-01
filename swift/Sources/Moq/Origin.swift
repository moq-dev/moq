import MoqFFI

/// The publish side of an origin: announce local broadcasts so subscribers can
/// discover them.
public final class OriginProducer: Sendable {
    let ffi: MoqOriginProducer

    public init() {
        ffi = MoqOriginProducer()
    }

    init(_ ffi: MoqOriginProducer) {
        self.ffi = ffi
    }

    /// A read handle for this origin.
    public func consume() -> OriginConsumer {
        OriginConsumer(ffi.consume())
    }

    /// Announce a broadcast under the given path so subscribers can find it.
    public func announce(path: String, broadcast: BroadcastProducer) throws {
        try ffi.announce(path: path, broadcast: broadcast.ffi)
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
