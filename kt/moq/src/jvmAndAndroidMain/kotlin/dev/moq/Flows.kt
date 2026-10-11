package dev.moq

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.flow
import kotlinx.coroutines.flow.onCompletion
import uniffi.moq.MoqException

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
 * Stream of announce events, as `announced(config).updates()`.
 *
 * Collect it once: the handle is cancelled when collection ends, however it ends.
 */
fun AnnounceConsumer.updates(): Flow<AnnounceEvent> = flow {
    while (true) {
        currentCoroutineContext().ensureActive()
        emit(next() ?: break)
    }
}.onCompletion { cancel() }
