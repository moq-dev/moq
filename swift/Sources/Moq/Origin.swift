import MoqFFI

/// The publish side of an origin: create routes that own broadcasts, or claims that serve
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

    /// Claim `prefix` and serve the requests beneath it.
    ///
    /// A claim covers `prefix` and every path beneath it (`""` claims every path).
    /// A service that only serves some of them rejects the rest as they are
    /// requested. Iterate the claim, or call `requested()`, to answer each request; hold
    /// it while it should stay advertised.
    public func claim(
        prefix: String, hidden: Bool = false, cost: UInt64 = 0, instance: String? = nil, epoch: UInt64 = 0
    ) throws -> Claim {
        Claim(
            try ffi.claim(
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
    /// Unless `instance` names one, it takes the instance its claim lends when it first
    /// announces or `Route.instance` first reads it, so readers that resolved the path through
    /// the claim continue on it; its end then moves the claim to a fresh instance, so a
    /// restarted job is a new stream. A claim that names its instance lends none, and the route
    /// mints its own.
    /// Created visible, it announces now and answers the waiting requesters; created `hidden`, they wait until
    /// `Route.announce()`, so the job can fill its catalog first.
    public func route(
        hidden: Bool = false, cost: UInt64 = 0, instance: String? = nil, epoch: UInt64 = 0
    ) throws -> Route {
        Route(
            try ffi.route(options: MoqRouteOptions(hidden: hidden, cost: cost, instance: instance, epoch: epoch)))
    }

    /// Serve held content under the claim's continuous sequence space, without an exact route.
    public func serve(broadcast: BroadcastConsumer) throws {
        try ffi.serve(broadcast: broadcast.ffi)
    }

    /// Reject the request with an application error code.
    public func reject(errorCode: UInt16) throws {
        try ffi.reject(errorCode: errorCode)
    }
}

/// An exact route on an origin, owning one broadcast.
///
/// Releasing or cancelling it retracts the route, as does closing its broadcast; a
/// `BroadcastProducer` alone does not keep it announced.
public final class Route: Sendable {
    let ffi: MoqRoute

    /// The broadcast this route owns, the same one on every read.
    public let broadcast: BroadcastProducer

    init(_ ffi: MoqRoute) {
        self.ffi = ffi
        broadcast = BroadcastProducer(ffi.broadcast())
    }

    /// The stream this route serves now. A named instance never changes. A job's route takes
    /// its claim's current one when it first announces or when this first reads it. Read
    /// first, the job announces under it, and `announce()` throws `Unroutable` once the claim
    /// has moved past it, so a signer built from it stays valid.
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

    /// Retract the route now: later calls throw `Cancelled`. Its broadcast lives on until
    /// closed.
    public func cancel() {
        ffi.cancel()
    }
}

/// A prefix route on an origin: a claim that every path beneath a prefix can be served,
/// yielding each request as it arrives. Iterate it directly:
/// `for try await request in claim { ... }`; cancelling the consuming task cancels the claim.
///
/// Releasing or cancelling it retracts the claim and rejects the requests still waiting.
public final class Claim: AsyncSequence, Sendable {
    /// The broadcast request emitted by this sequence.
    public typealias Element = BroadcastRequest

    let ffi: MoqClaim

    init(_ ffi: MoqClaim) {
        self.ffi = ffi
    }

    /// The stream this claim serves now. A named instance never changes. Unnamed, the claim
    /// moves to a fresh one each time a job holding its current one ends.
    public var instance: String { ffi.instanceId() }

    /// Make the claim visible, or keep it so.
    public func announce() throws {
        try ffi.announce()
    }

    /// Hide the claim, keeping its instance: `announce()` brings it back. Requests beneath it
    /// are rejected meanwhile.
    public func withdraw() throws {
        try ffi.withdraw()
    }

    /// Re-price the claim in place: lower wins among routes covering the same prefix.
    public func setCost(_ cost: UInt64) throws {
        try ffi.setCost(cost: cost)
    }

    /// The next broadcast requested beneath this claim, or `nil` once the claim is retracted
    /// or the origin closes.
    public func requested() async throws -> BroadcastRequest? {
        try await ffi.requested().map { BroadcastRequest($0) }
    }

    /// Retract the claim now: pending requests are rejected and later calls throw
    /// `Cancelled`.
    public func cancel() {
        ffi.cancel()
    }

    /// Create an iterator that cancels the claim when iteration is cancelled.
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

    /// List the paths under a literal prefix matching an optional pattern filter, as each
    /// becomes present or absent. A new publisher instance keeps its path present, so use
    /// `follow` to see it. Paths with a segment starting with `.` below the prefix are left
    /// out unless `hidden`.
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

/// A listing of paths as each becomes present or absent. Iterate directly:
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

    /// Suspend until the publisher instance serving the path changes.
    /// The first call resolves the instance serving the path once one is announced, and
    /// never returns nil. Each later call returns another instance taking over (a
    /// restarted publisher, or a replacement session), or nil once the path goes offline
    /// (or, with an `instance`, another one takes it over), then whatever serves it next.
    /// Drop what was read from the previous instance then: its tracks end when its
    /// session does. Throws `Closed` if cancelled or the origin closes first.
    public func available() async throws -> BroadcastConsumer? {
        (try await ffi.available()).map(BroadcastConsumer.init)
    }

    /// Cancel all current and future `available()` calls.
    public func cancel() {
        ffi.cancel()
    }
}
