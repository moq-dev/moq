package dev.moq

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.flow
import kotlinx.coroutines.flow.onCompletion
import uniffi.moq.MoqException

/**
 * Stream of catalog updates. Terminates when the underlying track ends.
 *
 * The Flow's [onCompletion] forwards Kotlin coroutine cancellation to the
 * native consumer's `cancel()` so structured concurrency propagates through
 * to the QUIC stream.
 */
fun CatalogConsumer.updates(): Flow<Catalog> = flow {
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
suspend fun BroadcastConsumer.catalog(): Catalog {
    val consumer = subscribeCatalog()
    try {
        return consumer.next() ?: throw MoqException.Closed()
    } finally {
        consumer.cancel()
    }
}

/** Stream of decoded media frames in decode order. */
fun MediaConsumer.frames(): Flow<MediaFrame> = flow {
    while (true) {
        currentCoroutineContext().ensureActive()
        emit(next() ?: break)
    }
}.onCompletion { cause ->
    if (cause is CancellationException) cancel()
}

/** Stream of decoded frames from one finite, fetched media group. */
fun MediaGroupConsumer.frames(): Flow<MediaFrame> = flow {
    while (true) {
        currentCoroutineContext().ensureActive()
        emit(next() ?: break)
    }
}.onCompletion { cause ->
    if (cause is CancellationException) cancel()
}

/**
 * Stream of decoded audio frames in the layout declared by the
 * [AudioDecoderOutput] the consumer was created with.
 */
fun AudioConsumer.frames(): Flow<AudioFrame> = flow {
    while (true) {
        currentCoroutineContext().ensureActive()
        emit(next() ?: break)
    }
}.onCompletion { cause ->
    if (cause is CancellationException) cancel()
}

/**
 * Stream of decoded video frames. Each owns the decoder's surface: close it
 * (or `use` it) when done, since held frames stall the decoder.
 */
fun VideoConsumer.frames(): Flow<VideoDecodedFrame> = flow {
    while (true) {
        currentCoroutineContext().ensureActive()
        emit(next() ?: break)
    }
}.onCompletion { cause ->
    if (cause is CancellationException) cancel()
}

/**
 * Stream of JSON values (as strings) from a snapshot track, yielding the latest reconstructed
 * value. A consumer that has fallen behind collapses the backlog to the latest.
 */
fun JsonSnapshotConsumer.values(): Flow<String> = flow {
    while (true) {
        currentCoroutineContext().ensureActive()
        emit(next() ?: break)
    }
}.onCompletion { cause ->
    if (cause is CancellationException) cancel()
}

/** Stream of JSON records (as strings) from a stream track, in order. */
fun JsonStreamConsumer.values(): Flow<String> = flow {
    while (true) {
        currentCoroutineContext().ensureActive()
        emit(next() ?: break)
    }
}.onCompletion { cause ->
    if (cause is CancellationException) cancel()
}

/** Stream of groups in sequence order, skipping forward if the reader falls behind. */
fun TrackConsumer.groups(): Flow<GroupConsumer> = flow {
    while (true) {
        currentCoroutineContext().ensureActive()
        emit(nextGroup() ?: break)
    }
}.onCompletion { cause ->
    if (cause is CancellationException) cancel()
}

/** Stream of groups in arrival order, including out-of-sequence deliveries. */
fun TrackConsumer.groupsAsArrived(): Flow<GroupConsumer> = flow {
    while (true) {
        currentCoroutineContext().ensureActive()
        emit(recvGroup() ?: break)
    }
}.onCompletion { cause ->
    if (cause is CancellationException) cancel()
}

/**
 * Stream of timestamped raw frames from one-frame-per-group tracks.
 *
 * Completed empty groups are skipped. The flow ends when the track ends.
 */
fun TrackConsumer.frames(): Flow<Frame> = flow {
    while (true) {
        currentCoroutineContext().ensureActive()
        emit(readFrame() ?: break)
    }
}.onCompletion { cause ->
    if (cause is CancellationException) cancel()
}

/** Stream of best-effort datagrams in arrival order. */
fun TrackConsumer.datagrams(): Flow<Datagram> = flow {
    while (true) {
        currentCoroutineContext().ensureActive()
        emit(recvDatagram() ?: break)
    }
}.onCompletion { cause ->
    if (cause is CancellationException) cancel()
}


/** Stream of tracks requested by subscribers. */
fun BroadcastDynamic.requestedTracks(): Flow<TrackRequest> = flow {
    while (true) {
        currentCoroutineContext().ensureActive()
        emit(requestedTrack())
    }
}.onCompletion { cause ->
    if (cause is CancellationException) cancel()
}

/** Stream of uncached group requests for one track. */
fun TrackDynamic.requestedGroups(): Flow<GroupRequest> = flow {
    while (true) {
        currentCoroutineContext().ensureActive()
        emit(requestedGroup())
    }
}.onCompletion { cause ->
    if (cause is CancellationException) cancel()
}

/** Stream of broadcasts requested by consumers. */
fun OriginDynamic.requestedBroadcasts(): Flow<BroadcastRequest> = flow {
    while (true) {
        currentCoroutineContext().ensureActive()
        emit(requestedBroadcast())
    }
}.onCompletion { cause ->
    if (cause is CancellationException) cancel()
}

/** Stream of timestamped raw frames within a group. */
fun GroupConsumer.frames(): Flow<Frame> = flow {
    while (true) {
        currentCoroutineContext().ensureActive()
        emit(readFrame() ?: break)
    }
}.onCompletion { cause ->
    if (cause is CancellationException) cancel()
}

/**
 * Stream of announce events matching [config]. An [AnnounceEventLive] follows the
 * routes live at subscribe time, so a collector can gather what is live and stop.
 *
 * Acquires the subscription on first collection and cancels it when collection
 * ends, so callers never touch the underlying handle. Use the raw
 * `announced(config)` if you need to hold and cancel the handle yourself.
 */
fun OriginConsumer.announcements(config: AnnounceConfig = AnnounceConfig()): Flow<AnnounceEvent> {
    val consumer = this
    return flow {
        val announced = consumer.announced(config)
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
