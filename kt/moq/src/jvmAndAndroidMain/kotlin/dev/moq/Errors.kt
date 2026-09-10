package dev.moq

import uniffi.moq.MoqException
import uniffi.moq.MoqProtocolError
import uniffi.moq.MoqProtocolKind

/**
 * True for [MoqException.Cancelled] and [MoqException.Closed], which arise
 * from graceful shutdown rather than actual failures. Useful for swallowing
 * the expected exception that a Flow produces when its consumer cancels.
 */
val MoqException.isShutdown: Boolean
    get() = this is MoqException.Cancelled || this is MoqException.Closed

/**
 * True for HTTP 401/403 and a protocol Unauthorized session close. Unlike a
 * transport failure, retrying without new credentials won't help, so callers
 * should surface these rather than reconnect.
 */
val MoqException.isAuth: Boolean
    get() {
        if (this is MoqException.Unauthorized || this is MoqException.Forbidden) return true
        val protocol = protocolError ?: return false
        return protocol.kind == MoqProtocolKind.UNAUTHORIZED
    }

/**
 * The structured protocol failure, or null if this is not one.
 *
 * Carries the peer's session or stream scope, the verbatim wire code, a known
 * kind when recognized, and a diagnostic message.
 */
val MoqException.protocolError: MoqProtocolError?
    get() = (this as? MoqException.Protocol)?.details
