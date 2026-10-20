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
fun dev.moq.audio.Consumer.frames(): Flow<dev.moq.audio.Frame> = pull({ cancel() }) { next() }

/**
 * Stream of decoded video frames. Each owns the decoder's surface: close it
 * (or `use` it) when done, since held frames stall the decoder.
 */
fun dev.moq.video.Consumer.frames(): Flow<VideoDecodedFrame> = pull({ cancel() }) { next() }

/** Stream of groups in sequence order, skipping forward if the reader falls behind. */
fun TrackConsumer.groups(): Flow<GroupConsumer> = pull({ cancel() }) { nextGroup() }

/** Stream of groups in arrival order, including out-of-sequence deliveries. */
fun TrackConsumer.groupsAsArrived(): Flow<GroupConsumer> = pull({ cancel() }) { recvGroup() }

/**
 * Stream of timestamped raw frames from one-frame-per-group tracks.
 *
 * Completed empty groups are skipped. The flow ends when the track ends.
 */
fun TrackConsumer.frames(): Flow<Frame> = pull({ cancel() }) { readFrame() }

/** Stream of best-effort datagrams in arrival order. */
fun TrackConsumer.datagrams(): Flow<Datagram> = pull({ cancel() }) { recvDatagram() }

/** Stream of tracks requested by subscribers. */
fun BroadcastDynamic.requestedTracks(): Flow<TrackRequest> = pull({ cancel() }) { requestedTrack() }

/** Stream of uncached group requests for one track. */
fun TrackDynamic.requestedGroups(): Flow<GroupRequest> = pull({ cancel() }) { requestedGroup() }

/** Stream of broadcasts requested by consumers. */
fun OriginDynamic.requestedBroadcasts(): Flow<BroadcastRequest> = pull({ cancel() }) { requestedBroadcast() }

/** Stream of timestamped raw frames within a group. */
fun GroupConsumer.frames(): Flow<Frame> = pull({ cancel() }) { readFrame() }

/**
 * Stream of announce events, as `announced(config).updates()`.
 *
 * Collect it once: the handle is cancelled when collection ends, however it ends.
 */
fun AnnounceConsumer.updates(): Flow<AnnounceEvent> = pull { next() }.onCompletion { cancel() }

/** Emits [next] until it returns null, checking for cancellation before each pull. */
private fun <T : Any> pull(next: suspend () -> T?): Flow<T> = flow {
    while (true) {
        currentCoroutineContext().ensureActive()
        emit(next() ?: break)
    }
}

/**
 * [pull], calling [cancel] when the collector is cancelled so the native
 * handle wakes a pending [next] and releases the stream.
 */
internal fun <T : Any> pull(cancel: () -> Unit, next: suspend () -> T?): Flow<T> =
    pull(next).onCompletion { cause -> if (cause is CancellationException) cancel() }
