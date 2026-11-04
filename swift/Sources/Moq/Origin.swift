import MoqFFI

/// The publish side of an origin: create routes that own broadcasts or serve
/// them on demand, so subscribers can discover them.
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

    /// Create a route at exactly `path`, owning a new broadcast to fill.
    ///
    /// Visible at once unless `hidden`: a hidden route is invisible and unroutable, for this
    /// origin's consumers and peers alike, until `Route.announce()`, so create it hidden to
    /// fill the catalog first. It serves a fresh instance, kept across `withdraw` and
    /// `announce`, unless `instance` names one, with `epoch` ordering its runs; `cost` prices
    /// it, lower wins. Hold the route: releasing or cancelling it retracts it, and
    /// `BroadcastProducer.close()` ends both.
    public func route(
        path: String, hidden: Bool = false, cost: UInt64 = 0, instance: String? = nil, epoch: UInt64 = 0
    ) throws -> Route {
        Route(
            try ffi.route(
                path: path,
                options: MoqRouteOptions(hidden: hidden, cost: cost, instance: instance, epoch: epoch)))
    }

    /// Create a route over `prefix` that serves the requests beneath it.
    ///
    /// A route claims `prefix` and every path beneath it (`""` claims every path).
    /// A service that only serves some of them rejects the rest as they are
    /// requested. Iterate the route, or call `requested()`, to answer each request; hold
    /// it while it should stay advertised.
    public func routePrefix(
        prefix: String, hidden: Bool = false, cost: UInt64 = 0, instance: String? = nil, epoch: UInt64 = 0
    ) throws -> Route {
        Route(
            try ffi.routePrefix(
                prefix: prefix,
                options: MoqRouteOptions(hidden: hidden, cost: cost, instance: instance, epoch: epoch)))
    }
}

/// A requested broadcast that has not been answered yet.
public final class BroadcastRequest: Sendable {
    let ffi: MoqBroadcastRequest

    init(_ ffi: MoqBroadcastRequest) {
        self.ffi = ffi
    }

    /// The requested broadcast path.
    public var path: String {
        get throws { try ffi.path() }
    }

    /// Answer with the path's own exact route, owning a new broadcast.
    ///
    /// It serves a fresh instance unless `instance` names one. Created visible, it announces
    /// now and answers the waiting requesters; created `hidden`, they wait until
    /// `Route.announce()`, so the job can fill its catalog first.
    public func route(
        hidden: Bool = false, cost: UInt64 = 0, instance: String? = nil, epoch: UInt64 = 0
    ) throws -> Route {
        Route(
            try ffi.route(options: MoqRouteOptions(hidden: hidden, cost: cost, instance: instance, epoch: epoch)))
    }

    /// Serve held content under the prefix route's continuous sequence space, without an exact route.
    public func serve(broadcast: BroadcastConsumer) throws {
        try ffi.serve(broadcast: broadcast.ffi)
    }

    /// Reject the request with an application error code.
    public func reject(errorCode: UInt16) throws {
        try ffi.reject(errorCode: errorCode)
    }
}

/// A route on an origin: an exact route owning one broadcast, or a prefix route yielding
/// the requests beneath it. Iterate a prefix route directly:
/// `for try await request in route { ... }`; cancelling the consuming task cancels the route.
///
/// Releasing or cancelling it retracts the route, as does closing an exact route's
/// broadcast; a `BroadcastProducer` alone does not keep it announced.
public final class Route: AsyncSequence, Sendable {
    /// The broadcast request emitted by this sequence.
    public typealias Element = BroadcastRequest

    let ffi: MoqRoute
    private let owned: BroadcastProducer?

    init(_ ffi: MoqRoute) {
        self.ffi = ffi
        // A prefix route owns no broadcast.
        owned = (try? ffi.broadcast()).map { BroadcastProducer($0) }
    }

    /// The broadcast this exact route owns, the same one on every read. Throws on a prefix
    /// route, which owns none.
    public var broadcast: BroadcastProducer {
        get throws {
            if let owned { return owned }
            return BroadcastProducer(try ffi.broadcast())
        }
    }

    /// The stream this route serves, minted at creation unless `instance` named one.
    public var instance: String { ffi.instanceId() }

    /// Make the route visible, or keep it so; throws `Closed` once its broadcast closed.
    public func announce() throws {
        try ffi.announce()
    }

    /// Hide the route, keeping its broadcast and instance: `announce()` brings the same stream back.
    public func withdraw() throws {
        try ffi.withdraw()
    }

    /// Re-price the route in place: lower wins among routes covering the same prefix.
    public func setCost(_ cost: UInt64) throws {
        try ffi.setCost(cost: cost)
    }

    /// The next broadcast requested beneath this prefix route, or `nil` once the origin
    /// closes, and at once on an exact route.
    public func requested() async throws -> BroadcastRequest? {
        try await ffi.requested().map { BroadcastRequest($0) }
    }

    /// Retract the route now: pending requests are rejected and later calls throw
    /// `Cancelled`. An exact route's broadcast lives on until closed.
    public func cancel() {
        ffi.cancel()
    }

    /// Create an iterator that cancels the route when iteration is cancelled.
    public func makeAsyncIterator() -> AsyncThrowingStream<BroadcastRequest, Swift.Error>.Iterator {
        moqStream(cancel: { [ffi] in ffi.cancel() }) { [ffi] in
            try await ffi.requested().map { BroadcastRequest($0) }
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

    /// Follow an exact path, resolving each stream that serves it.
    /// Pass an `instance` to follow only that stream.
    public func follow(path: String, instance: String? = nil) throws -> Following {
        Following(try ffi.follow(path: path, instance: instance))
    }

    /// Request a broadcast by path, resolving one stream as soon as it can be
    /// served through the best announced route covering it: an announced broadcast on this
    /// origin, a route a session announced (served on demand by that session), or a dynamic
    /// handler on the origin, or an error if nothing can serve it. Unlike `follow`, this
    /// does not wait for a future announcement. Pass an `instance` to refuse any other stream.
    public func request(path: String, instance: String? = nil) async throws -> BroadcastConsumer {
        BroadcastConsumer(try await ffi.request(path: path, instance: instance))
    }
}

/// A stream of announce events. Iterate directly:
/// `for try await event in announced { ... }`.
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

/// Follows a specific broadcast path across the publisher instances that serve it.
public final class Following: Sendable {
    let ffi: MoqFollowing

    init(_ ffi: MoqFollowing) {
        self.ffi = ffi
    }

    /// Suspend until the next publisher instance serves the path, then resolve it.
    /// The first call resolves the instance serving the path once one is announced.
    /// Each later call waits for another to take over (a restarted publisher, or a
    /// replacement session) or for the broadcast to return after it went offline.
    /// Drop what was read from the previous instance then: its tracks end when its
    /// session does. Throws `Closed` if cancelled or the origin closes first.
    public func available() async throws -> BroadcastConsumer {
        BroadcastConsumer(try await ffi.available())
    }

    /// Cancel the pending `available()` call.
    public func cancel() {
        ffi.cancel()
    }
}
