import MoqFFI

/// The publish side of an origin: create broadcasts, serve them on demand,
/// and announce them so subscribers can discover them.
public final class OriginProducer: Sendable {
    let ffi: MoqOriginProducer

    /// Create a standalone origin, optionally capping its cache at `cacheCapacityBytes`
    /// (nil uses the default capacity).
    public init(cacheCapacityBytes: UInt64? = nil) {
        ffi = MoqOriginProducer(config: MoqOriginConfig(cacheCapacityBytes: cacheCapacityBytes))
    }

    init(_ ffi: MoqOriginProducer) {
        self.ffi = ffi
    }

    /// A read handle for this origin.
    public func consume() -> OriginConsumer {
        OriginConsumer(ffi.consume())
    }

    /// Advertise `prefix` and serve the requests beneath it.
    ///
    /// A route claims `prefix` and every path beneath it (`""` claims every path).
    /// A service that only serves some of them rejects the rest as they are
    /// requested. Create, `dynamic` if tracks are served on demand, populate,
    /// then `BroadcastProducer.announce`.
    public func dynamic(prefix: String, route: Route = Route()) throws -> OriginDynamic {
        OriginDynamic(try ffi.dynamic(prefix: prefix, route: route))
    }

    /// Create a broadcast at `path`, returning the producer that feeds it.
    ///
    /// The broadcast is invisible and unroutable, for this origin's consumers
    /// and peers alike, until `BroadcastProducer.announce(route:)`. Announce it
    /// after populating tracks. `finish()`
    /// unpublishes immediately, while releasing the producer without finishing
    /// also unpublishes but reads to subscribers as a failure rather than a
    /// deliberate end.
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

    /// Reject the request with an application error code.
    public func reject(errorCode: UInt16) throws {
        try ffi.reject(errorCode: errorCode)
    }
}

/// A stream of broadcasts requested by consumers. Iterate directly:
/// `for try await request in dynamic { ... }`. Hold this while missing
/// broadcasts should be served; cancelling the consuming task stops serving.
public final class OriginDynamic: AsyncSequence, Sendable {
    /// The broadcast request emitted by this sequence.
    public typealias Element = BroadcastRequest

    let ffi: MoqOriginDynamic

    init(_ ffi: MoqOriginDynamic) {
        self.ffi = ffi
    }

    /// The next requested broadcast. Throws `Closed` once the origin closes.
    public func requestedBroadcast() async throws -> BroadcastRequest {
        BroadcastRequest(try await ffi.requestedBroadcast())
    }

    /// Re-price the route in place: replace its hops and costs.
    public func update(route: Route) throws {
        try ffi.update(route: route)
    }

    /// Cancel all current and future `requestedBroadcast()` calls and retract
    /// the route.
    public func cancel() {
        ffi.cancel()
    }

    /// Create an iterator that cancels native waits when iteration ends.
    public func makeAsyncIterator() -> AsyncThrowingStream<BroadcastRequest, Swift.Error>.Iterator {
        moqStream(cancel: { [ffi] in ffi.cancel() }) { [ffi] in
            BroadcastRequest(try await ffi.requestedBroadcast())
        }.makeAsyncIterator()
    }
}

/// The subscribe side of an origin: discover and request published broadcasts.
public final class OriginConsumer: Sendable {
    let ffi: MoqOriginConsumer

    init(_ ffi: MoqOriginConsumer) {
        self.ffi = ffi
    }

    /// Stream routes under a literal prefix matching an optional pattern filter.
    /// Paths with a segment starting with `.` below the prefix are left out unless `hidden`.
    public func announced(prefix: String = "", filter: String? = nil, hidden: Bool = false) throws -> AnnounceConsumer {
        AnnounceConsumer(try ffi.announced(config: MoqAnnounceConfig(prefix: prefix, filter: filter, hidden: hidden)))
    }

    /// Wait for a route covering an exact path, then resolve the broadcast there.
    public func announcedBroadcast(path: String) throws -> AnnouncedBroadcast {
        AnnouncedBroadcast(try ffi.announcedBroadcast(path: path))
    }

    /// Request a broadcast by path, resolving as soon as it can be served through the
    /// best announced route covering it: an announced broadcast on this origin, a route
    /// a session announced (served on demand by that session), or a dynamic handler on
    /// the origin, or an error if nothing can serve it. Unlike `announcedBroadcast`,
    /// this does not wait for a future announcement.
    public func requestBroadcast(path: String) async throws -> BroadcastConsumer {
        BroadcastConsumer(try await ffi.requestBroadcast(path: path))
    }
}

/// A stream of announce events. Iterate directly:
/// `for try await event in announced { ... }`. A `.live` event follows the routes
/// live at subscribe time, so a loop can `break` there to list what is live.
/// The sequence ends when the origin closes; cancelling the consuming task
/// cancels the subscription.
public final class AnnounceConsumer: AsyncSequence, Sendable {
    /// The announce event emitted by this sequence.
    public typealias Element = AnnounceEvent

    let ffi: MoqAnnounceConsumer

    init(_ ffi: MoqAnnounceConsumer) {
        self.ffi = ffi
    }

    /// The next announce event, or `nil` once the origin closes.
    public func next() async throws -> AnnounceEvent? {
        try await ffi.next()
    }

    /// Cancel all current and future `next()` calls.
    public func cancel() {
        ffi.cancel()
    }

    /// Create an iterator that cancels native reads when iteration ends.
    public func makeAsyncIterator() -> AsyncThrowingStream<AnnounceEvent, Swift.Error>.Iterator {
        moqStream(cancel: { [ffi] in ffi.cancel() }) { [ffi] in
            try await ffi.next()
        }.makeAsyncIterator()
    }
}

/// A pending wait for a specific broadcast path.
public final class AnnouncedBroadcast: Sendable {
    let ffi: MoqAnnouncedBroadcast

    init(_ ffi: MoqAnnouncedBroadcast) {
        self.ffi = ffi
    }

    /// Suspend until a route covers the path, then resolve the broadcast there.
    /// Throws `Closed` if cancelled or the origin closes first.
    public func available() async throws -> BroadcastConsumer {
        BroadcastConsumer(try await ffi.available())
    }

    /// Cancel the pending `available()` call.
    public func cancel() {
        ffi.cancel()
    }
}
