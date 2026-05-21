package dev.moq

import uniffi.moq.MoqException

/**
 * True for [MoqException.Cancelled] and [MoqException.Closed], which arise
 * from graceful shutdown rather than actual failures. Useful for swallowing
 * the expected exception that a Flow produces when its consumer cancels.
 */
val MoqException.isShutdown: Boolean
    get() = this is MoqException.Cancelled || this is MoqException.Closed
