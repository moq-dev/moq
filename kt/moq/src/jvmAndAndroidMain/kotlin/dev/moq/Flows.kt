package dev.moq

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.flow
import kotlinx.coroutines.flow.onCompletion
import uniffi.moq.MoqException

/**
 * Stream of reads: decoded audio frames in the layout declared by the
 * [AudioDecoderOutput] the consumer was created with, and the gaps and encoder
 * ends between them.
 */
fun dev.moq.audio.Consumer.reads(): Flow<dev.moq.audio.Read> = pull({ cancel() }) { next() }

/**
 * Stream of reads: decoded video frames, and the gaps and encoder ends between
 * them. Each frame owns the decoder's surface: close it (or `use` it) when done,
 * since held frames stall the decoder.
 */
fun dev.moq.video.Consumer.reads(): Flow<dev.moq.video.Read> = pull({ cancel() }) { next() }


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
fun BroadcastRequests.requestedTracks(): Flow<TrackRequest> = pull({ cancel() }) { requestedTrack() }

/** Stream of the fills readers of one track need. */
fun TrackRequests.requestedFills(): Flow<Fill> = pull({ cancel() }) { requestedFill() }

/** Stream of broadcasts requested by consumers. */
fun OriginRequests.requestedBroadcasts(): Flow<BroadcastRequest> = pull({ cancel() }) { requestedBroadcast() }

/** Stream of timestamped raw frames within a group. */
fun GroupConsumer.frames(): Flow<Frame> = pull({ cancel() }) { readFrame() }

/**
 * Stream of announce events, as `announced(config).updates()`.
 *
 * Collect it once: the handle is cancelled when collection ends, however it ends.
 */
fun AnnounceConsumer.updates(): Flow<AnnounceEvent> = pull { next() }.onCompletion { cancel() }

/**
 * The session's grant, as `grants().updates()`: the current one, then each change, null
 * while there is none.
 *
 * Collect it once: the consumer is cancelled when collection ends, however it ends.
 * Completes when the session shuts down, and throws its close reason otherwise.
 */
fun GrantConsumer.updates(): Flow<Grant?> = flow {
    emit(grant())
    while (true) {
        currentCoroutineContext().ensureActive()
        val grant = try {
            changed()
        } catch (e: MoqException) {
            if (e.isShutdown) break
            throw e
        }
        emit(grant)
    }
}.onCompletion { cancel() }

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
