import MoqFFI

/// What the peer lets this side do, in this side's own paths: the broadcast
/// patterns it may publish and subscribe to, and `expiresUs` until it lapses.
/// An empty pattern list grants nothing in that direction; `**` grants everything.
public typealias Grant = MoqFFI.MoqGrant

/// A session's authorization: the grant its peer gave it, and more tokens to present.
///
/// Returned by `Session.auth()`. AUTH is only on `moq-lite-07-wip` and on
/// moq-transport sessions that negotiate the MoQ Auth extension; elsewhere the
/// grant stays `nil`.
public final class Auth: Sendable {
    let ffi: MoqAuth

    init(_ ffi: MoqAuth) { self.ffi = ffi }

    /// The union of every token's grant, or `nil` until the peer first answers one.
    /// A client session reads `nil` while it reconnects.
    public func grant() -> Grant? { ffi.grant() }

    /// Suspend until the grant differs from the one last reported, or held when this
    /// handle was made, returning it.
    /// Throws once nothing will change it again: a client's connection or a server's
    /// session ended. A reconnect is not an end.
    public func grantChanged() async throws -> Grant? { try await ffi.grantChanged() }

    /// Present another token, returning once the peer accepts it.
    ///
    /// Its grant joins the union until `AuthToken.cancel()`; a client session presents
    /// it again on every reconnect. The peer's refusal throws `MoqError.Protocol`
    /// carrying its code, and a session that cannot carry another token throws
    /// `MoqError.Unsupported`.
    public func add(token: String) async throws -> AuthToken {
        AuthToken(try await ffi.add(token: token))
    }
}

/// One token presented with `Auth.add(token:)`, withdrawn by `cancel()`.
public final class AuthToken: Sendable {
    let ffi: MoqAuthToken

    init(_ ffi: MoqAuthToken) { self.ffi = ffi }

    /// This token's own grant, or `nil` after it ended.
    public func grant() -> Grant? { ffi.grant() }

    /// Suspend until the token ends, throwing the peer's revocation or the end of a client's
    /// connection or a server's session. A reconnect does not end it.
    public func closed() async throws { try await ffi.closed() }

    /// Withdraw the token, removing its grant from the union.
    public func cancel() { ffi.cancel() }
}
