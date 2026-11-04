import MoqFFI

/// Point-to-point sessions over hidden endpoint broadcasts.
public enum Socket {}

extension Socket {
    /// Announces one endpoint and dials or accepts independent TCP and track sessions.
    public final class Endpoint: Sendable {
        let ffi: MoqSocketEndpoint

        /// Announce `id` beneath `prefix`, using `origin` to find peers.
        public init(origin: OriginProducer, prefix: String, id: String) throws {
            ffi = try MoqSocketEndpoint(origin: origin.ffi, prefix: prefix, id: id)
        }

        /// This endpoint's id.
        public var id: String { ffi.id() }
        /// The shared endpoint prefix.
        public var prefix: String { ffi.prefix() }
        /// Retract the endpoint and reset its pending dials and sessions.
        public func close() { ffi.close() }

        /// Dial a new tcp session; cancelling resets this dial.
        public func dialTcp(peer: String) async throws -> TcpStream {
            TcpStream(try await ffi.dialTcp(peer: peer))
        }
        /// Await the next tcp dial, independently of the other protocol.
        public func acceptTcp() async throws -> IncomingTcp {
            IncomingTcp(try await ffi.acceptTcp())
        }

        /// Dial a new track session; cancelling resets this dial.
        public func dialTrack(peer: String, info: TrackInfo? = nil) async throws -> TrackStream {
            TrackStream(try await ffi.dialTrack(peer: peer, info: info))
        }
        /// Await the next track dial, independently of the other protocol.
        public func acceptTrack() async throws -> IncomingTrack {
            IncomingTrack(try await ffi.acceptTrack())
        }
    }

    /// An incoming tcp dial, consumed by accepting or rejecting it.
    public final class IncomingTcp: Sendable {
        let ffi: MoqSocketIncomingTcp
        init(_ ffi: MoqSocketIncomingTcp) { self.ffi = ffi }
        /// The claimed peer; accepting verifies return routability.
        public var peer: String { ffi.peer() }
        /// The private session id.
        public var session: String { ffi.session() }
        /// Verify return routability and accept the dial.
        public func accept() async throws -> TcpStream {
            TcpStream(try await ffi.accept())
        }
        /// Refuse this dial as not found.
        public func reject() throws { try ffi.reject() }
    }

    /// A tcp session's two halves and identities.
    public final class TcpStream: Sendable {
        /// This side's write half.
        public let producer: GroupProducer
        /// The peer's read half, retaining its subscription.
        public let consumer: GroupConsumer
        /// The peer endpoint's id.
        public let peer: String
        /// The private session id.
        public let session: String
        init(_ ffi: MoqSocketTcpStream) {
            producer = GroupProducer(ffi.producer())
            consumer = GroupConsumer(ffi.consumer())
            peer = ffi.peer()
            session = ffi.session()
        }
    }

    /// An incoming track dial, consumed by accepting or rejecting it.
    public final class IncomingTrack: Sendable {
        let ffi: MoqSocketIncomingTrack
        init(_ ffi: MoqSocketIncomingTrack) { self.ffi = ffi }
        /// The claimed peer; accepting verifies return routability.
        public var peer: String { ffi.peer() }
        /// The private session id.
        public var session: String { ffi.session() }
        /// Verify return routability and accept the dial.
        public func accept(info: TrackInfo? = nil) async throws -> TrackStream {
            TrackStream(try await ffi.accept(info: info))
        }
        /// Refuse this dial as not found.
        public func reject() throws { try ffi.reject() }
    }

    /// A track session's two halves and identities.
    public final class TrackStream: Sendable {
        /// This side's write half.
        public let producer: TrackProducer
        /// The peer's read half, retaining its subscription.
        public let consumer: TrackConsumer
        /// The peer endpoint's id.
        public let peer: String
        /// The private session id.
        public let session: String
        init(_ ffi: MoqSocketTrackStream) {
            producer = TrackProducer(ffi.producer())
            consumer = TrackConsumer(ffi.consumer())
            peer = ffi.peer()
            session = ffi.session()
        }
    }
}
