package dev.moq

import uniffi.moq.MoqException

/**
 * True for [MoqException.Cancelled] and [MoqException.Closed], which arise
 * from graceful shutdown rather than actual failures. Useful for swallowing
 * the expected exception that a Flow produces when its consumer cancels.
 */
val MoqException.isShutdown: Boolean
    get() = this is MoqException.Cancelled || this is MoqException.Closed

/**
 * True for [MoqException.Unauthorized] (HTTP 401) and [MoqException.Forbidden]
 * (HTTP 403), which the server returns to reject the connection on
 * authentication or authorization grounds. Unlike a transport failure, retrying
 * without new credentials won't help, so callers should surface these rather
 * than reconnect.
 */
val MoqException.isAuth: Boolean
    get() = this is MoqException.Unauthorized || this is MoqException.Forbidden
