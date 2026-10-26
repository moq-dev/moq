import Foundation
import MoqFFI

/// What the peer lets this side do, in this side's own paths: the broadcast
/// patterns it may publish and subscribe to, and `expires` until it lapses.
/// An empty pattern list grants nothing in that direction; `**` grants everything.
public typealias Grant = MoqFFI.MoqGrant

/// Follows the grant a session's peer gave it. Iterate directly:
/// `for try await grant in session.grants() { ... }` yields the current grant,
/// then each change, `nil` while there is none.
///
/// Returned by `Session.grants()`. AUTH is only on `moq-lite-07-wip` and on
/// moq-transport sessions that negotiate the MoQ Auth extension; elsewhere the
/// grant stays `nil`. The sequence ends when the session does; cancelling the
/// consuming task cancels the consumer.
public final class GrantConsumer: AsyncSequence, Sendable {
    /// The grant emitted by this sequence, `nil` while there is none.
    public typealias Element = Grant?

    let ffi: MoqGrantConsumer

    init(_ ffi: MoqGrantConsumer) { self.ffi = ffi }

    /// The grant now, or `nil` until the peer first answers a token.
    /// A client session reads `nil` while it reconnects.
    public func grant() -> Grant? { ffi.grant() }

    /// Suspend until the grant differs from the one this last returned, or held when
    /// it was made, returning it.
    /// Throws once nothing will change it again: a client's connection or a server's
    /// session ended, or `cancel()`. A reconnect is not an end.
    public func changed() async throws -> Grant? { try await ffi.changed() }

    /// Cancel all current and future `changed()` calls.
    public func cancel() { ffi.cancel() }

    /// Create an iterator that cancels the native wait when iteration ends.
    public func makeAsyncIterator() -> AsyncThrowingStream<Grant?, Swift.Error>.Iterator {
        let first = First()
        return moqStream(cancel: { [ffi] in ffi.cancel() }) { [ffi] () async throws -> Grant?? in
            if first.take() { return .some(ffi.grant()) }
            do {
                return .some(try await ffi.changed())
            } catch let error as MoqError where error.isShutdown {
                return nil
            }
        }.makeAsyncIterator()
    }
}

/// Whether the current grant is still to be delivered, taken once.
private final class First: @unchecked Sendable {
    private let lock = NSLock()
    private var pending = true

    func take() -> Bool {
        lock.lock()
        defer { lock.unlock() }
        defer { pending = false }
        return pending
    }
}

/// The credentials a native client session presents, replaceable while it runs.
///
/// Returned by `Session.tokens()`, seeded from `Client(tokens:)`.
public final class Tokens: Sendable {
    let ffi: MoqTokens

    init(_ ffi: MoqTokens) {
        self.ffi = ffi
    }

    /// Replace the credentials every dial presents, such as before the current ones expire.
    ///
    /// The first is the connection credential, unless the URL carries its own `?jwt=`;
    /// each other one widens the grant over in-band AUTH. The next dial presents them,
    /// and a live session with in-band AUTH presents them at once, withdrawing each
    /// replaced token once its replacements are answered, even when one is refused.
    /// After an authorization refusal the session redials only if they changed.
    /// - Throws: `MoqError.Config` for an empty token.
    public func set(_ tokens: [String]) throws {
        try ffi.set(tokens: tokens)
    }

    /// Suspend until the peer answers the tokens set last: return once a live session
    /// holds every one, in band or as the connection credential it dialed with.
    ///
    /// While offline it waits for the next session. Throws the peer's refusal as
    /// `MoqError.Protocol` carrying its code, `MoqError.Unsupported` when the live session
    /// cannot carry an in-band token, or the session's close reason. The native wait ends
    /// with that answer, not with the calling task.
    public func answered() async throws {
        try await ffi.answered()
    }
}
