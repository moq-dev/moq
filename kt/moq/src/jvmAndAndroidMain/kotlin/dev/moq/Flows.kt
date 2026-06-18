package dev.moq

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.flow
import kotlinx.coroutines.flow.onCompletion
import uniffi.moq.MoqAnnouncement
import uniffi.moq.MoqAudioConsumer
import uniffi.moq.MoqAudioFrame
import uniffi.moq.MoqBroadcastConsumer
import uniffi.moq.MoqBroadcastDynamic
import uniffi.moq.MoqCatalog
import uniffi.moq.MoqCatalogConsumer
import uniffi.moq.MoqException
import uniffi.moq.MoqFrame
import uniffi.moq.MoqGroupConsumer
import uniffi.moq.MoqMediaConsumer
import uniffi.moq.MoqOriginConsumer
import uniffi.moq.MoqTrackConsumer
import uniffi.moq.MoqTrackRequest

/**
 * Stream of catalog updates. Terminates when the underlying track ends.
 *
 * The Flow's [onCompletion] forwards Kotlin coroutine cancellation to the
 * native consumer's `cancel()` so structured concurrency propagates through
 * to the QUIC stream.
 */
fun MoqCatalogConsumer.updates(): Flow<MoqCatalog> = flow {
    while (true) {
        currentCoroutineContext().ensureActive()
        emit(next() ?: break)
    }
}.onCompletion { cause ->
    if (cause is CancellationException) cancel()
}

/**
 * Subscribe to the catalog track and return the first catalog, cancelling the
 * subscription before returning. Convenience for callers that only need the
 * current catalog rather than a stream of updates (use [updates] for that).
 */
suspend fun MoqBroadcastConsumer.catalog(): MoqCatalog {
    val consumer = subscribeCatalog()
    try {
        return consumer.next() ?: throw MoqException.Closed("broadcast closed before a catalog was published")
    } finally {
        consumer.cancel()
    }
}

/** Stream of decoded media frames in decode order. */
fun MoqMediaConsumer.frames(): Flow<MoqFrame> = flow {
    while (true) {
        currentCoroutineContext().ensureActive()
        emit(next() ?: break)
    }
}.onCompletion { cause ->
    if (cause is CancellationException) cancel()
}

/**
 * Stream of decoded audio frames in the layout declared by the
 * `MoqAudioDecoderConfig` the consumer was created with.
 */
fun MoqAudioConsumer.frames(): Flow<MoqAudioFrame> = flow {
    while (true) {
        currentCoroutineContext().ensureActive()
        emit(next() ?: break)
    }
}.onCompletion { cause ->
    if (cause is CancellationException) cancel()
}

/** Stream of groups in sequence order, skipping forward if the reader falls behind. */
fun MoqTrackConsumer.groups(): Flow<MoqGroupConsumer> = flow {
    while (true) {
        currentCoroutineContext().ensureActive()
        emit(nextGroup() ?: break)
    }
}.onCompletion { cause ->
    if (cause is CancellationException) cancel()
}

/** Stream of groups in arrival order, including out-of-sequence deliveries. */
fun MoqTrackConsumer.groupsAsArrived(): Flow<MoqGroupConsumer> = flow {
    while (true) {
        currentCoroutineContext().ensureActive()
        emit(recvGroup() ?: break)
    }
}.onCompletion { cause ->
    if (cause is CancellationException) cancel()
}

/** Stream of tracks requested by subscribers. */
fun MoqBroadcastDynamic.requestedTracks(): Flow<MoqTrackRequest> = flow {
    while (true) {
        currentCoroutineContext().ensureActive()
        emit(requestedTrack())
    }
}.onCompletion { cause ->
    if (cause is CancellationException) cancel()
}

/** Stream of raw frame payloads within a group. */
fun MoqGroupConsumer.frames(): Flow<ByteArray> = flow {
    while (true) {
        currentCoroutineContext().ensureActive()
        emit(readFrame() ?: break)
    }
}.onCompletion { cause ->
    if (cause is CancellationException) cancel()
}

/**
 * Stream of broadcast announcements under a prefix.
 *
 * Acquires the subscription on first collection and cancels it when collection
 * ends, so callers never touch the underlying handle. Use the raw
 * `announced(prefix)` if you need to hold and cancel the handle yourself.
 */
fun MoqOriginConsumer.announcements(prefix: String): Flow<MoqAnnouncement> {
    val consumer = this
    return flow {
        val announced = consumer.announced(prefix)
        try {
            while (true) {
                currentCoroutineContext().ensureActive()
                emit(announced.next() ?: break)
            }
        } finally {
            announced.cancel()
        }
    }
}
